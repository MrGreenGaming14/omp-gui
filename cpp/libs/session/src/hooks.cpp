#include "loa/session/hooks.hpp"

#include <asio.hpp>

#include <fstream>
#include <map>
#include <sstream>
#include <system_error>

#include "detail.hpp"
#include "loa/core/schema.hpp"

namespace loa::session::hooks {

namespace fs = std::filesystem;
using core::Value;
namespace attr = core::attr;

namespace {

std::optional<std::string> read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::nullopt;
    std::ostringstream text;
    text << in.rdbuf();
    if (in.bad()) return std::nullopt;
    return std::move(text).str();
}

std::string_view trim_quotes(std::string_view text) {
    while (!text.empty() && (text.front() == '"' || text.front() == '\'')) text.remove_prefix(1);
    while (!text.empty() && (text.back() == '"' || text.back() == '\'')) text.remove_suffix(1);
    return text;
}

Rule parse_rule(std::string name, fs::path path, std::string_view text) {
    if (!text.starts_with("---")) return Rule{std::move(name), std::move(path), std::nullopt, true, std::string(detail::trim(text))};
    const auto rest = text.substr(3);
    const auto end = rest.find("\n---");
    if (end == std::string_view::npos)
        return Rule{std::move(name), std::move(path), std::nullopt, true, std::string(detail::trim(text))};
    const auto front = rest.substr(0, end);
    const auto after = rest.substr(end + 4);
    std::optional<std::string> description;
    bool always = false;
    std::size_t pos = 0;
    while (pos < front.size()) {
        auto nl = front.find('\n', pos);
        auto line = front.substr(pos, nl == std::string_view::npos ? std::string_view::npos : nl - pos);
        pos = nl == std::string_view::npos ? front.size() : nl + 1;
        if (line.ends_with('\r')) line.remove_suffix(1);
        const auto colon = line.find(':');
        if (colon == std::string_view::npos) continue;
        const auto key = detail::trim(line.substr(0, colon));
        const auto value = detail::trim(trim_quotes(detail::trim(line.substr(colon + 1))));
        if (key == "description" && !value.empty()) description = std::string(value);
        else if (key == "alwaysApply") always = value == "true";
    }
    const auto nl = after.find('\n');
    const auto body = nl == std::string_view::npos ? std::string_view{} : after.substr(nl + 1);
    return Rule{std::move(name), std::move(path), std::move(description), always, std::string(detail::trim(body))};
}

} // namespace

std::vector<Rule> discover_rules(const fs::path& cwd, const fs::path& home) {
    std::map<std::string, Rule> rules;
    for (const auto& dir : {home / ".omp/agent/rules", cwd / ".omp/rules"}) {
        std::error_code ec;
        fs::directory_iterator it(dir, ec), end;
        if (ec) continue;
        for (; it != end; it.increment(ec)) {
            if (ec) break;
            const auto path = it->path();
            if (path.extension() != ".md") continue;
            auto text = read_file(path);
            if (!text) continue;
            auto name = path.stem().string();
            rules.insert_or_assign(name, parse_rule(name, path, *text));
        }
    }
    std::vector<Rule> out;
    out.reserve(rules.size());
    for (auto& [_, rule] : rules) out.push_back(std::move(rule));
    return out;
}

std::optional<std::string> rules_context(const std::vector<Rule>& rules) {
    if (rules.empty()) return std::nullopt;
    std::string out = "Rules for this project. Follow them.\n";
    for (const auto& rule : rules) {
        if (!rule.always) continue;
        out += "\n## Rule: " + rule.name + "\n" + rule.body + "\n";
    }
    bool listed = false;
    for (const auto& rule : rules) {
        if (rule.always) continue;
        if (!listed) out += "\nRules to read when they apply:\n";
        listed = true;
        out += "- " + rule.name + ": " + rule.description.value_or("no description") + " (" + rule.path.string() +
               ")\n";
    }
    return out;
}

TurnHooks TurnHooks::start(contracts::SessionConfig& config, std::optional<std::string> rules) {
    if (rules) {
        auto& context = config.context ? *config.context : config.context.emplace();
        if (!context.empty()) context += "\n\n";
        context += *rules;
    }
    TurnHooks hooks;
    hooks.rules_ = std::move(rules);
    return hooks;
}

TurnHooks TurnHooks::resumed(std::optional<std::string> rules) {
    TurnHooks hooks;
    hooks.rules_ = std::move(rules);
    return hooks;
}

std::string TurnHooks::before_turn(std::optional<std::string> rules, std::string_view text) {
    std::string out;
    if (rules != rules_) {
        if (rules) out += "[The project rules changed]\n" + *rules + "\n\n";
        else out += "[The project rules were removed]\n\n";
        rules_ = std::move(rules);
    }
    if (note_) {
        out += "[Advisor note on your last turn]\n" + *note_ + "\n\n";
        note_.reset();
    }
    if (out.empty()) return std::string(text);
    out += text;
    return out;
}

// The advisor's answer for a turn that needs no note.
static constexpr std::string_view NO_NOTE = "OK";

std::string Advisor::prompt(const core::Dom& dom, core::NodeId turn, std::optional<std::string_view> rules) {
    std::string prompt =
        "You are the advisor of a coding agent. Check the agent's last turn against the project rules and the "
        "user's request: broken rules, bugs, missed parts of the request, risky actions, wrong assumptions. If you "
        "find a problem, write a short note to the agent (at most five sentences) that names it and says what to "
        "do. Only if there is no problem at all, answer with the single word OK.\n";
    if (rules) {
        auto r = *rules;
        while (!r.empty() && detail::is_space(r.back())) r.remove_suffix(1);
        prompt += "\nProject rules:\n";
        prompt += r;
        prompt += '\n';
    }
    prompt += "\nThe agent's last turn:\n";
    prompt += turn_transcript(dom, turn);
    prompt += "\nEnd of the turn. Now give your note to the agent, or the single word OK.\n";
    return prompt;
}

Awaitable<Result<std::optional<std::string>>> Advisor::review(const core::Dom& dom, core::NodeId turn,
                                                              std::optional<std::string> rules) const {
    std::optional<std::string_view> r;
    if (rules) r = *rules;
    auto answer = co_await model_->summarize("", prompt(dom, turn, r));
    if (!answer) co_return std::unexpected(Error::port(std::move(answer.error())));
    auto note = detail::trim(*answer);
    while (!note.empty() && note.back() == '.') note.remove_suffix(1);
    note = detail::trim(note);
    bool ok = note.size() == NO_NOTE.size();
    for (std::size_t i = 0; ok && i < note.size(); ++i)
        ok = (note[i] | 0x20) == (NO_NOTE[i] | 0x20);
    if (note.empty() || ok) co_return std::optional<std::string>{};
    co_return std::optional<std::string>(detail::trim(*answer));
}

Result<core::NodeId> record_note(core::PatchSink& sink, core::NodeId turn, std::string_view note) {
    auto attrs = detail::attrs({{attr::ROLE, Value(core::Role::System)},
                                {attr::BY, Value(ADVISOR)},
                                {attr::STATUS, Value(core::MessageStatus::Done)}});
    auto patch = detail::commit(
        sink, core::Draft::insert(turn, core::ElementKind::Message, std::move(attrs), std::string(note)));
    if (!patch) return std::unexpected(patch.error());
    return patch->node;
}

} // namespace loa::session::hooks
