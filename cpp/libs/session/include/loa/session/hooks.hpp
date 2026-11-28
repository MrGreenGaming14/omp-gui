#pragma once

// Rules and advisor as hooks around each turn, for harnesses that lack them
// (`Feature::AdvisorRules` missing). Port of `loa_session::hooks`.
//
// Rules are the omp rule files: `~/.omp/agent/rules/*.md` and the project's
// `.omp/rules/*.md` (a project rule overrides a global one of the same name).
// A rule with `alwaysApply: true` or no front matter is included whole; any
// other rule is listed by its `description` with the path to read when it
// applies. The advisor reviews each finished turn with one small-model call.

#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/sink.hpp"
#include "loa/session/session.hpp"

namespace loa::session::hooks {

// `by` of advisor notes.
inline constexpr std::string_view ADVISOR = "advisor";

// One rule file.
struct Rule {
    // File stem.
    std::string name;
    std::filesystem::path path;
    std::optional<std::string> description;
    // Included whole in every turn's context.
    bool always = false;
    // Text after the front matter.
    std::string body;
    friend bool operator==(const Rule&, const Rule&) = default;
};

// The rule files that apply in `cwd`, global first, sorted by name.
[[nodiscard]] std::vector<Rule> discover_rules(const std::filesystem::path& cwd, const std::filesystem::path& home);

// The context text for `rules`, or `nullopt` without rules.
[[nodiscard]] std::optional<std::string> rules_context(const std::vector<Rule>& rules);

// The per session state of the hooks.
class TurnHooks {
  public:
    TurnHooks() = default;

    // Hooks of a session starting with `rules`, which go into its context.
    [[nodiscard]] static TurnHooks start(contracts::SessionConfig& config, std::optional<std::string> rules);
    // Hooks of a session resumed or reopened whose context already carries `rules`.
    [[nodiscard]] static TurnHooks resumed(std::optional<std::string> rules);

    // The text to send for the user's `text`, given the rules as they are now:
    // changed rules and the pending advisor note go first.
    [[nodiscard]] std::string before_turn(std::optional<std::string> rules, std::string_view text);
    // Queues an advisor note for the next prompt.
    void advise(std::string note) { note_ = std::move(note); }

    [[nodiscard]] std::optional<std::string_view> pending_note() const {
        return note_ ? std::optional<std::string_view>(*note_) : std::nullopt;
    }
    [[nodiscard]] std::optional<std::string_view> rules() const {
        return rules_ ? std::optional<std::string_view>(*rules_) : std::nullopt;
    }

  private:
    // Rules text the agent has.
    std::optional<std::string> rules_;
    // Advisor note not sent yet.
    std::optional<std::string> note_;
};

// Reviews finished turns with one small-model call each.
class Advisor {
  public:
    explicit Advisor(std::shared_ptr<const contracts::Summarizer> model) : model_(std::move(model)) {}

    // What the advisor model is asked about `turn`.
    [[nodiscard]] static std::string prompt(const core::Dom& dom, core::NodeId turn,
                                            std::optional<std::string_view> rules);

    // The note on `turn`, or `nullopt` when it needs none.
    [[nodiscard]] Awaitable<Result<std::optional<std::string>>> review(const core::Dom& dom, core::NodeId turn,
                                                                       std::optional<std::string> rules) const;

  private:
    std::shared_ptr<const contracts::Summarizer> model_;
};

// Records an advisor note at the end of `turn`.
[[nodiscard]] Result<core::NodeId> record_note(core::PatchSink& sink, core::NodeId turn, std::string_view note);

} // namespace loa::session::hooks
