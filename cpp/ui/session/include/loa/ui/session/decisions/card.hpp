#pragma once

// One decision card: its input state, with per-kind behaviour in `CardKind` strategies (approval,
// question, plan review, form) picked by `kind_for`. Port of
// `crates/loa-ui-session/src/decisions/card.rs`; the widgets are in `view.hpp`.

#include <cstddef>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/kit/style.hpp"
#include "loa/ui/panel/diff.hpp"
#include "loa/ui/session/decisions/form.hpp"
#include "loa/ui/session/decisions/model.hpp"
#include "loa/view/theme.hpp"

namespace loa::ui::session::decisions {

using loa::ui::kit::Tone;

// `by` of an answer applied because an unattended timeout passed.
inline constexpr std::string_view TIMEOUT_BY = "timeout";
// Lines of a diff shown before the rest waits behind a button.
inline constexpr std::size_t PREVIEW_LINES = 300;
// Form answer that submits the defaults the form declares.
inline constexpr std::string_view FORM_DEFAULTS = "{}";

// Values and reason of an answer, before the answering user is added.
struct Reply {
    std::vector<std::string> values;
    std::optional<std::string> reason;
    friend bool operator==(const Reply&, const Reply&) = default;
};

// Id of a card's deny reason, reject feedback or typed answer field.
[[nodiscard]] std::string text_field_id(NodeId decision);
// Id of the text input of form field `index`.
[[nodiscard]] std::string form_field_id(NodeId decision, std::size_t index);

// What the user typed or picked into a form field; `Toggle{n}` flips option `n` of a multi field.
namespace field_input {
struct Text {
    std::string text;
    friend bool operator==(const Text&, const Text&) = default;
};
struct Bool {
    bool on = false;
    friend bool operator==(const Bool&, const Bool&) = default;
};
struct Choice {
    std::size_t index = 0;
    friend bool operator==(const Choice&, const Choice&) = default;
};
struct Toggle {
    std::size_t option = 0;
    friend bool operator==(const Toggle&, const Toggle&) = default;
};
} // namespace field_input
using FieldInput = std::variant<field_input::Text, field_input::Bool, field_input::Choice, field_input::Toggle>;

class CardKind;

struct Card {
    Decision decision;
    // Answered locally, waiting for the DOM to record it.
    bool pending = false;
    // Option highlighted by a number key, confirmed by Enter.
    std::optional<std::size_t> selected;
    // Multi choice questions: which options are ticked.
    std::vector<bool> picked;
    // Deny reason, reject feedback or the typed answer.
    std::string text;
    // The plan text while it is being edited.
    std::optional<std::string> editor;
    std::optional<std::expected<Form, FormError>> form;
    // The approval body parsed as a unified diff, with its line count; parsed when the body
    // changes, not per frame.
    std::optional<std::pair<std::vector<loa::ui::panel::diff::FileDiff>, std::size_t>> diff;
    // Whether an approval diff longer than the preview is shown in full.
    bool show_full = false;

    explicit Card(Decision d);

    // Takes the latest DOM state, keeping what the user entered while it waits.
    void refresh(Decision d);
    // Can take input from the local user.
    [[nodiscard]] bool open() const noexcept { return decision.is_waiting() && !pending; }
    [[nodiscard]] const CardKind& kind() const;
    // Number key: highlight option `index` (tick it on a multi choice question).
    void pick(std::size_t index);
    [[nodiscard]] std::optional<Reply> choose(std::size_t index);
    void set_field(std::size_t index, const FieldInput& input);
    // The trimmed text, when not empty.
    [[nodiscard]] std::optional<std::string> reason() const;

