#pragma once

// The service behind the model screens: everything they need from omp and the other
// harnesses, as coroutines returning the screens' data (port of `loa-models`
// `service.rs`).
//
// A `Backend` is built from injected `Parts`: the omp RPC, omp's account for provider
// logins, where omp's settings files are, a validator, the browser opener and the other
// harnesses' accounts. The app composes the real parts from `loa::harness`; tests pass
// fakes. The app keeps one `Backend` (it is cheap to copy) and turns the screens' events
// into these calls.
//
// The current session's model is not switched here: that goes through the session's own
// harness connection (`HarnessSession::set_model`).
//
// No function here accepts, stores or returns an API key value; see
// `loa::settings::models::keys` for how keys are referenced instead.

#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/account.hpp"
#include "loa/contracts/async.hpp"
#include "loa/settings/models/config.hpp"
#include "loa/settings/models/error.hpp"
#include "loa/settings/models/files.hpp"
#include "loa/settings/models/keys.hpp"
#include "loa/settings/models/login.hpp"
#include "loa/settings/models/models_yml.hpp"
#include "loa/settings/models/rpc.hpp"
#include "loa/settings/models/selector.hpp"
#include "loa/settings/models/settings.hpp"
#include "loa/settings/models/workflows.hpp"

namespace loa::models {

using contracts::HarnessAccount;
using contracts::LoginUpdate;
using contracts::UrlOpener;
using settings::models::SettingsValidator;
using settings::models::files::OmpFiles;
using settings::models::files::OmpFilesResolver;
using settings::models::keys::EnvironmentPresence;
using settings::models::keys::KeySource;
using settings::models::login::Auth;
using settings::models::login::Credential;
using settings::models::login::LoginStrategy;
using settings::models::login::OmpProviderAccount;
using settings::models::login::ProviderEntry;
using settings::models::login::ProviderKey;
using settings::models::models_yml::ModelsYml;
using settings::models::rpc::OmpRpc;
using settings::models::selector::Catalog;
using settings::models::selector::ModelInfo;
using settings::models::selector::ModelRef;
using settings::models::selector::RoleAssignment;
using settings::models::selector::Selector;
using settings::models::settings::HarnessModel;
using settings::models::settings::ModelSettings;
using settings::models::settings::ValidationError;
namespace auth = settings::models::login::auth;

// The harness id of omp's rows on the sign in screen.
inline constexpr std::string_view OMP = "omp";

// Where omp's settings files are: a fixed directory, or the one omp reports (asked once;
// a successful answer is kept, a failure is returned as is and asked again next time).
using FilesSource = std::variant<OmpFiles, std::shared_ptr<OmpFilesResolver>>;

// The parts a `Backend` is built from. Tests pass fakes.
struct Parts {
    std::shared_ptr<OmpRpc> rpc;
    // omp's account; runs provider logins. Normally owns the same RPC as `rpc`.
    std::shared_ptr<OmpProviderAccount> omp_account;
    FilesSource files;
    std::shared_ptr<SettingsValidator> validator;
    std::shared_ptr<UrlOpener> opener;
    std::vector<std::shared_ptr<HarnessAccount>> harnesses;
    std::shared_ptr<EnvironmentPresence> environment;
};

// What `Backend::load_settings` returns.
struct SettingsLoad {
    ModelSettings settings;
    // Selectors of the available models, offered when adding chain entries and overrides.
    std::vector<std::string> known_models;
    friend bool operator==(const SettingsLoad&, const SettingsLoad&) = default;
};

// The progress a running login reports; closed when the login ends. Defined in
// `loa/models/login_updates.hpp`, which pulls in Asio.
struct LoginUpdates;

// The model screens' service. Cheap to copy; copies share state.
class Backend {
  public:
    explicit Backend(Parts parts);

    // The omp RPC the backend uses.
    [[nodiscard]] std::shared_ptr<OmpRpc> rpc() const;

    // omp's settings files.
    [[nodiscard]] contracts::Awaitable<std::expected<OmpFiles, std::string>> files() const;

    // Loads omp's workflows and the snapshot used to guard later edits.
    [[nodiscard]] contracts::Awaitable<settings::models::Result<
        std::pair<std::vector<settings::models::workflows::Workflow>, settings::models::workflows::WorkflowSnapshot>>>
    load_workflows() const;
    // Saves against the exact files last loaded. Arguments must outlive the coroutine.
    [[nodiscard]] contracts::Awaitable<settings::models::Result<>> save_workflow(
        const settings::models::workflows::Workflow& workflow, std::optional<std::string_view> previous_role,
        const settings::models::workflows::WorkflowSnapshot& snapshot) const;
    // Deletes against the exact files last loaded. Arguments must outlive the coroutine.
    [[nodiscard]] contracts::Awaitable<settings::models::Result<>> delete_workflow(
        std::string_view role, const settings::models::workflows::WorkflowSnapshot& snapshot) const;

    // Every sign in row: omp's login providers, providers omp reads an API key for, then
    // one row per other harness. A part that cannot be read shows as an unavailable row
    // with the reason.
    [[nodiscard]] contracts::Awaitable<std::vector<ProviderEntry>> providers() const;

    // Runs the login of a sign in row to its end on `executor`, reporting its progress.
    // omp providers run omp's OAuth flow; other harnesses run their own login command.
    // Steer it with `submit_input` and `cancel`. Starting another login of `key` ends
    // this one.
    [[nodiscard]] std::shared_ptr<LoginUpdates> login(const asio::any_io_executor& executor, ProviderKey key) const;
    // Answers the running login's prompt of `key`.
    void submit_input(const ProviderKey& key, std::string text) const;
    // Stops the running login of `key`.
    void cancel(const ProviderKey& key) const;

    // Opens `url` in the browser.
    [[nodiscard]] std::expected<void, std::string> open_url(std::string_view url) const;

    // The stored sign ins signing out of `key` can remove: the credentials omp stores for
    // the provider, the active one first, or the one sign in another harness keeps.
    [[nodiscard]] contracts::Awaitable<std::expected<std::vector<Credential>, std::string>> credentials(ProviderKey key) const;
    // Removes the stored sign in `credential` (a `Credential::id`) of `key`: omp deletes
    // its credential, another harness runs its logout command. Returns what still signs
    // the provider in, if anything.
    [[nodiscard]] contracts::Awaitable<std::expected<std::optional<std::string>, std::string>> sign_out(
        ProviderKey key, std::optional<std::int64_t> credential) const;

    // The models the picker offers and the roles they serve.
    [[nodiscard]] contracts::Awaitable<std::expected<Catalog, std::string>> catalog() const;
    // Makes `role` resolve to `model` in omp's `config.yml`, keeping the thinking level the
    // role had. omp validates the file before it is saved.
    [[nodiscard]] contracts::Awaitable<std::expected<void, std::string>> assign_role(std::string role, ModelRef model) const;

    // Reads omp's settings for the settings screen. Available models that cannot be
    // listed (omp not running) leave the model lists empty.
    [[nodiscard]] contracts::Awaitable<std::expected<SettingsLoad, std::string>> load_settings() const;
    // Writes `settings` over what `load_settings` returned: only changed keys are edited,
    // omp validates both files before either is written, and the saved settings are read
    // back.
    [[nodiscard]] contracts::Awaitable<std::expected<ModelSettings, std::vector<ValidationError>>> save_settings(
        ModelSettings settings) const;

    struct Inner;

  private:
    std::shared_ptr<Inner> inner_;
};

} // namespace loa::models
