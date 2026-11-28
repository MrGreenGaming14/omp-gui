// Rules and the advisor for harnesses without their own: rules join each prompt, the small
// model reviews each finished turn (port of `loa-app/src/areas/sessions/hooks.rs`).

#include "sessions.hpp"

#include "loa/app/perform.hpp"
#include "loa/core/schema.hpp"

namespace loa::app::areas::sessions {

namespace {

contracts::Awaitable<std::expected<std::optional<std::string>, std::string>> review(
    std::shared_ptr<const projects::Summarizers> small_model, core::Dom dom, NodeId turn, std::optional<std::string> rules) {
    session::hooks::Advisor advisor(small_model->on(SMALL_MODEL).first);
    auto note = co_await advisor.review(dom, turn, std::move(rules));
    if (!note) co_return std::unexpected(note.error().to_string());
    co_return std::move(*note);
}

} // namespace

std::optional<std::string> State::rules(const std::filesystem::path& cwd) const {
    return session::hooks::rules_context(session::hooks::discover_rules(cwd, home_));
}

void State::start_hooks(contracts::SessionConfig& config, const Target& target) const {
    if (target.capabilities && target.capabilities->has(contracts::Feature::AdvisorRules)) return;
    auto hooks = session::hooks::TurnHooks::start(config, rules(config.cwd));
    std::lock_guard lock(hooks_mutex_);
    hooks_.insert_or_assign(config.session_id, std::move(hooks));
}

void State::hook_prompt(const Ctx& ctx, const SessionId& session, contracts::Prompt& prompt) {
    const auto* open = ctx.runtime.sessions().get(session.as_str());
    if (!open) return;
    if (detail::capabilities(ctx, open->harness_id().value_or("")).has(contracts::Feature::AdvisorRules)) return;
    std::filesystem::path cwd = ctx.runtime.cwd();
    if (const auto* root = open->dom().root())
        if (auto recorded = root->str_attr(core::attr::CWD)) cwd = std::filesystem::path(std::string(*recorded));
    auto current = rules(cwd);
    std::lock_guard lock(hooks_mutex_);
    auto it = hooks_.find(session.as_str());
    if (it == hooks_.end()) it = hooks_.emplace(session.str(), session::hooks::TurnHooks::resumed(current)).first;
    prompt.text = it->second.before_turn(std::move(current), prompt.text);
}

Task State::advise(const Ctx& ctx, const SessionId& session, NodeId turn) const {
    const auto* open = ctx.runtime.sessions().get(session.as_str());
    if (!small || !open) return Task::none();
    if (detail::capabilities(ctx, open->harness_id().value_or("")).has(contracts::Feature::AdvisorRules))
        return Task::none();
    std::optional<std::string> known;
    {
        std::lock_guard lock(hooks_mutex_);
        if (auto it = hooks_.find(session.as_str()); it != hooks_.end())
            if (auto r = it->second.rules()) known = std::string(*r);
    }
    return perform([small_model = small, dom = open->dom(), turn, known] { return review(small_model, dom, turn, known); },
                   [session, turn](std::expected<std::optional<std::string>, std::string> result) -> loa::app::Message {
                       return message(msg::Advised{session, turn, std::move(result)});
                   });
}

void State::advised(Ctx& ctx, const SessionId& session, NodeId turn,
                    std::expected<std::optional<std::string>, std::string> result) {
    auto reviewed = ctx.report("The advisor failed", std::move(result));
    if (!reviewed || !*reviewed) return;
    auto note = std::move(**reviewed);
    if (const auto* open = ctx.runtime.sessions().get(session.as_str()))
        if (auto recorded = session::hooks::record_note(*open->writer(), turn, note); !recorded)
            ctx.notify("Can't record the advisor's note: " + recorded.error().to_string());
    std::lock_guard lock(hooks_mutex_);
    if (auto it = hooks_.find(session.as_str()); it != hooks_.end()) it->second.advise(std::move(note));
}

} // namespace loa::app::areas::sessions
