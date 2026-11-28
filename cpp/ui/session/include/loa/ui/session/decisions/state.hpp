#pragma once

// Decision prompt cards: approval, question, plan review and form. `State` holds the cards of one
// session; `Desk` (desk.hpp) is the app-facing component over every open session. Both report
// what the app must do as `loa::view::session::decisions` intents. Port of
// `crates/loa-ui-session/src/decisions/mod.rs`.

#include <cstddef>
#include <map>
#include <optional>
#include <string>
#include <utility>
#include <variant>

#include "loa/ui/session/decisions/card.hpp"
#include "loa/ui/shell/task.hpp"
#include "loa/view/session.hpp"

namespace loa::ui::session::decisions {

using loa::ui::shell::Task;
using loa::view::session::decisions::Intent;

// Who looks at the cards: `by` is recorded on answers; view-only peers can't answer.
struct Viewer {
    std::string by;
    bool view_only = false;
    friend bool operator==(const Viewer&, const Viewer&) = default;
};

// A keyboard shortcut for the focused card: digit `n` picks option `n - 1`, Enter confirms,
// Esc skips, denies or rejects.
struct Shortcut {
    enum class Kind : std::uint8_t { Pick, Confirm, Dismiss };
    Kind kind = Kind::Confirm;
    std::size_t index = 0;

    [[nodiscard]] static constexpr Shortcut pick(std::size_t i) { return {Kind::Pick, i}; }
    [[nodiscard]] static constexpr Shortcut confirm() { return {Kind::Confirm, 0}; }
    [[nodiscard]] static constexpr Shortcut dismiss() { return {Kind::Dismiss, 0}; }
    friend bool operator==(const Shortcut&, const Shortcut&) = default;
};

// A key press as the view sees it: a named key or the typed text, and the held modifiers.
struct KeyPress {
    enum class Named : std::uint8_t { None, Enter, Escape };
    Named named = Named::None;
    std::string text;
    bool ctrl = false, alt = false, logo = false;
};

// Maps a key press to a card shortcut; presses with Ctrl, Alt or Logo are left to the app.
[[nodiscard]] std::optional<Shortcut> shortcut(const KeyPress& key);

namespace msg {
// Click on option `index` of a card.
struct Choose {
    NodeId id;
    std::size_t index = 0;
    friend bool operator==(const Choose&, const Choose&) = default;
};
// Submit button: multi choice, form, edited plan.
struct Submit {
    NodeId id;
    friend bool operator==(const Submit&, const Submit&) = default;
};
struct Skip {
    NodeId id;
    friend bool operator==(const Skip&, const Skip&) = default;
};
// Deny reason, reject feedback or typed answer edited.
struct Text {
    NodeId id;
    std::string text;
    friend bool operator==(const Text&, const Text&) = default;
};
// Enter in that text field.
struct SubmitText {
    NodeId id;
    friend bool operator==(const SubmitText&, const SubmitText&) = default;
};
// The plan editor's whole text after an edit.
struct EditPlan {
    NodeId id;
    std::string text;
    friend bool operator==(const EditPlan&, const EditPlan&) = default;
};
struct CancelEdit {
    NodeId id;
    friend bool operator==(const CancelEdit&, const CancelEdit&) = default;
};
struct Field {
    NodeId id;
    std::size_t index = 0;
    FieldInput input;
    friend bool operator==(const Field&, const Field&) = default;
};
struct Key {
    Shortcut shortcut;
    friend bool operator==(const Key&, const Key&) = default;
};
struct Link {
    std::string url;
    friend bool operator==(const Link&, const Link&) = default;
};
// Expands or collapses the whole diff of an approval card.
struct ToggleDiff {
    NodeId id;
    friend bool operator==(const ToggleDiff&, const ToggleDiff&) = default;
};
} // namespace msg

using Message = std::variant<msg::Choose, msg::Submit, msg::Skip, msg::Text, msg::SubmitText, msg::EditPlan,
                             msg::CancelEdit, msg::Field, msg::Key, msg::Link, msg::ToggleDiff>;

// The decision cards of one session.
class State {
  public:
    explicit State(Viewer viewer) : viewer_(std::move(viewer)) {}

    void set_viewer(Viewer viewer) { viewer_ = std::move(viewer); }
    [[nodiscard]] const Viewer& viewer() const noexcept { return viewer_; }
    // The local node's host name, shown on its answers instead of its key.
    void set_local_name(std::optional<std::string> name) { local_name_ = std::move(name); }
    [[nodiscard]] const std::optional<std::string>& local_name() const noexcept { return local_name_; }

    // Brings the cards in line with the DOM, keeping what was typed into waiting cards.
    void sync(const loa::core::Dom& dom);
    // Reopens a card whose answer the harness refused.
    void answer_failed(NodeId decision);
    // Locks a card answered from elsewhere (an unattended timeout) until the DOM records it.
    void answering(NodeId decision);
    // The card that takes shortcuts: the oldest one waiting for the local user.
    [[nodiscard]] std::optional<NodeId> focused() const;

    [[nodiscard]] std::pair<Task, std::optional<Intent>> update(Message message);

    // Every card by node id (document order: ids only go up).
    [[nodiscard]] const std::map<NodeId, Card>& cards() const noexcept { return cards_; }
    [[nodiscard]] const Card* card(NodeId decision) const;
    // The footer of a card as this viewer sees it.
    [[nodiscard]] std::optional<Footer> footer_of(const Card& c) const;

  private:
    Viewer viewer_;
    std::map<NodeId, Card> cards_;
    std::optional<std::string> local_name_;
};

} // namespace loa::ui::session::decisions
