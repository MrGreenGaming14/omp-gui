#pragma once

// Decision cards (port of `loa-app/src/areas/decisions/mod.rs`): approvals, questions, plan
// reviews and forms of every open session, drawn inline in the transcript and answered through
// the session's harness (or the sessions area, for plan reviews the app raised itself, or the
// tools area, for the `remote` server's questions). Unattended timeouts answer for the user
// (`TIMEOUT_BY`); new waiting cards raise a desktop notification while the window is in the
// background.

#include <expected>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/ui/session/decisions/desk.hpp"

namespace loa::app::areas::decisions {

namespace desk = loa::ui::session::decisions;

namespace msg {
struct Desk {
    desk::DeskMessage message;
};
// The harness took (or refused) an answer.
using Answered = to::decisions::Answered;
// An answer given elsewhere (a peer with drive access), routed like a card's own.
using Answer = to::decisions::Answer;
// A peer's session this node may only watch (`true`) or also drive.
struct ViewOnly {
    std::string session;
    bool view_only = false;
};
// The unattended clock ticked: check the timeouts now.
struct Clock {};
struct CloseSettings {};
} // namespace msg

using Message = std::variant<msg::Desk, msg::Answered, msg::Answer, msg::ViewOnly, msg::Clock, msg::CloseSettings>;

class State final : public TypedArea<State>, public DecisionsPort {
  public:
    static constexpr std::string_view ID = area_id::DECISIONS;
    using Message = decisions::Message;

    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    State(std::string by, desk::UnattendedView policy, std::shared_ptr<const desk::Notifier> notifier);

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    // DecisionsPort
    [[nodiscard]] bool focused() const override { return desk_.focused().has_value(); }
    [[nodiscard]] loa::app::Message dismiss() const override;
    [[nodiscard]] QWidget* card(std::string_view session, core::NodeId node, const Emit& send) const override;
    [[nodiscard]] std::optional<loa::app::Message> shortcut(const ui::shell::palette::KeyPress& key) const override;

    [[nodiscard]] const desk::Desk& desk() const noexcept { return desk_; }
    // Turns unattended prompts on or off, as the palette commands do.
    Task set_unattended(bool enabled, Ctx& ctx);

  private:
    Task sync(const SessionId& session, Ctx& ctx);
    Task answer(std::string session, core::NodeId decision, contracts::Answer answer, Ctx& ctx);
    Task notify(const std::vector<desk::Notice>& notices) const;

    desk::Desk desk_;
    // A failed system notification was reported; later ones stay quiet.
    bool notify_failed_ = false;
};

} // namespace loa::app::areas::decisions
