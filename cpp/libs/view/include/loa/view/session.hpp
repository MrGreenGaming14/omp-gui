#pragma once

// The session screen: composer rows and intents, decision-card and transcript
// intents, the unattended policy, and the ports the app implements. Transcript
// and decision cards render the session DOM (`loa::core`) directly; these types
// cover everything else.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"

namespace loa::view::session {

using loa::contracts::Answer;
using loa::contracts::Attachment;
using loa::contracts::Prompt;
using loa::core::SessionMode;

} // namespace loa::view::session

// Composer rows the app feeds the pickers and popups with, and what the composer asks of it.
namespace loa::view::session::composer {

// An image pasted or dropped into the composer, saved at `path`: shown as `[Image #n]` and a
// card, and sent as an `Attachment` while its token is in the text.
struct PastedImage {
    std::filesystem::path path;
    // Width and height in pixels, when known.
    std::optional<std::pair<std::uint32_t, std::uint32_t>> size;

    friend bool operator==(const PastedImage&, const PastedImage&) = default;
};

// A `/` command offered in the popup; `name` has no slash.
struct Command {
    std::string name;
    std::string description;

    friend bool operator==(const Command&, const Command&) = default;
};

// A model the session can switch to; `id` is what `set_model` takes, `role` the role it fills.
struct ModelOption {
    std::string id;
    std::string name;
    std::string provider;
    std::optional<std::string> role;

    // "role · name (provider)", without the role part when there is none.
    [[nodiscard]] std::string to_string() const;

    friend bool operator==(const ModelOption&, const ModelOption&) = default;
};

// A row under the model choices that opens something instead of picking a model.
struct PickerAction {
    std::string id;
    std::string label;

    friend bool operator==(const PickerAction&, const PickerAction&) = default;
};

// A harness prompts can be routed to; unavailable ones are listed, not picked.
struct HarnessOption {
    std::string id;
    std::string label;
    bool available = false;
    std::optional<std::string> detail;

    friend bool operator==(const HarnessOption&, const HarnessOption&) = default;
};

// A permission level: what the agent may do without asking.
struct AccessOption {
    std::string id;
    std::string label;
    std::string detail;

    friend bool operator==(const AccessOption&, const AccessOption&) = default;
};

// The folder a session works in: `path` shortened for display, `full` on hover.
struct Workspace {
    std::string path;
    std::string full;
    std::optional<std::string> branch;

    friend bool operator==(const Workspace&, const Workspace&) = default;
};

// A plain-text picker option (effort levels, modes).
struct Label {
    std::string text;

    friend bool operator==(const Label&, const Label&) = default;
};

// The source of a workflow's rules.
enum class WorkflowKind : std::uint8_t { Managed, HandWritten, None };

// A workflow offered by the composer.
struct WorkflowOption {
    std::string role;
    std::string label;
    WorkflowKind kind = WorkflowKind::None;
    bool current = false;

    friend bool operator==(const WorkflowOption&, const WorkflowOption&) = default;
};

// What the composer asks the app to do.
namespace intent {
// Start a turn: typed while idle, or the next queued message at a turn end. `@` mentions of
// known files are attachments.
struct Submit {
    Prompt prompt;
    friend bool operator==(const Submit&, const Submit&) = default;
};
// Interrupt the running turn; queued messages went back into the editor.
struct Stop {
    friend bool operator==(const Stop&, const Stop&) = default;
};
struct QueueChanged {
    std::vector<std::string> queue;
    friend bool operator==(const QueueChanged&, const QueueChanged&) = default;
};
// The `@` popup opened: pass the project's files back.
struct FilesRequested {
    std::string query;
    friend bool operator==(const FilesRequested&, const FilesRequested&) = default;
};
struct ModelChanged {
    ModelOption model;
    friend bool operator==(const ModelChanged&, const ModelChanged&) = default;
};
struct EffortChanged {
    std::string effort;
    friend bool operator==(const EffortChanged&, const EffortChanged&) = default;
};
struct ModeChanged {
    SessionMode mode = SessionMode::Normal;
    friend bool operator==(const ModeChanged&, const ModeChanged&) = default;
};
struct AccessChanged {
    std::string access;
    friend bool operator==(const AccessChanged&, const AccessChanged&) = default;
};
struct HarnessChanged {
    std::string harness;
    friend bool operator==(const HarnessChanged&, const HarnessChanged&) = default;
};
// A model action row, by id.
struct ModelAction {
    std::string id;
    friend bool operator==(const ModelAction&, const ModelAction&) = default;
};
// The folder menu's "Show in Files".
struct OpenWorkspace {
    friend bool operator==(const OpenWorkspace&, const OpenWorkspace&) = default;
};
// A folder picked from the folder menu, by full path.
struct FolderPicked {
    std::string folder;
    friend bool operator==(const FolderPicked&, const FolderPicked&) = default;
};
// "Choose another folder…".
struct BrowseFolders {
    friend bool operator==(const BrowseFolders&, const BrowseFolders&) = default;
};
// Ctrl+V was pressed. When the clipboard holds an image, save it and hand it back with
// the composer's `attach_image`; the editor pastes any text itself.
struct PasteImage {
    friend bool operator==(const PasteImage&, const PasteImage&) = default;
};
// A project was picked in the project chip: the shown session moves there, or new
// sessions join it. `std::nullopt` is "No project".
struct ProjectPicked {
    std::optional<std::string> project;
    friend bool operator==(const ProjectPicked&, const ProjectPicked&) = default;
};
struct WorkflowPicked {
    std::string role;
    friend bool operator==(const WorkflowPicked&, const WorkflowPicked&) = default;
};
struct OpenRules {
    friend bool operator==(const OpenRules&, const OpenRules&) = default;
};
} // namespace intent

using Intent = std::variant<intent::Submit, intent::Stop, intent::QueueChanged, intent::FilesRequested,
                            intent::ModelChanged, intent::EffortChanged, intent::ModeChanged, intent::AccessChanged,
                            intent::HarnessChanged, intent::ModelAction, intent::OpenWorkspace, intent::FolderPicked,
                            intent::BrowseFolders, intent::PasteImage, intent::ProjectPicked, intent::WorkflowPicked,
                            intent::OpenRules>;

} // namespace loa::view::session::composer

