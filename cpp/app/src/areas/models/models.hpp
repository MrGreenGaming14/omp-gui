#pragma once

// Models (port of `loa-app/src/areas/models/mod.rs`): the sign in, model and model settings
// screens, and the composer's model and effort pickers.
//
// The models a harness lists are read once per harness from its running session (omp's come
// from its catalog until a session runs); omp's role assignments come with the catalog. A model
// or effort picked while the session's harness isn't running is kept as pending and passed in
// the session config when it starts or resumes (`contribute`).

#include <cstdint>
#include <expected>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/models/service.hpp"
#include "loa/settings/models/login.hpp"
#include "loa/settings/models/selector.hpp"
#include "loa/settings/models/settings.hpp"
#include "loa/ui/settings/models/picker.hpp"
#include "loa/ui/settings/models/settings.hpp"
#include "loa/ui/settings/models/signin.hpp"
#include "loa/view/settings_models.hpp"

namespace loa::app::areas::models {

namespace screen = loa::ui::settings::models;
namespace shown = loa::view::settings::models;
using Page = to::models::Page;
using loa::settings::models::selector::Catalog;
using loa::settings::models::selector::ModelRef;
using loa::settings::models::selector::RoleAssignment;

// The composer lists every model of a harness offering at most this many; otherwise the
// current model and the ones serving a role.
inline constexpr std::size_t COMPOSER_MODELS = 12;

// Why a harness call failed.
struct Failure {
    enum class Kind : std::uint8_t {
        // The harness can't do it during a session.
        Unsupported,
        Error,
    };
    Kind kind = Kind::Error;
    std::string text;

    [[nodiscard]] static Failure from(const contracts::AdapterError& error);
    friend bool operator==(const Failure&, const Failure&) = default;
};

namespace msg {
// The user picked a model in the composer.
using Model = to::models::Model;
// The user picked an effort in the composer.
using Effort = to::models::Effort;
// The user pressed one of the composer's model actions.
using Action = to::models::Action;
// Shows a page of the models screen, loading it.
using Show = to::models::Show;
using Close = to::models::Close;
// Pick omp model role `role` for the active session, or for the next one when none runs.
using PickRole = to::models::PickRole;
// The models a harness lists for its sessions.
struct Listed {
    std::string harness;
    std::expected<std::vector<contracts::HarnessModel>, std::string> result;
};
// omp's models and role assignments.
struct CatalogRead {
    std::expected<Catalog, std::string> result;
};
struct ModelSet {
    SessionId session;
    std::string model;
    std::optional<std::string> role;
    std::expected<void, Failure> result;
};
struct EffortSet {
    SessionId session;
    std::string harness;
    std::string effort;
    std::expected<void, Failure> result;
};
struct Providers {
    std::vector<loa::settings::models::login::ProviderEntry> providers;
};
struct SignIn {
    screen::signin::Message message;
};
struct Login {
    shown::ProviderKey key;
    contracts::LoginUpdate update;
};
// The stored sign ins a row's sign out may remove.
struct Credentials {
    shown::ProviderKey key;
    std::expected<std::vector<loa::settings::models::login::Credential>, std::string> result;
};
// A stored sign in was removed: what still signs the provider in, if anything.
struct SignedOut {
    shown::ProviderKey key;
    std::expected<std::optional<std::string>, std::string> result;
};
struct Picker {
    screen::picker::Message message;
};
struct RoleAssigned {
    std::expected<void, std::string> result;
};
// The model and thinking level of the role picked with no running session.
struct PendingRole {
    std::optional<std::string> session;
    std::string harness;
    std::string role;
    std::expected<std::pair<std::string, std::optional<std::string>>, std::string> result;
};
struct SettingsLoaded {
    std::expected<loa::models::SettingsLoad, std::string> result;
};
struct Settings {
    screen::settings::Message message;
};
struct Saved {
    std::expected<loa::settings::models::settings::ModelSettings, std::vector<loa::settings::models::settings::ValidationError>> result;
};
} // namespace msg

using Message = std::variant<msg::Model, msg::Effort, msg::Action, msg::Show, msg::Close, msg::PickRole, msg::Listed,
                             msg::CatalogRead, msg::ModelSet, msg::EffortSet, msg::Providers, msg::SignIn, msg::Login,
                             msg::Credentials, msg::SignedOut, msg::Picker, msg::RoleAssigned, msg::PendingRole,
                             msg::SettingsLoaded, msg::Settings, msg::Saved>;

// A page's content while it is read.
struct Loading {};
struct Failed {
    std::string error;
};
template <typename T>
using Load = std::variant<Loading, Failed, T>;

// The open model picker and how its picks reach the harness.
struct OpenPicker {
    screen::picker::State state;
    // The harness model id of each row's model.
    std::map<ModelRef, std::string> ids;
};

// A model or effort chosen while the session's harness wasn't running, passed in the config
// when it starts or resumes.
struct Pending {
    std::string harness;
    // Harness model id and the role it was picked through.
    std::optional<std::pair<std::string, std::optional<std::string>>> model;
    std::optional<std::string> effort;
};

class State final : public TypedArea<State>, public ModelsPort {
  public:
    static constexpr std::string_view ID = area_id::MODELS;
    using Message = models::Message;

