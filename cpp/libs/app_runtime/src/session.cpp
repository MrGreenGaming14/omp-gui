#include "loa/app_runtime/session.hpp"

#include <algorithm>
#include <atomic>
#include <format>

#include <asio/awaitable.hpp>
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>

#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"
#include "loa/runtime/time.hpp"

#if defined(_WIN32)
#include <process.h>
#else
#include <unistd.h>
#endif

namespace loa::app_runtime {

namespace {

template <class... F>
struct Overloaded : F... {
    using F::operator()...;
};

bool dom_busy(const core::Dom& dom) {
    return std::ranges::any_of(dom_turns(dom), [](const core::Node* turn) {
        auto status = turn->str_attr(core::attr::STATUS);
        auto parsed = status ? core::parse<core::TurnStatus>(*status) : std::nullopt;
        return parsed == core::TurnStatus::Running || parsed == core::TurnStatus::Waiting;
    });
}

bool touches_turn_status(const core::Dom& dom, const core::Patch& patch) {
    return std::visit(Overloaded{
                          [](const core::op::Insert& insert) { return insert.kind == core::ElementKind::Turn; },
                          [&](const core::op::Set& set) {
                              const core::Node* node = dom.get(patch.node);
                              return set.name == core::attr::STATUS && node && node->kind == core::ElementKind::Turn;
                          },
                          [](const core::op::Remove&) { return true; },
                          [](const core::op::Append&) { return false; },
                      },
                      patch.op);
}

// The event `patch` (already applied to `dom`) stands for, besides itself.
std::optional<SessionEvent> derive(const SessionId& session, const core::Dom& dom, const core::Patch& patch) {
    const core::Node* node = dom.get(patch.node);
    if (!node) return std::nullopt;
    auto status = node->str_attr(core::attr::STATUS);
    const bool insert = std::holds_alternative<core::op::Insert>(patch.op);
    const auto* set = std::get_if<core::op::Set>(&patch.op);
    if (insert && node->kind == core::ElementKind::Turn) return session_event::TurnStarted{session, node->id};
    if (insert && node->kind == core::ElementKind::Compaction) return session_event::Compaction{session, node->id};
    if ((insert || set) && node->kind == core::ElementKind::Decision) {
        if (set && set->name != core::attr::STATUS) return std::nullopt;
        auto parsed = status ? core::parse<core::DecisionStatus>(*status) : std::nullopt;
        if (!parsed) return std::nullopt;
        switch (*parsed) {
        case core::DecisionStatus::Waiting: return session_event::DecisionWaiting{session, node->id};
        case core::DecisionStatus::Answered: return session_event::DecisionAnswered{session, node->id};
        case core::DecisionStatus::Expired:
        case core::DecisionStatus::Cancelled: return std::nullopt;
        }
    }
    if (set && node->kind == core::ElementKind::Turn && set->name == core::attr::STATUS) {
        auto parsed = status ? core::parse<core::TurnStatus>(*status) : std::nullopt;
        if (!parsed) return std::nullopt;
        switch (*parsed) {
        case core::TurnStatus::Done:
        case core::TurnStatus::Error:
        case core::TurnStatus::Cancelled: return session_event::TurnEnded{session, node->id, *parsed};
        case core::TurnStatus::Running:
        case core::TurnStatus::Waiting: return std::nullopt;
        }
    }
    return std::nullopt;
}

// The session holds turns, or continues another session.
bool keeps_history(const core::Dom& dom) {
    if (!dom_turns(dom).empty()) return true;
    const core::Node* root = dom.root();
    return root && root->attr(core::attr::PARENT_SESSION) != nullptr;
}

} // namespace

// ---- WriterFeed ------------------------------------------------------------

WriterFeed::WriterFeed(std::shared_ptr<bus::SessionWriter> writer, bus::Policy policy)
    : writer_(std::move(writer)), policy_(policy) {}
WriterFeed::~WriterFeed() = default;
WriterFeed::WriterFeed(WriterFeed&&) noexcept = default;
WriterFeed& WriterFeed::operator=(WriterFeed&&) noexcept = default;

contracts::Awaitable<std::optional<Feed>> WriterFeed::next() {
    for (;;) {
        if (ended_) co_return std::nullopt;
        if (!subscription_) {
            auto [dom, subscription] = writer_->subscribe(policy_);
            subscription_.emplace(std::move(subscription));
            co_return Feed(feed::Synced{std::make_shared<const core::Dom>(std::move(dom))});
        }
        std::optional<bus::Event> first = co_await subscription_->recv();
        if (!first) {
            ended_ = true;
            co_return Feed(feed::Ended{});
        }
        if (std::holds_alternative<bus::event::Lagged>(*first)) {
            subscription_.reset();
            continue;
        }
        // Appends that arrive during the frame merge in the bus queue.
        std::vector<bus::Event> batch;
        batch.push_back(std::move(*first));
        asio::steady_timer frame(co_await asio::this_coro::executor, FRAME);
        co_await frame.async_wait(asio::use_awaitable);
        for (;;) {
            auto event = subscription_->try_recv();
            // A closed writer ends the feed on the next call.
            if (!event) break;
            // The batch goes out first; the next call resubscribes from the writer's DOM.
            if (std::holds_alternative<bus::event::Lagged>(*event)) {
                subscription_.reset();
                break;
            }
            batch.push_back(std::move(*event));
        }
        co_return Feed(feed::Events{std::move(batch)});
    }
}

// ---- Session ---------------------------------------------------------------

Session::Session(SessionId id, session::Live live, core::Dom dom)
    : id_(std::move(id)), writer_(std::move(live.writer)), journal_(std::move(live.journal)), dom_(std::move(dom)) {
    busy_ = dom_busy(dom_);
    summary_ = contracts::SessionSummary::from_dom(dom_, false);
}

std::optional<std::string_view> Session::harness_id() const {
    if (harness_) return harness_->harness();
    const core::Node* root = dom_.root();
    if (!root) return std::nullopt;
    return root->str_attr(core::attr::HARNESS);
}

bool Session::has_turns() const { return !dom_turns(dom_).empty(); }

void Session::reload(core::Dom dom) {
    dom_ = std::move(dom);
    busy_ = dom_busy(dom_);
    summary_ = contracts::SessionSummary::from_dom(dom_, false);
}

void Session::attach(HarnessPtr harness) {
    harness_ = std::move(harness);
    reload(writer_->with_dom([](const core::Dom& dom) { return dom; }));
}

HarnessPtr Session::detach() { return std::exchange(harness_, nullptr); }

std::expected<PatchPtr, core::ApplyError> Session::apply(bus::Event event) {
    const std::uint64_t seq = dom_.seq();
    if (auto* patch = std::get_if<bus::event::Patch>(&event)) {
        if (patch->patch->seq <= seq) return nullptr;
        if (auto applied = dom_.apply(*patch->patch); !applied) return std::unexpected(applied.error());
        return patch->patch;
    }
    if (auto* appends = std::get_if<bus::Appends>(&event)) {
        if (appends->last_seq <= seq) return nullptr;
        auto patch = std::make_shared<core::Patch>(
            core::Patch{appends->first_seq, appends->node, appends->ts, core::op::Append{std::move(appends->text)}});
        if (auto applied = dom_.apply(*patch); !applied) return std::unexpected(applied.error());
        // The other merged patches carry no text of their own.
        for (std::uint64_t each = appends->first_seq + 1; each <= appends->last_seq; ++each) {
            core::Patch empty{each, patch->node, patch->ts, core::op::Append{}};
            if (auto applied = dom_.apply(empty); !applied) return std::unexpected(applied.error());
        }
        return PatchPtr(std::move(patch));
    }
    return nullptr;
}

// ---- Sessions --------------------------------------------------------------

const Session* Sessions::get(std::string_view id) const {
    auto it = open_.find(id);
    return it == open_.end() ? nullptr : &it->second;
}

Session* Sessions::get_mut(std::string_view id) {
    auto it = open_.find(id);
    return it == open_.end() ? nullptr : &it->second;
}

const Session* Sessions::active() const { return active_ ? get(active_->as_str()) : nullptr; }

Session* Sessions::active_mut() { return active_ ? get_mut(active_->as_str()) : nullptr; }

bool Sessions::is_active(std::string_view id) const { return active_ && active_->as_str() == id; }

std::expected<std::pair<SessionId, std::shared_ptr<bus::SessionWriter>>, std::string> Sessions::create(
    const session::Library& library) {
    std::string id = new_session_id();
    auto live = library.create(id);
    if (!live) return std::unexpected(std::format("Can't create session {}: {}", id, live.error().to_string()));
    auto writer = live->writer;
    SessionId session(std::move(id));
    open_.insert_or_assign(session, Session(session, std::move(*live), core::Dom()));
    return std::pair{session, std::move(writer)};
}

std::expected<void, std::string> Sessions::load(const session::Library& library, std::string_view id) {
    if (open_.contains(id)) return {};
    auto live = library.open(id);
    if (!live) return std::unexpected(std::format("Can't open session {}: {}", id, live.error().to_string()));
    core::Dom dom = live->writer->with_dom([](const core::Dom& d) { return d; });
    SessionId session(id);
    open_.insert_or_assign(session, Session(session, std::move(*live), std::move(dom)));
    return {};
}

std::pair<std::optional<Session>, std::optional<std::string>> Sessions::close(const session::Library& library,
                                                                              std::string_view id, bool delete_) {
    std::optional<Session> session;
    if (auto it = open_.find(id); it != open_.end()) {
        session.emplace(std::move(it->second));
        open_.erase(it);
    }
    if (is_active(id)) active_.reset();
    if (starting_ && starting_->as_str() == id) starting_.reset();
    std::optional<std::string> error;
    if (delete_) {
        if (auto removed = library.remove(id); !removed) error = removed.error().to_string();
    } else {
        if (auto closed = library.close(id); !closed) error = closed.error().to_string();
    }
    if (error) error = std::format("Can't close session {}: {}", id, *error);
    return {std::move(session), std::move(error)};
}

Change Sessions::sync(std::string_view id, std::shared_ptr<const core::Dom> dom) {
    Session* session = get_mut(id);
    if (!session) return change::None{};
    if (dom->seq() == session->dom_.seq()) return change::None{};
    session->reload(*dom);
    return change::Loaded{};
}

Change Sessions::apply(std::string_view id, bus::Event event) {
    Session* session = get_mut(id);
    if (!session) return change::None{};
    auto applied = session->apply(std::move(event));
    if (!applied) {
        // The copy fell out of step: take the writer's DOM.
        session->reload(session->writer_->with_dom([](const core::Dom& dom) { return dom; }));
        return change::Loaded{};
    }
    PatchPtr patch = std::move(*applied);
    if (!patch) return change::None{};
    const bool turn_changed = touches_turn_status(session->dom_, *patch);
    if (turn_changed) session->busy_ = dom_busy(session->dom_);
    bool summary_changed;
    if (session->summary_) {
        summary_changed = session->summary_->apply(session->dom_, *patch);
    } else {
        session->summary_ = contracts::SessionSummary::from_dom(session->dom_, false);
        summary_changed = session->summary_.has_value();
    }
    auto derived = derive(session->id_, session->dom_, *patch);
    return change::Patch{std::move(patch), std::move(derived), summary_changed, turn_changed};
}

// ---- free functions --------------------------------------------------------

Recents recents(const session::Library& library) {
    Recents out;
    auto listings = library.store().list();
    if (!listings) {
        out.problems.push_back(std::format("Can't list sessions: {}", bus::to_string(listings.error())));
        return out;
    }
    for (auto& listing : *listings) {
        if (auto* unreadable = std::get_if<bus::listing::Unreadable>(&listing)) {
            out.problems.push_back(
                std::format("Session {} can't be read: {}", unreadable->id, bus::to_string(unreadable->error)));
            continue;
        }
        const auto& info = std::get<bus::SessionInfo>(listing);
        auto dom = library.load(info.id);
        if (!dom) {
            out.problems.push_back(std::format("Session {} can't be read: {}", info.id, dom.error().to_string()));
            continue;
        }
        auto summary = contracts::SessionSummary::from_dom(*dom, false);
        if (summary && keeps_history(*dom))
            out.summaries.push_back(std::move(*summary));
        else
            out.empty.push_back(info.id);
    }
    std::ranges::stable_sort(out.summaries, std::greater<>{}, &contracts::SessionSummary::updated);
    return out;
}

std::vector<const core::Node*> dom_turns(const core::Dom& dom) {
    const core::Node* root = dom.root();
    if (!root) return {};
    return dom.children_of_kind(root->id, core::ElementKind::Turn);
}

std::optional<std::filesystem::path> session_cwd(const core::Dom& dom) {
    const core::Node* root = dom.root();
    if (!root) return std::nullopt;
    auto cwd = root->str_attr(core::attr::CWD);
    if (!cwd) return std::nullopt;
    return std::filesystem::path(*cwd);
}

std::string new_session_id() {
    static std::atomic<std::uint64_t> counter{0};
#if defined(_WIN32)
    const auto pid = _getpid();
#else
    const auto pid = getpid();
#endif
    return std::format("loa-{}-{}-{}", loa::runtime::time::now_ms(), pid, counter.fetch_add(1, std::memory_order_relaxed));
}

} // namespace loa::app_runtime