  private:
    void parse(bool body, bool fields, bool options);
};

// Per-kind card behaviour.
class CardKind {
  public:
    virtual ~CardKind() = default;
    // Header icon and what the agent wants, after its name: "wants to run".
    [[nodiscard]] virtual std::pair<loa::view::theme::Icon, std::string_view> header(const Decision& d) const = 0;
    // The system notification's tail: "needs approval".
    [[nodiscard]] virtual std::string_view notice() const = 0;
    // The unattended settings row: "Approvals".
    [[nodiscard]] virtual std::string_view settings_label() const = 0;
    // Options number keys address.
    [[nodiscard]] virtual std::size_t options(const Card& c) const = 0;
    // A click on option `index < options`.
    [[nodiscard]] virtual std::optional<Reply> choose(Card& c, std::size_t index) const = 0;
    // Enter, or the submit button.
    [[nodiscard]] virtual std::optional<Reply> confirm(Card& c) const = 0;
    // Esc: deny, reject (or leave plan editing), skip.
    [[nodiscard]] virtual std::optional<Reply> dismiss(Card& c) const = 0;
    // Enter in the card's text field.
    [[nodiscard]] virtual std::optional<Reply> submit_text(Card& c) const = 0;
    // Whether `d.defaults` is a valid answer.
    [[nodiscard]] virtual bool valid_default(const Decision& d) const = 0;
    // The decision's own default values, normalized if valid; `nullopt` when invalid.
    [[nodiscard]] virtual std::optional<std::vector<std::string>> default_values(const Decision& d) const {
        if (valid_default(d)) return d.defaults;
        return std::nullopt;
    }
    // Tone of an answered card's status line, by its first value.
    [[nodiscard]] virtual Tone outcome(std::string_view /*first*/) const { return Tone::Normal; }
    // The answer the unattended policy's `configured` value gives `d`, and whether it refuses.
    [[nodiscard]] virtual std::optional<std::pair<std::string, bool>> policy(const Decision& d,
                                                                             std::string_view configured) const = 0;
    // Answers the unattended settings offer, as `(answer, label)`; empty takes typed text.
    [[nodiscard]] virtual std::vector<std::pair<std::string_view, std::string_view>> presets() const = 0;
};

// The strategy of `kind`.
[[nodiscard]] const CardKind& kind_for(DecisionKind kind);

// How `by` reads on a card: the local node's key as `local_name`, other node keys (long
// alphanumeric ids) as their first 8 characters, anything else as is.
[[nodiscard]] std::string_view node_name(std::string_view by, std::string_view local, std::optional<std::string_view> local_name);

// The card's header line: "<harness or The agent> <what it wants>", the tool, and the title
// unless an approval names its tool.
struct Header {
    loa::view::theme::Icon icon;
    std::string lead;
    std::optional<std::string> tool;
    std::optional<std::string> title;
    friend bool operator==(const Header&, const Header&) = default;
};
[[nodiscard]] Header header(const Card& c);

// Status line of a card: who answered, why it closed, or why it can't be clicked. `json` is an
// answered form's object; `reason` is shown below, monospace when it is an edited plan.
struct Footer {
    std::string status;
    Tone tone = Tone::Muted;
    std::optional<std::string> json;
    std::optional<std::string> reason;
    bool reason_mono = false;
    friend bool operator==(const Footer&, const Footer&) = default;
};
// `nullopt` while the card waits for the local user. `local` is the viewer's key.
[[nodiscard]] std::optional<Footer> footer(const Card& c, bool view_only, std::string_view local,
                                           std::optional<std::string_view> local_name);

// One row of an approval's diff preview: a file's path, or a line with its mark (`+`, `-`, ` `).
struct PreviewRow {
    bool path = false;
    loa::ui::panel::diff::Tag tag = loa::ui::panel::diff::Tag::Equal;
    std::string text;
    friend bool operator==(const PreviewRow&, const PreviewRow&) = default;
};
// The rows of a parsed diff, each file's path then its lines; unless `full`, only the first
// `PREVIEW_LINES` rows.
[[nodiscard]] std::vector<PreviewRow> preview_rows(const std::vector<loa::ui::panel::diff::FileDiff>& files, bool full);
// The button under a diff longer than the preview: "Show all N lines" or "Show fewer lines".
[[nodiscard]] std::optional<std::string> preview_toggle(const Card& c);

} // namespace loa::ui::session::decisions