// Decision cards: what one session's cards and the desk over every session ask of
// the app, and the unattended policy they apply and edit.
namespace loa::view::session::decisions {

// What to answer when a decision has waited `timeout_secs`; `reason` is recorded with it.
struct DecisionDefaultView {
    std::uint64_t timeout_secs = 0;
    std::string answer;
    std::optional<std::string> reason;

    friend bool operator==(const DecisionDefaultView&, const DecisionDefaultView&) = default;
};

// Default answers for decisions nobody answers in time; a kind without one waits forever.
struct UnattendedView {
    std::optional<DecisionDefaultView> approval;
    std::optional<DecisionDefaultView> question;
    std::optional<DecisionDefaultView> plan;
    std::optional<DecisionDefaultView> form;

    [[nodiscard]] const std::optional<DecisionDefaultView>& for_kind(loa::core::DecisionKind kind) const noexcept;
    [[nodiscard]] std::optional<DecisionDefaultView>& for_kind(loa::core::DecisionKind kind) noexcept;

    friend bool operator==(const UnattendedView&, const UnattendedView&) = default;
};

// What the cards of one session ask the app to do.
namespace intent {
// Pass to the harness; the card stays locked until the DOM records the answer or the app
// reports the answer failed.
struct Answer {
    loa::core::NodeId decision;
    loa::contracts::Answer answer;
    friend bool operator==(const Answer&, const Answer&) = default;
};
struct OpenLink {
    std::string url;
    friend bool operator==(const OpenLink&, const OpenLink&) = default;
};
} // namespace intent

using Intent = std::variant<intent::Answer, intent::OpenLink>;

// What the desk over every open session asks the app to do.
namespace desk_intent {
struct Answer {
    std::string session;
    loa::core::NodeId decision;
    loa::contracts::Answer answer;
    friend bool operator==(const Answer&, const Answer&) = default;
};
struct OpenLink {
    std::string url;
    friend bool operator==(const OpenLink&, const OpenLink&) = default;
};
// The unattended settings changed: persist them.
struct PolicyChanged {
    UnattendedView policy;
    friend bool operator==(const PolicyChanged&, const PolicyChanged&) = default;
};
struct NotifyFailed {
    std::string message;
    friend bool operator==(const NotifyFailed&, const NotifyFailed&) = default;
};
} // namespace desk_intent

using DeskIntent =
    std::variant<desk_intent::Answer, desk_intent::OpenLink, desk_intent::PolicyChanged, desk_intent::NotifyFailed>;

} // namespace loa::view::session::decisions

// What the transcript asks of the app.
namespace loa::view::session::transcript {

namespace intent {
// A user message was clicked: rewind the session to it.
struct Rewind {
    loa::core::NodeId message;
    friend bool operator==(const Rewind&, const Rewind&) = default;
};
struct OpenLink {
    std::string url;
    friend bool operator==(const OpenLink&, const OpenLink&) = default;
};
} // namespace intent

using Intent = std::variant<intent::Rewind, intent::OpenLink>;

} // namespace loa::view::session::transcript

// Ports the session screen needs and the app implements.
namespace loa::view::session::port {

// A system notification about a session that needs input.
struct Notice {
    std::string session;
    std::optional<std::string> session_title;
    loa::core::NodeId decision;
    loa::core::DecisionKind kind = loa::core::DecisionKind::Approval;
    // The notification's title, e.g. "A session needs approval".
    std::string heading;
    // One line about what is asked: tool and command, question, title.
    std::string summary;

    friend bool operator==(const Notice&, const Notice&) = default;
};

struct NotifyError {
    std::string reason;

    // "system notification failed: <reason>".
    [[nodiscard]] std::string message() const;

    friend bool operator==(const NotifyError&, const NotifyError&) = default;
};

// The coroutine a `Notifier` returns.
using NotifyFuture = loa::contracts::Awaitable<std::expected<void, NotifyError>>;

// Raises notifications off the UI thread.
class Notifier {
  public:
    virtual ~Notifier() = default;
    [[nodiscard]] virtual NotifyFuture notify(const Notice& notice) const = 0;
};

} // namespace loa::view::session::port
