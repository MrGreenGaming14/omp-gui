#include "loa/projects/rebuild.hpp"

#include <asio/co_spawn.hpp>
#include <asio/detached.hpp>
#include <asio/io_context.hpp>
#include <asio/post.hpp>
#include <asio/redirect_error.hpp>
#include <asio/steady_timer.hpp>
#include <asio/strand.hpp>
#include <asio/use_awaitable.hpp>

#include "loa/core/schema.hpp"
#include "loa/projects/digest.hpp"
#include "loa/runtime/async_util.hpp"
#include "loa/runtime/log.hpp"

namespace loa::projects {

using settings::DigestState;

Decision decide(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return Decision::Empty;
    if (is_excluded(dom)) return Decision::Excluded;
    if (dom.children_of_kind(root->id, core::ElementKind::Turn).empty()) return Decision::Empty;
    if (digest_state(dom) == DigestState::Fresh) return Decision::UpToDate;
    return Decision::Rebuild;
}

void Outcomes::push(RebuildOutcome outcome) {
    {
        std::scoped_lock lock(mutex_);
        queue_.push_back(std::move(outcome));
    }
    ready_.notify_all();
}

void Outcomes::finish() {
    {
        std::scoped_lock lock(mutex_);
        finished_ = true;
    }
    ready_.notify_all();
}

std::optional<RebuildOutcome> Outcomes::recv(std::chrono::milliseconds timeout) {
    std::unique_lock lock(mutex_);
    ready_.wait_for(lock, timeout, [&] { return !queue_.empty() || finished_; });
    if (queue_.empty()) return std::nullopt;
    auto out = std::move(queue_.front());
    queue_.pop_front();
    return out;
}

std::optional<RebuildOutcome> Outcomes::try_recv() {
    std::scoped_lock lock(mutex_);
    if (queue_.empty()) return std::nullopt;
    auto out = std::move(queue_.front());
    queue_.pop_front();
    return out;
}

namespace {

using Clock = std::chrono::steady_clock;
using Strand = asio::strand<asio::io_context::executor_type>;

// What the driver coroutine and its senders share.
struct Shared : std::enable_shared_from_this<Shared> {
    Shared(Strand strand, Clock::duration debounce) : strand(std::move(strand)), timer(this->strand), due(debounce) {}

    Strand strand;
    // Touched only on the strand.
    asio::steady_timer timer;
    std::mutex mutex;
    runtime::KeyedDebouncer<std::string> due;
    bool open = true;
    bool stopped = false;

    // Wakes the driver to look at `due` and `open` again.
    void wake() {
        asio::post(strand, [self = shared_from_this()] { self->timer.cancel(); });
    }
};

struct Driver {
    RebuildDriver config;
    std::shared_ptr<Shared> shared;
    std::shared_ptr<Outcomes> out;

    asio::awaitable<RebuildOutcome> handle(std::string session) const {
        auto loaded = config.source->load(session);
        if (!loaded) co_return rebuild_outcome::Failed{session, loaded.error().to_string()};
        if (!*loaded) co_return rebuild_outcome::Failed{session, Error::session_not_found(session).to_string()};
        const auto dom = *loaded;
        if (auto decision = decide(*dom); decision != Decision::Rebuild) co_return rebuild_outcome::Skipped{session, decision};
        auto rebuilt = co_await rebuild(session, *dom);
        if (rebuilt) co_return rebuild_outcome::Rebuilt{session, *rebuilt};
        auto error = rebuilt.error().to_string();
        if (auto draft = mark_stale_draft(*dom)) {
            if (auto r = config.writer->commit(session, std::move(*draft)); !r)
                error += "; flagging the old digest stale failed: " + r.error().to_string();
        }
        runtime::log::warn("loa_projects", "digest rebuild failed: session=" + session + " error=" + error);
        co_return rebuild_outcome::Failed{session, std::move(error)};
    }

    asio::awaitable<Result<std::uint64_t>> rebuild(const std::string& session, const core::Dom& dom) const {
        std::optional<std::string> model;
        if (auto project = session_project(dom); config.settings && project) {
            auto settings = config.settings->load(*project);
            if (!settings) co_return std::unexpected(settings.error());
            model = settings->digest_model;
        }
        auto built = co_await config.digester.digest(dom, std::move(model));
        if (!built) co_return std::unexpected(built.error());
        auto drafts = write_drafts(dom, built->content, built->source_seq,
                                   built->model ? std::optional<std::string_view>(*built->model) : std::nullopt, core::now_ms());
        for (auto& draft : drafts)
            if (auto r = config.writer->commit(session, std::move(draft)); !r) co_return std::unexpected(r.error());
        co_return built->source_seq;
    }

    asio::awaitable<void> run() const {
        for (;;) {
            std::optional<Clock::time_point> wake;
            {
                std::scoped_lock lock(shared->mutex);
                if (!shared->open && shared->due.empty()) break;
                // Once the last sender is gone, pending sessions run right away.
                wake = shared->open ? shared->due.next() : std::optional(Clock::now());
            }
            shared->timer.expires_at(wake.value_or(Clock::time_point::max()));
            std::error_code ec;
            co_await shared->timer.async_wait(asio::redirect_error(asio::use_awaitable, ec));
            std::vector<std::string> ready;
            {
                std::scoped_lock lock(shared->mutex);
                ready = shared->due.take_due(Clock::now(), !shared->open);
            }
            for (auto& session : ready) out->push(co_await handle(std::move(session)));
        }
        {
            std::scoped_lock lock(shared->mutex);
            shared->stopped = true;
        }
        out->finish();
    }
};

} // namespace

struct RebuildSender::Handle {
    explicit Handle(std::shared_ptr<Shared> s) : shared(std::move(s)) {}
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
    std::shared_ptr<Shared> shared;

    ~Handle() {
        {
            std::scoped_lock lock(shared->mutex);
            shared->open = false;
        }
        shared->wake();
    }
};

bool RebuildSender::send(std::string session) const {
    {
        std::scoped_lock lock(handle_->shared->mutex);
        if (handle_->shared->stopped) return false;
        handle_->shared->due.touch(std::move(session));
    }
    handle_->shared->wake();
    return true;
}

std::pair<RebuildSender, std::shared_ptr<Outcomes>> RebuildDriver::spawn(asio::io_context& context) && {
    auto shared = std::make_shared<Shared>(asio::make_strand(context), debounce);
    auto out = std::make_shared<Outcomes>();
    auto driver = std::make_shared<Driver>(Driver{std::move(*this), shared, out});
    asio::co_spawn(shared->strand, [driver]() -> asio::awaitable<void> { co_await driver->run(); }, asio::detached);
    return {RebuildSender(std::make_shared<RebuildSender::Handle>(shared)), out};
}

} // namespace loa::projects
