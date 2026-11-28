#pragma once

// Digest prompts and answers: a bounded transcript of the session (recent turns whole, older ones
// abbreviated, tool output clipped) goes to a `Summarizer`; its strict JSON answer is validated and its
// turn labels mapped back to node ids.

#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/projects/error.hpp"
#include "loa/settings/projects.hpp"

namespace loa::projects {

using contracts::Summarizer;

// Picks the `Summarizer` for a model or role (none: the default) and the model name recorded as
// `<digest model>`.
class Summarizers {
  public:
    virtual ~Summarizers() = default;
    [[nodiscard]] virtual std::pair<std::shared_ptr<const Summarizer>, std::optional<std::string>> on(
        std::optional<std::string_view> model) const = 0;
};

// One summarizer for every model, recording no model name.
class OneSummarizer final : public Summarizers {
  public:
    explicit OneSummarizer(std::shared_ptr<const Summarizer> summarizer) : summarizer_(std::move(summarizer)) {}
    [[nodiscard]] std::pair<std::shared_ptr<const Summarizer>, std::optional<std::string>> on(
        std::optional<std::string_view>) const override {
        return {summarizer_, std::nullopt};
    }

  private:
    std::shared_ptr<const Summarizer> summarizer_;
};

// Size bounds of the digest prompt, in bytes.
struct PromptLimits {
    // Whole transcript; older turns are dropped past it.
    std::size_t max_chars = 24'000;
    // Newest turns rendered whole.
    std::size_t full_turns = 4;
    std::size_t message_chars = 2'000;
    // A tool input or result in a whole turn.
    std::size_t tool_chars = 400;
    // A message in an abbreviated turn.
    std::size_t brief_chars = 200;
    friend bool operator==(const PromptLimits&, const PromptLimits&) = default;
};

// A rendered transcript and its `T<n>` turn labels.
struct DigestPrompt {
    std::string text;
    std::vector<core::NodeId> labels;
    std::uint64_t source_seq = 0;
};

// A digest ready for `write_drafts`.
struct BuiltDigest {
    settings::DigestContent content;
    std::uint64_t source_seq = 0;
    std::optional<std::string> model;
    friend bool operator==(const BuiltDigest&, const BuiltDigest&) = default;
};

extern const std::string_view INSTRUCTIONS;

// Turns session DOMs into digests with one summarizer call each.
class Digester {
  public:
    Digester(std::shared_ptr<const Summarizers> summarizers, PromptLimits limits)
        : summarizers_(std::move(summarizers)), limits_(limits) {}

    // `model` is the project's digest model, if it sets one. `dom` must outlive the coroutine.
    [[nodiscard]] contracts::Awaitable<Result<BuiltDigest>> digest(const core::Dom& dom,
                                                                   std::optional<std::string> model) const;

  private:
    std::shared_ptr<const Summarizers> summarizers_;
    PromptLimits limits_;
};

// The transcript of `dom`; NothingToDigest without turns.
[[nodiscard]] Result<DigestPrompt> build_prompt(const core::Dom& dom, const PromptLimits& limits);

// Parses and validates an answer to `prompt`, ignoring text around the JSON object.
[[nodiscard]] Result<settings::DigestContent> parse_answer(std::string_view text, const DigestPrompt& prompt);

} // namespace loa::projects
