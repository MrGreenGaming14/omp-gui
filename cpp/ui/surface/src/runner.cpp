#include "loa/ui/surface/runner.hpp"

#include <atomic>
#include <deque>
#include <mutex>

#include <asio.hpp>

#include "loa/contracts/surface_channel.hpp"
#include "loa/runtime/executor.hpp"

namespace loa::ui::surface {

using Awaitable = asio::awaitable<void, asio::any_io_executor>;

struct Runner::Shared {
    SurfacePtr surface;
    asio::strand<asio::any_io_executor> strand;
    Post post;
    Deliver deliver;
    std::atomic<bool> alive{true};
    asio::cancellation_signal cancel; // touched on the strand only

    std::mutex lock;
    std::deque<InputEvent> inputs;
    bool sending = false;

    Shared(SurfacePtr s, asio::any_io_executor ex, Post p, Deliver d)
        : surface(std::move(s)), strand(asio::make_strand(std::move(ex))), post(std::move(p)), deliver(std::move(d)) {}

    // Hands `m` to the GUI thread, dropped once the runner is gone.
    static void send(const std::shared_ptr<Shared>& self, Message m) {
        if (!self->alive) return;
        self->post([self, m = std::move(m)]() mutable {
            if (self->alive) self->deliver(std::move(m));
        });
    }
};

namespace {

using SharedPtr = std::shared_ptr<Runner::Shared>;

Awaitable stream_frames(SharedPtr s, std::shared_ptr<FrameReceiver> frames) {
    while (s->alive) {
        auto frame = co_await frames->recv();
        if (!s->alive) co_return;
        if (auto lagged = frames->take_lagged(); lagged > 0) Runner::Shared::send(s, msg::Lagged{lagged});
        if (!frame) {
            Runner::Shared::send(s, msg::StreamEnded{});
            co_return;
        }
        Runner::Shared::send(s, msg::Frame{std::move(*frame)});
    }
}

// Sends queued inputs one at a time, in order; reports only failures.
Awaitable deliver_inputs(SharedPtr s) {
    while (s->alive) {
        InputEvent event;
        {
            std::scoped_lock g(s->lock);
            if (s->inputs.empty()) {
                s->sending = false;
                co_return;
            }
            event = std::move(s->inputs.front());
            s->inputs.pop_front();
        }
        auto sent = co_await s->surface->input(std::move(event));
        if (!sent) Runner::Shared::send(s, msg::Failed{sent.error().message()});
    }
    std::scoped_lock g(s->lock);
    s->sending = false;
}

// A surface call reported as `Done` or `Failed`.
template <typename Call> Awaitable call(SharedPtr s, Call c) {
    auto r = co_await c();
    if (r) {
        Runner::Shared::send(s, msg::Done{});
    } else {
        Runner::Shared::send(s, msg::Failed{r.error().message()});
    }
}

Awaitable fetch_document(SharedPtr s) {
    auto doc = co_await s->surface->document();
    msg::Document m;
    if (doc) {
        m.document = std::make_shared<const Document>(std::move(*doc));
    } else {
        m.document = std::unexpected(doc.error().message());
    }
    Runner::Shared::send(s, std::move(m));
}

} // namespace

Runner::Runner(SurfacePtr surface, loa::runtime::Executor& executor, Post post, Deliver deliver)
    : shared_(std::make_shared<Shared>(std::move(surface), executor.context().get_executor(), std::move(post), std::move(deliver))) {}

Runner::~Runner() {
    shared_->alive = false;
    asio::post(shared_->strand, [s = shared_] { s->cancel.emit(asio::cancellation_type::all); });
}

void Runner::run(Task task) {
    const auto& s = shared_;
    for (auto& w : task.work) {
        if (auto* f = std::get_if<work::StreamFrames>(&w)) {
            if (!f->receiver) continue;
            asio::co_spawn(s->strand, stream_frames(s, std::move(f->receiver)), asio::bind_cancellation_slot(s->cancel.slot(), asio::detached));
        } else if (auto* send = std::get_if<work::Send>(&w)) {
            bool start = false;
            {
                std::scoped_lock g(s->lock);
                s->inputs.push_back(std::move(send->event));
                start = !std::exchange(s->sending, true);
            }
            if (start) asio::co_spawn(s->strand, deliver_inputs(s), asio::detached);
        } else if (std::holds_alternative<work::RequestKeyframe>(w)) {
            asio::co_spawn(s->strand, call(s, [s] { return s->surface->request_keyframe(); }), asio::detached);
        } else if (auto* n = std::get_if<work::Negotiate>(&w)) {
            asio::co_spawn(s->strand, call(s, [s, accepted = std::move(n->accepted)] { return s->surface->negotiate(accepted); }),
                           asio::detached);
        } else if (std::holds_alternative<work::FetchDocument>(w)) {
            asio::co_spawn(s->strand, fetch_document(s), asio::detached);
        }
    }
}

} // namespace loa::ui::surface
