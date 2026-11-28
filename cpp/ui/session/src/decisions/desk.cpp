#include "loa/ui/session/decisions/desk.hpp"

#include <algorithm>

#include <asio/awaitable.hpp>
#include <limits>

namespace loa::ui::session::decisions {

namespace attr = loa::core::attr;
namespace di = loa::view::session::decisions::desk_intent;

namespace {

std::int64_t saturating_add(std::int64_t a, std::int64_t b) {
    std::int64_t out = 0;
    if (__builtin_add_overflow(a, b, &out)) return b > 0 ? std::numeric_limits<std::int64_t>::max() : std::numeric_limits<std::int64_t>::min();
    return out;
}

std::string_view trim(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto b = s.find_first_not_of(ws);
    if (b == std::string_view::npos) return {};
    return s.substr(b, s.find_last_not_of(ws) - b + 1);
}

std::string summary(const Decision& d) {
    std::string_view first;
    std::string_view rest = d.body;
    while (!rest.empty()) {
        const auto nl = rest.find('\n');
        const auto line = trim(rest.substr(0, nl));
        rest = nl == std::string_view::npos ? std::string_view{} : rest.substr(nl + 1);
        if (!line.empty()) {
            first = line;
            break;
        }
    }
    const std::optional<std::string>& lead = d.kind == DecisionKind::Approval && d.tool ? d.tool : d.title;
    if (!lead) return std::string(first);
    if (first.empty()) return *lead;
    return *lead + ": " + std::string(first);
}

// The title of a notice about a `kind` decision in the session titled `title`.
std::string heading(const std::optional<std::string>& title, DecisionKind kind) {
    return title.value_or("A session") + " " + std::string(kind_for(kind).notice());
}

} // namespace

std::vector<Notice> Tracker::observe(const loa::core::Dom& dom, std::int64_t now_ms) {
    const auto* root = dom.root();
    if (!root) return {};
    const auto id = root->str_attr(attr::ID);
    if (!id) return {};
    const std::string session(*id);
    auto& known = sessions_[session];
    std::map<NodeId, Waiting> still;
    std::vector<Notice> notices;
    for (auto& decision : Decision::all(dom)) {
        if (!decision.is_waiting()) continue;
        Waiting waiting;
        if (auto it = known.find(decision.id); it != known.end()) {
            waiting = std::move(it->second);
            known.erase(it);
        } else {
            if (background) {
                std::optional<std::string> title;
                if (auto t = root->str_attr(attr::TITLE)) title = std::string(*t);
                notices.push_back(Notice{session, title, decision.id, decision.kind, heading(title, decision.kind), summary(decision)});
            }
            const std::int64_t since = decision.created.value_or(now_ms);
            std::optional<std::pair<std::int64_t, loa::contracts::Answer>> own;
            if (decision.timeout_ms)
                if (auto a = decision.default_answer(TIMEOUT_BY)) own.emplace(saturating_add(since, *decision.timeout_ms), std::move(*a));
            waiting = Waiting{std::move(decision), since, std::move(own), false};
        }
        const auto key = waiting.decision.id;
        still.insert_or_assign(key, std::move(waiting));
    }
    known = std::move(still);
    return notices;
}

void Tracker::forget(std::string_view session) {
    if (auto it = sessions_.find(session); it != sessions_.end()) sessions_.erase(it);
}

std::size_t Tracker::badge(std::string_view session) const {
    const auto it = sessions_.find(session);
    return it == sessions_.end() ? 0 : it->second.size();
}

std::size_t Tracker::total() const {
    std::size_t n = 0;
    for (const auto& [_, w] : sessions_) n += w.size();
    return n;
}

std::optional<std::int64_t> Tracker::deadline(const Waiting& w) const {
    if (w.own) return w.own->first;
    if (auto t = policy.timeout_ms(w.decision.kind)) return saturating_add(w.since, *t);
    return std::nullopt;
}

std::vector<Expired> Tracker::tick(std::int64_t now_ms) {
    std::vector<Expired> out;
    // `sessions_` and each session's map are ordered, so this walks (session, decision) sorted.
    for (auto& [session, waiting] : sessions_)
        for (auto& [id, w] : waiting) {
            if (w.fired) continue;
            const auto d = deadline(w);
            if (!d || *d > now_ms) continue;
            w.fired = true;
            // A hand-edited default that doesn't fit leaves it waiting for someone.
            std::optional<loa::contracts::Answer> answer;
            if (w.own) answer = w.own->second;
            else answer = policy.answer(w.decision, TIMEOUT_BY);
            if (answer) out.push_back(Expired{session, id, std::move(*answer)});
        }
    return out;
}

std::optional<std::int64_t> Tracker::next_deadline() const {
    std::optional<std::int64_t> best;
    for (const auto& [_, waiting] : sessions_)
        for (const auto& [__, w] : waiting) {
            if (w.fired) continue;
            if (auto d = deadline(w); d && (!best || *d < *best)) best = d;
        }
    return best;
}

Desk::Desk(std::string by, UnattendedView policy, std::shared_ptr<const Notifier> notifier)
    : local_(std::move(by)), notifier_(std::move(notifier)) {
    tracker_.policy = Unattended{std::move(policy)};
}

void Desk::set_local_name(std::string name) {
    for (auto& [_, s] : sessions_) s.set_local_name(name);
    local_name_ = std::move(name);
}

void Desk::set_view_only(std::string_view session, bool view_only) { state(session).set_viewer(Viewer{local_, view_only}); }

State& Desk::state(std::string_view session) {
    auto it = sessions_.find(session);
    if (it == sessions_.end()) {
        it = sessions_.emplace(std::string(session), State(Viewer{local_, false})).first;
        it->second.set_local_name(local_name_);
    }
    return it->second;
}

std::vector<Notice> Desk::sync(const loa::core::Dom& dom, std::int64_t now_ms) {
    const auto* root = dom.root();
    if (!root) return {};
    const auto session = root->str_attr(attr::ID);
    if (!session) return {};
    state(*session).sync(dom);
    return tracker_.observe(dom, now_ms);
}

std::vector<NotifyFuture> Desk::notify(const std::vector<Notice>& notices) const {
    std::vector<NotifyFuture> out;
    if (!notifier_) return out;
    out.reserve(notices.size());
    for (const auto& n : notices) out.push_back(notifier_->notify(n));
    return out;
}

void Desk::forget(std::string_view session) {
    if (auto it = sessions_.find(session); it != sessions_.end()) sessions_.erase(it);
    tracker_.forget(session);
    if (active_ && *active_ == session) active_ = std::nullopt;
}

void Desk::answer_failed(std::string_view session, NodeId decision) {
    if (auto it = sessions_.find(session); it != sessions_.end()) it->second.answer_failed(decision);
}

const State* Desk::session(std::string_view session) const {
    const auto it = sessions_.find(session);
    return it == sessions_.end() ? nullptr : &it->second;
}

std::optional<NodeId> Desk::focused() const {
    if (!active_) return std::nullopt;
    const auto* s = session(*active_);
    return s ? s->focused() : std::nullopt;
}

std::vector<DeskIntent> Desk::card(const std::string& session, Message message) {
    const auto it = sessions_.find(session);
    if (it == sessions_.end()) return {};
    auto intent = it->second.update(std::move(message)).second;
    if (!intent) return {};
    if (auto* a = std::get_if<loa::view::session::decisions::intent::Answer>(&*intent))
        return {di::Answer{session, a->decision, std::move(a->answer)}};
    return {di::OpenLink{std::get<loa::view::session::decisions::intent::OpenLink>(*intent).url}};
}

std::vector<DeskIntent> Desk::update(DeskMessage message) {
    return std::visit(
        [&](auto& m) -> std::vector<DeskIntent> {
            using T = std::decay_t<decltype(m)>;
            if constexpr (std::is_same_v<T, desk_msg::Card>) {
                return card(m.session, std::move(m.message));
            } else if constexpr (std::is_same_v<T, desk_msg::Key>) {
                if (!active_) return {};
                return card(*active_, msg::Key{m.shortcut});
            } else if constexpr (std::is_same_v<T, desk_msg::Focused>) {
                tracker_.background = !m.focused;
                return {};
            } else if constexpr (std::is_same_v<T, desk_msg::Tick>) {
                auto expired = tracker_.tick(m.now_ms);
                std::vector<DeskIntent> out;
                for (auto& e : expired) {
                    if (auto it = sessions_.find(e.session); it != sessions_.end()) it->second.answering(e.decision);
                    out.emplace_back(di::Answer{std::move(e.session), e.decision, std::move(e.answer)});
                }
                return out;
            } else if constexpr (std::is_same_v<T, desk_msg::Notified>) {
                if (m.result) return {};
                return {di::NotifyFailed{m.result.error().message()}};
            } else {
                Unattended next = tracker_.policy;
                next.update(m.edit);
                if (next == tracker_.policy) return {};
                tracker_.policy = next;
                return {di::PolicyChanged{std::move(next.view)}};
            }
        },
        message);
}

} // namespace loa::ui::session::decisions
