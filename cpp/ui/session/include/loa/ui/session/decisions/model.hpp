#pragma once

// `<decision>` nodes read out of the session DOM, and the fixed answer sets of approvals and
// plan reviews as `Choice` enums. Port of `crates/loa-ui-session/src/decisions/model.rs`.

#include <array>
#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/schema.hpp"

namespace loa::ui::session::decisions {

using loa::contracts::Answer;
using loa::core::DecisionKind;
using loa::core::DecisionStatus;
using loa::core::NodeId;

// The responses of an approval card, in shortcut order.
enum class ApprovalChoice : std::uint8_t { AllowOnce, AllowSession, Deny };
// The responses of a plan review card, in shortcut order.
enum class PlanChoice : std::uint8_t { Approve, Edit, Reject };

// A fixed answer set of a card: `ALL` in shortcut order, the wire `answer`, the button `label`
// and how an answered card reads (`past`).
template <class C>
struct Choice;

template <>
struct Choice<ApprovalChoice> {
    static constexpr std::array ALL{ApprovalChoice::AllowOnce, ApprovalChoice::AllowSession, ApprovalChoice::Deny};
    static constexpr std::array<std::string_view, 3> ANSWERS{loa::core::answer::ALLOW_ONCE, loa::core::answer::ALLOW_SESSION,
                                                             loa::core::answer::DENY};
    static constexpr std::array<std::string_view, 3> LABELS{"Allow once", "Allow for this session", "Deny"};
    static constexpr std::array<std::string_view, 3> PAST{"Allowed", "Allowed for this session", "Denied"};
};

template <>
struct Choice<PlanChoice> {
    static constexpr std::array ALL{PlanChoice::Approve, PlanChoice::Edit, PlanChoice::Reject};
    static constexpr std::array<std::string_view, 3> ANSWERS{loa::core::answer::APPROVE, loa::core::answer::EDIT,
                                                             loa::core::answer::REJECT};
    static constexpr std::array<std::string_view, 3> LABELS{"Approve", "Edit", "Reject"};
    static constexpr std::array<std::string_view, 3> PAST{"Approved", "Approved with edits", "Rejected"};
};

template <class C>
[[nodiscard]] constexpr std::string_view answer_of(C c) noexcept { return Choice<C>::ANSWERS[static_cast<std::size_t>(c)]; }
template <class C>
[[nodiscard]] constexpr std::string_view label_of(C c) noexcept { return Choice<C>::LABELS[static_cast<std::size_t>(c)]; }
template <class C>
[[nodiscard]] constexpr std::string_view past_of(C c) noexcept { return Choice<C>::PAST[static_cast<std::size_t>(c)]; }
template <class C>
[[nodiscard]] constexpr std::optional<C> from_answer(std::string_view value) noexcept {
    for (C c : Choice<C>::ALL)
        if (answer_of(c) == value) return c;
    return std::nullopt;
}

// One `<decision>` node as plain data: what a card shows and what the tracker needs.
struct Decision {
    NodeId id;
    DecisionKind kind = DecisionKind::Approval;
    // A decision inserted without `status` is waiting.
    DecisionStatus status = DecisionStatus::Waiting;
    std::optional<std::string> title;
    // Command or diff preview, question, plan markdown.
    std::string body;
    std::vector<std::string> options;
    bool multi = false;
    std::vector<std::string> answer;
    std::optional<std::string> by;
    std::optional<std::string> reason;
    // Form: JSON Schema of the fields.
    std::optional<std::string> fields;
    // Unattended runs: answer applied when `timeout_ms` passes (Rust `default`).
    std::vector<std::string> defaults;
    std::optional<std::int64_t> timeout_ms;
    std::optional<std::int64_t> created;
    // Tool of the `<call>` named by `call`, else of the enclosing `<call>`.
    std::optional<std::string> tool;
    // `harness` of the node or its nearest ancestor that has one.
    std::optional<std::string> harness;

    [[nodiscard]] static std::optional<Decision> from_dom(const loa::core::Dom& dom, NodeId id);
    // Reads `node`; `nullopt` when it is not a `<decision>` of a known kind.
    [[nodiscard]] static std::optional<Decision> from_node(const loa::core::Dom& dom, const loa::core::Node& node);
    // Every decision in the DOM, in document order.
    [[nodiscard]] static std::vector<Decision> all(const loa::core::Dom& dom);

    [[nodiscard]] bool is_waiting() const noexcept { return status == DecisionStatus::Waiting; }
    // Whether an approval decision advertises the canonical `allow_session` grant.
    [[nodiscard]] bool allows_session() const;
    // Approval choices displayed for this decision, in shortcut and click order.
    [[nodiscard]] std::span<const ApprovalChoice> approval_choices() const;
    // The decision's own default as an answer by `by`, when it is valid for the kind.
    [[nodiscard]] std::optional<Answer> default_answer(std::string_view by) const;

    friend bool operator==(const Decision&, const Decision&) = default;
};

// One line saying what was answered; an empty answer is a skipped prompt.
[[nodiscard]] std::string answer_summary(DecisionKind kind, std::span<const std::string> values);

} // namespace loa::ui::session::decisions