    explicit State(loa::models::Backend backend) : backend(std::move(backend)) {}

    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;

    // ModelsPort
    [[nodiscard]] Task read_catalog() override { return read_roles(); }
    [[nodiscard]] QWidget* page_body(Page page, const Emit& send) const override;

    // Shows `page` and reads what it shows.
    Task show(Page page, Ctx& ctx);

    [[nodiscard]] Page page() const noexcept { return page_; }
    [[nodiscard]] const Load<screen::signin::State>& signin() const noexcept { return signin_; }
    [[nodiscard]] const std::pair<std::string, Load<OpenPicker>>& picker() const noexcept { return picker_; }
    // Puts `picker` up for harness `harness`, as `show(Page::Model)` does once the models are read.
    void set_picker(std::string harness, Load<OpenPicker> picker) { picker_ = {std::move(harness), std::move(picker)}; }
    [[nodiscard]] const Load<screen::settings::State>& settings() const noexcept { return settings_; }
    [[nodiscard]] const std::map<std::optional<std::string>, Pending>& pending() const noexcept { return pending_; }

    // The model screens' backend.
    loa::models::Backend backend;

  private:
    [[nodiscard]] Task read_providers() const;
    [[nodiscard]] Task read_roles() const;
    Task load_picker(Ctx& ctx);
    void open_picker(const Ctx& ctx);
    [[nodiscard]] shown::SessionModel current(const Ctx& ctx) const;
    Task choose(std::string model, std::optional<std::string> role, Ctx& ctx);
    Task set_effort(std::string effort, Ctx& ctx);
    [[nodiscard]] Task list_models(const Ctx& ctx) const;
    void sync(Ctx& ctx) const;
    Task signin_event(shown::signin::Intent intent, Ctx& ctx);
    Task pick_role(std::string role, Ctx& ctx);
    Task picker_event(shown::picker::Intent intent, Ctx& ctx);
    Task refresh_models(Ctx& ctx);
    Pending& pending_for(std::optional<std::string> key, const std::string& harness) const;

    Page page_ = Page::Model;
    Load<screen::signin::State> signin_;
    // The harness whose models the picker shows, and the picker.
    std::pair<std::string, Load<OpenPicker>> picker_;
    Load<screen::settings::State> settings_;
    // The models each harness listed, by harness id.
    std::map<std::string, std::vector<contracts::HarnessModel>, std::less<>> listed_;
    // omp's role assignments; `nullopt` until read.
    std::optional<std::vector<RoleAssignment>> roles_;
    // Keyed by session id; `nullopt` is the next new session. `contribute` takes from it.
    mutable std::map<std::optional<std::string>, Pending> pending_;
};

} // namespace loa::app::areas::models
