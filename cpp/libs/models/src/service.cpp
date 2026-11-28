#include "loa/models/service.hpp"

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <deque>
#include <mutex>
#include <span>
#include <unordered_map>

#include <asio/as_tuple.hpp>
#include <asio/co_spawn.hpp>
#include <asio/detached.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>

#include "loa/contracts/login_channels.hpp"
#include "loa/models/login_updates.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/settings/models/rpc_channel.hpp"

namespace loa::models {

namespace {

namespace sm = settings::models;
namespace rpc = settings::models::rpc;
namespace cfg = settings::models::config;
namespace keys = settings::models::keys;
namespace st = settings::models::settings;
using contracts::CommandSpec;
using contracts::LoginControl;
using contracts::LoginKind;
using contracts::SignInMethod;
using contracts::SignInState;

template <typename T>
using Awaitable = contracts::Awaitable<T>;

constexpr auto tuple_awaitable = asio::as_tuple(asio::use_awaitable);

// Decodes one UTF-8 scalar at `i`, advancing it; invalid bytes decode as U+FFFD.
char32_t next_char(std::string_view s, std::size_t& i) {
    const auto b = static_cast<unsigned char>(s[i++]);
    if (b < 0x80) return b;
    int extra = b >= 0xF0 ? 3 : b >= 0xE0 ? 2 : b >= 0xC0 ? 1 : -1;
    if (extra < 0) return 0xFFFD;
    char32_t c = b & (0x3F >> extra);
    for (int k = 0; k < extra; ++k) {
        if (i >= s.size() || (static_cast<unsigned char>(s[i]) & 0xC0) != 0x80) return 0xFFFD;
        c = (c << 6) | (static_cast<unsigned char>(s[i++]) & 0x3F);
    }
    return c;
}

// Rust `char::is_whitespace` (Unicode White_Space).
bool is_whitespace(char32_t c) {
    return (c >= 0x09 && c <= 0x0D) || c == 0x20 || c == 0x85 || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) ||
           c == 0x2028 || c == 0x2029 || c == 0x202F || c == 0x205F || c == 0x3000;
}

// Rust `char::is_control` (general category Cc).
bool is_control(char32_t c) { return c <= 0x1F || (c >= 0x7F && c <= 0x9F); }

std::string join(std::span<const std::string_view> items, std::string_view sep) {
    std::string out;
    for (std::size_t i = 0; i < items.size(); ++i) {
        if (i) out += sep;
        out += items[i];
    }
    return out;
}

std::string lowercase(std::string s) {
    for (auto& c : s) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return s;
}

// A harness account exposed on the sign-in and settings screens.
struct Harness {
    std::shared_ptr<HarnessAccount> account;

    [[nodiscard]] std::string_view key() const { return account->id(); }
    [[nodiscard]] std::string_view name() const { return account->name(); }

    [[nodiscard]] std::expected<HarnessModel, std::string> model_settings() const {
        auto settings = account->read_model();
        if (!settings) return std::unexpected(settings.error().message());
        HarnessModel m;
        m.harness = std::string(key());
        m.name = std::string(name());
        if (auto path = account->config_path()) m.config_path = path->string();
        m.model = settings->model.value_or("");
        m.effort = settings->effort.value_or("");
        for (auto e : account->efforts()) m.efforts.emplace_back(e);
        return m;
    }

    [[nodiscard]] std::expected<void, std::string> check(const HarnessModel& after) const {
        for (std::size_t i = 0; i < after.model.size();) {
            const char32_t c = next_char(after.model, i);
            if (is_whitespace(c) || is_control(c)) return std::unexpected("model `" + after.model + "` must have no spaces");
        }
        const auto efforts = account->efforts();
        if (!after.effort.empty() && std::ranges::find(efforts, std::string_view(after.effort)) == efforts.end()) {
            return std::unexpected("effort `" + after.effort + "` is not one of " + join(efforts, ", "));
        }
        return {};
    }

    [[nodiscard]] std::expected<void, std::string> write(const HarnessModel& before, const HarnessModel& after) const {
        auto settings = account->read_model();
        if (!settings) return std::unexpected(settings.error().message());
        if (before.model != after.model) {
            settings->model = after.model.empty() ? std::nullopt : std::optional(after.model);
        }
        if (before.effort != after.effort) {
            settings->effort = after.effort.empty() ? std::nullopt : std::optional(after.effort);
        }
        if (auto written = account->write_model(*settings); !written) return std::unexpected(written.error().message());
        return {};
    }
};

// An async mutex (tokio `Mutex`): held across awaits, waiters woken one at a time.
class AsyncLock {
  public:
    Awaitable<void> lock() {
        auto executor = co_await asio::this_coro::executor;
        for (;;) {
            std::shared_ptr<asio::steady_timer> timer;
            {
                std::lock_guard guard(mutex_);
                if (!held_) {
                    held_ = true;
                    co_return;
                }
                timer = std::make_shared<asio::steady_timer>(executor, asio::steady_timer::time_point::max());
                waiters_.push_back(timer);
            }
            co_await timer->async_wait(tuple_awaitable);
        }
    }

    void unlock() {
        std::lock_guard guard(mutex_);
        held_ = false;
        if (!waiters_.empty()) {
            // Expiring (rather than cancelling) also wakes a waiter that has not started waiting yet.
            waiters_.front()->expires_at(asio::steady_timer::time_point::min());
            waiters_.pop_front();
        }
    }

  private:
    std::mutex mutex_;
    bool held_ = false;
    std::deque<std::shared_ptr<asio::steady_timer>> waiters_;
};

struct Unlock {
    AsyncLock* lock;
    ~Unlock() { lock->unlock(); }
};

// The settings shown by the screen and the files they were read from, kept so a save
// edits exactly what was shown and notices outside changes.
struct Loaded {
    cfg::OmpConfig config;
    ModelsYml models;
    ModelSettings settings;
    std::vector<rpc::ModelInfo> available;
};

using ControlChannel = contracts::LoginControlChannel;

// A command as the user would type it: the program's name and its arguments.
std::string command_line(const CommandSpec& command) {
    std::string out = command.program.stem().empty() ? command.program_name() : command.program.stem().string();
    for (const auto& arg : command.args) out += " " + arg;
    return out;
}

// What a harness still signs in with, as a sign out reports it.
std::string method_name(const SignInMethod& method) {
    switch (method.kind) {
    case SignInMethod::Kind::Subscription: return "a subscription";
    case SignInMethod::Kind::ApiKey: return "an API key";
    case SignInMethod::Kind::ApiKeyHelper: return "an API key helper";
    case SignInMethod::Kind::OAuthToken: return "an OAuth token";
    case SignInMethod::Kind::ThirdParty: return "a third-party provider";
    case SignInMethod::Kind::Other: break;
    }
    return method.other;
}

using Roles = std::unordered_map<std::string_view, std::span<const Selector>>;

std::optional<ModelRef> resolve_role(std::string_view role, const Roles& roles, std::span<const rpc::ModelInfo> available,
                                     std::size_t depth) {
    const auto it = roles.find(role);
    if (it == roles.end()) return std::nullopt;
    for (const auto& candidate : it->second) {
        std::optional<ModelRef> found;
        if (!candidate.is_role()) {
            const auto& model = candidate.name();
            auto m = std::ranges::find_if(available, [&](const rpc::ModelInfo& m) { return m.selector() == model; });
            if (m == available.end()) m = std::ranges::find_if(available, [&](const rpc::ModelInfo& m) { return m.id == model; });
            if (m != available.end()) found = ModelRef{m->provider, m->id};
        } else if (depth < 4) {
            found = resolve_role(candidate.name(), roles, available, depth + 1);
        }
        if (found) return found;
    }
    return std::nullopt;
}

std::expected<Catalog, std::string> make_catalog(std::span<const rpc::ModelInfo> available, const cfg::OmpConfig& config,
                                                 const std::unordered_map<std::string, std::string>& names) {
    auto assignments = config.model_roles();
    if (!assignments) return std::unexpected(assignments.error().to_string());
    Roles roles;
    for (const auto& a : *assignments) roles.insert_or_assign(std::string_view(a.role), std::span<const Selector>(a.candidates));
    Catalog catalog;
    for (const auto& m : available) {
        const auto name = names.find(m.provider);
        catalog.models.push_back({ModelRef{m.provider, m.id}, m.name, name == names.end() ? std::string() : name->second});
    }
    for (const auto& a : *assignments) {
        if (auto model = resolve_role(a.role, roles, available, 0)) catalog.roles.push_back({a.role, std::move(*model)});
    }
    return catalog;
}

std::vector<ValidationError> fail(std::string_view location, std::string message) {
    return {ValidationError{std::string(location), std::move(message)}};
}

} // namespace

struct Backend::Inner {
    std::shared_ptr<OmpRpc> rpc;
    FilesSource files;
    // The resolved files of a resolver source.
    std::mutex resolved_mutex;
    std::optional<OmpFiles> resolved;
    std::shared_ptr<SettingsValidator> validator;
    std::shared_ptr<UrlOpener> opener;
    std::shared_ptr<OmpProviderAccount> omp_account;
    std::vector<Harness> harnesses;
    std::shared_ptr<EnvironmentPresence> environment;
    // omp's login providers from the last listing, for display names and to start logins.
    std::mutex providers_mutex;
    std::vector<rpc::LoginProvider> login_providers;
    // Controls of the running logins, tagged with the login that registered them so a
    // finished login never removes its replacement's controls.
    std::mutex logins_mutex;
    std::map<ProviderKey, std::pair<std::uint64_t, std::shared_ptr<ControlChannel>>> logins;
    std::atomic<std::uint64_t> next_login{0};
    AsyncLock loaded_lock;
    std::optional<Loaded> loaded;

    std::vector<rpc::LoginProvider> providers_now() {
        std::lock_guard guard(providers_mutex);
        return login_providers;
    }

    std::unordered_map<std::string, std::string> provider_names() {
        std::unordered_map<std::string, std::string> names;
        std::lock_guard guard(providers_mutex);
        for (const auto& p : login_providers) names.insert_or_assign(p.id, p.name);
        return names;
    }

    Awaitable<std::expected<std::vector<rpc::LoginProvider>, std::string>> refresh_login_providers() {
        auto providers = co_await rpc->get_login_providers();
        if (!providers) co_return std::unexpected(providers.error().to_string());
        {
            std::lock_guard guard(providers_mutex);
            login_providers = *providers;
        }
        co_return std::move(*providers);
    }

    const Harness* harness(std::string_view key) const {
        const auto it = std::ranges::find_if(harnesses, [&](const Harness& h) { return h.key() == key; });
        return it == harnesses.end() ? nullptr : &*it;
    }

    // The account and logout command of a harness other than omp.
    std::expected<std::pair<const Harness*, CommandSpec>, std::string> logout_command(const ProviderKey& key) const {
        const auto* h = harness(key.harness);
        if (!h) return std::unexpected("unknown harness " + key.harness);
        auto command = h->account->logout_command();
        if (!command) return std::unexpected(std::string(h->name()) + " has no sign out");
        return std::pair{h, std::move(*command)};
    }

    // The other harnesses' model settings; a harness whose config cannot be read is left out.
    std::vector<HarnessModel> harness_models() const {
        std::vector<HarnessModel> out;
        for (const auto& h : harnesses) {
            if (auto m = h.model_settings()) out.push_back(std::move(*m));
        }
        return out;
    }
};

Backend::Backend(Parts parts) : inner_(std::make_shared<Inner>()) {
    inner_->rpc = std::move(parts.rpc);
    inner_->files = std::move(parts.files);
    inner_->validator = std::move(parts.validator);
    inner_->opener = std::move(parts.opener);
    inner_->omp_account = std::move(parts.omp_account);
    for (auto& h : parts.harnesses) inner_->harnesses.push_back(Harness{std::move(h)});
    inner_->environment = std::move(parts.environment);
}

std::shared_ptr<OmpRpc> Backend::rpc() const { return inner_->rpc; }

Awaitable<std::expected<OmpFiles, std::string>> Backend::files() const {
    auto inner = inner_;
    if (const auto* fixed = std::get_if<OmpFiles>(&inner->files)) co_return *fixed;
    {
        std::lock_guard guard(inner->resolved_mutex);
        if (inner->resolved) co_return *inner->resolved;
    }
    auto resolved = co_await std::get<std::shared_ptr<OmpFilesResolver>>(inner->files)->resolve();
    if (!resolved) co_return std::unexpected(resolved.error().to_string());
    std::lock_guard guard(inner->resolved_mutex);
    if (!inner->resolved) inner->resolved = std::move(*resolved);
    co_return *inner->resolved;
}

Awaitable<sm::Result<std::pair<std::vector<sm::workflows::Workflow>, sm::workflows::WorkflowSnapshot>>> Backend::load_workflows()
    const {
    auto files = co_await this->files();
    if (!files) co_return std::unexpected(sm::Error::invalid("files", files.error()));
    co_return sm::workflows::WorkflowFiles::from(*files).load();
}

Awaitable<sm::Result<>> Backend::save_workflow(const sm::workflows::Workflow& workflow, std::optional<std::string_view> previous_role,
                                               const sm::workflows::WorkflowSnapshot& snapshot) const {
    auto inner = inner_;
    auto files = co_await this->files();
    if (!files) co_return std::unexpected(sm::Error::invalid("files", files.error()));
    const auto workflow_files = sm::workflows::WorkflowFiles::from(*files);
    co_return co_await workflow_files.save(workflow, previous_role, snapshot, *inner->validator);
}

Awaitable<sm::Result<>> Backend::delete_workflow(std::string_view role, const sm::workflows::WorkflowSnapshot& snapshot) const {
    auto inner = inner_;
    auto files = co_await this->files();
    if (!files) co_return std::unexpected(sm::Error::invalid("files", files.error()));
    const auto workflow_files = sm::workflows::WorkflowFiles::from(*files);
    co_return co_await workflow_files.remove(role, snapshot, *inner->validator);
}

Awaitable<std::vector<ProviderEntry>> Backend::providers() const {
    auto inner = inner_;
    std::vector<ProviderEntry> entries;
    auto listed = co_await inner->refresh_login_providers();
    if (listed) {
        auto providers = std::move(*listed);
        const auto rank = [](const rpc::LoginProvider& p) { return p.authenticated ? 0 : p.available ? 1 : 2; };
        std::ranges::stable_sort(providers, [&](const rpc::LoginProvider& a, const rpc::LoginProvider& b) {
            if (rank(a) != rank(b)) return rank(a) < rank(b);
            return lowercase(a.name) < lowercase(b.name);
        });
        for (auto& p : providers) {
            Auth auth = p.authenticated ? Auth(auth::Login{true, std::nullopt})
                        : p.available   ? Auth(auth::Login{false, std::nullopt})
                                        : Auth(auth::Unavailable{"omp offers no sign in for this provider now"});
            entries.push_back({ProviderKey{std::string(OMP), p.id}, std::move(p.name), std::string(OMP), std::move(auth)});
        }
    } else {
        entries.push_back({ProviderKey{std::string(OMP), std::string(OMP)}, "omp", std::string(OMP), auth::Unavailable{listed.error()}});
    }

    // API key providers: the custom providers of `models.yml` and the built in providers
    // whose key variable is set. Names only, never key values.
    if (auto files = co_await this->files()) {
        if (auto models = ModelsYml::load(files->models())) {
            const auto& env = *inner->environment;
            std::vector<std::string> oauth;
            for (const auto& p : inner->providers_now()) oauth.push_back(p.id);
            const auto add = [&](std::string_view provider, const KeySource& source) {
                const auto names = keys::builtin_environment_names(provider);
                std::optional<std::string> configured_via;
                switch (source.kind) {
                case KeySource::Kind::ConfigEnvironment: configured_via = source.name + " (models.yml)"; break;
                case KeySource::Kind::SecretCommand: configured_via = "a secret command (models.yml)"; break;
                case KeySource::Kind::ConfigEntry: configured_via = "models.yml"; break;
                case KeySource::Kind::Environment: configured_via = source.name; break;
                case KeySource::Kind::NotConfigured: break;
                }
                entries.push_back({ProviderKey{std::string(OMP), std::string(provider)}, std::string(provider), "omp API keys",
                                   auth::ApiKey{std::move(configured_via), keys::key_instructions(provider, source, names)}});
            };
            for (const auto& entry : models->providers()) {
                if (!entry.api_key && !entry.base_url) continue;
                if (std::ranges::find(oauth, entry.id) != oauth.end()) continue;
                add(entry.id, keys::credential_source(*models, entry.id, keys::builtin_environment_names(entry.id), env));
            }
            static constexpr std::string_view BUILTIN[] = {
                "anthropic", "openai",   "google",   "groq",     "openrouter", "mistral",        "xai",
                "deepseek",  "together", "fireworks", "cerebras", "azure",      "amazon-bedrock", "google-vertex",
            };
            for (const auto provider : BUILTIN) {
                if (models->provider(provider)) continue;
                const auto source = keys::credential_source(*models, provider, keys::builtin_environment_names(provider), env);
                if (source.kind == KeySource::Kind::Environment) add(provider, source);
            }
        }
    }

    for (const auto& h : inner->harnesses) {
        const auto state = co_await h.account->sign_in_state();
        Auth auth = std::holds_alternative<contracts::SignedIn>(state)    ? Auth(auth::Login{true, std::nullopt})
                    : std::holds_alternative<contracts::SignedOut>(state) ? Auth(auth::Login{false, std::nullopt})
                                                                          : Auth(auth::Unavailable{std::get<contracts::SignInUnknown>(state).reason});
        entries.push_back({ProviderKey{std::string(h.key()), std::string(h.key())}, std::string(h.name()), std::string(h.name()),
                           std::move(auth)});
    }
    co_return entries;
}

namespace {

Awaitable<void> send_update(LoginUpdates& updates, LoginUpdate update) {
    co_await updates.channel.async_send(asio::error_code{}, std::move(update), tuple_awaitable);
}

// Forwards the screen's controls to the strategy until either side closes.
Awaitable<void> forward_controls(std::shared_ptr<ControlChannel> controls, std::shared_ptr<contracts::LoginChannels> channels) {
    for (;;) {
        auto [ec, control] = co_await controls->async_receive(tuple_awaitable);
        if (ec) co_return;
        auto [sent] = co_await channels->control.async_send(asio::error_code{}, std::move(control), tuple_awaitable);
        if (sent) co_return;
    }
}

// Runs the strategy until it ends or its controls close, then closes its updates.
Awaitable<void> run_strategy(LoginStrategy& strategy, std::shared_ptr<ControlChannel> controls,
                             std::shared_ptr<contracts::LoginChannels> channels) {
    using namespace asio::experimental::awaitable_operators;
    co_await (strategy.run(channels) || forward_controls(std::move(controls), channels));
    channels->control.close();
    channels->updates.close();
}

Awaitable<void> forward_updates(std::shared_ptr<contracts::LoginChannels> channels, std::optional<Harness> harness,
                                std::shared_ptr<LoginUpdates> updates) {
    for (;;) {
        auto [ec, update] = co_await channels->updates.async_receive(tuple_awaitable);
        if (ec) co_return;
        if (harness && std::holds_alternative<contracts::login::Succeeded>(update)) {
            const auto state = co_await harness->account->sign_in_state();
            if (!std::holds_alternative<contracts::SignedIn>(state)) {
                update = contracts::login::Failed{std::string(harness->name()) + " still reports no sign in"};
            }
        }
        co_await send_update(*updates, std::move(update));
    }
}

Awaitable<void> run_login(std::shared_ptr<Backend::Inner> inner, ProviderKey key, std::shared_ptr<ControlChannel> controls,
                          std::shared_ptr<LoginUpdates> updates) {
    std::unique_ptr<LoginStrategy> strategy;
    std::optional<Harness> harness;
    if (key.harness == OMP) {
        std::optional<rpc::LoginProvider> provider;
        for (const auto& p : inner->providers_now()) {
            if (p.id == key.provider) provider = p;
        }
        if (!provider) provider = rpc::LoginProvider{key.provider, key.provider, true, false};
        auto made = inner->omp_account->login_provider(std::move(*provider));
        if (!made) {
            co_await send_update(*updates, contracts::login::Failed{made.error().message()});
            co_return;
        }
        strategy = std::move(*made);
    } else {
        const auto* h = inner->harness(key.harness);
        if (!h) {
            co_await send_update(*updates, contracts::login::Failed{"unknown harness " + key.harness});
            co_return;
        }
        auto made = h->account->login(LoginKind::Browser);
        if (!made) {
            co_await send_update(*updates, contracts::login::Failed{made.error().message()});
            co_return;
        }
        strategy = std::move(*made);
        harness = *h;
    }
    using namespace asio::experimental::awaitable_operators;
    auto channels = std::make_shared<contracts::LoginChannels>(co_await asio::this_coro::executor, 16);
    co_await (run_strategy(*strategy, std::move(controls), channels) && forward_updates(channels, std::move(harness), updates));
}

Awaitable<void> login_task(std::shared_ptr<Backend::Inner> inner, ProviderKey key, std::uint64_t generation,
                           std::shared_ptr<ControlChannel> controls, std::shared_ptr<LoginUpdates> updates) {
    try {
        co_await run_login(inner, key, std::move(controls), updates);
    } catch (...) {
    }
    {
        std::lock_guard guard(inner->logins_mutex);
        const auto it = inner->logins.find(key);
        if (it != inner->logins.end() && it->second.first == generation) inner->logins.erase(it);
    }
    updates->channel.close();
}

} // namespace

std::shared_ptr<LoginUpdates> Backend::login(const asio::any_io_executor& executor, ProviderKey key) const {
    auto controls = std::make_shared<ControlChannel>(executor, 64);
    const auto generation = inner_->next_login.fetch_add(1, std::memory_order_relaxed);
    {
        std::lock_guard guard(inner_->logins_mutex);
        auto& slot = inner_->logins[key];
        // Replacing the controls ends the previous login (Rust drops its sender).
        if (slot.second) slot.second->close();
        slot = {generation, controls};
    }
    auto updates = std::make_shared<LoginUpdates>(executor);
    asio::co_spawn(executor, login_task(inner_, std::move(key), generation, std::move(controls), updates), asio::detached);
    return updates;
}

void Backend::submit_input(const ProviderKey& key, std::string text) const {
    std::lock_guard guard(inner_->logins_mutex);
    if (const auto it = inner_->logins.find(key); it != inner_->logins.end()) {
        (void)it->second.second->try_send(asio::error_code{}, contracts::SubmitInput{std::move(text)});
    }
}

void Backend::cancel(const ProviderKey& key) const {
    std::lock_guard guard(inner_->logins_mutex);
    if (const auto it = inner_->logins.find(key); it != inner_->logins.end()) {
        (void)it->second.second->try_send(asio::error_code{}, contracts::CancelLogin{});
    }
}

std::expected<void, std::string> Backend::open_url(std::string_view url) const { return inner_->opener->open_url(url); }

Awaitable<std::expected<std::vector<Credential>, std::string>> Backend::credentials(ProviderKey key) const {
    auto inner = inner_;
    if (key.harness == OMP) {
        auto accounts = co_await inner->rpc->get_logout_accounts(key.provider);
        if (!accounts) co_return std::unexpected(accounts.error().to_string());
        std::vector<Credential> out;
        for (auto& a : *accounts) out.push_back({a.credential_id, std::move(a.label), std::move(a.detail), a.active});
        co_return out;
    }
    auto found = inner->logout_command(key);
    if (!found) co_return std::unexpected(found.error());
    co_return std::vector<Credential>{
        Credential{std::nullopt, std::string(found->first->name()), "Runs `" + command_line(found->second) + "`", true}};
}

Awaitable<std::expected<std::optional<std::string>, std::string>> Backend::sign_out(ProviderKey key,
                                                                                    std::optional<std::int64_t> credential) const {
    auto inner = inner_;
    const bool omp = key.harness == OMP;
    if (omp && credential) {
        auto remaining = co_await inner->rpc->logout(key.provider, *credential);
        if (!remaining) co_return std::unexpected(remaining.error().to_string());
        co_return std::move(*remaining);
    }
    if (!omp && !credential) {
        auto found = inner->logout_command(key);
        if (!found) co_return std::unexpected(found.error());
        const auto& [harness, command] = *found;
        const auto process = runtime::exec::command(command.program, command.args, command.env, command.current_dir);
        if (auto ran = runtime::exec::run_checked(process); !ran) {
            co_return std::unexpected("`" + command_line(command) + "` failed: " + ran.error());
        }
        const auto state = co_await harness->account->sign_in_state();
        if (const auto* signed_in = std::get_if<contracts::SignedIn>(&state)) co_return method_name(signed_in->method);
        co_return std::nullopt;
    }
    co_return std::unexpected(key.provider + " has no such stored sign in");
}

Awaitable<std::expected<Catalog, std::string>> Backend::catalog() const {
    auto inner = inner_;
    auto files = co_await this->files();
    if (!files) co_return std::unexpected(files.error());
    auto config = cfg::OmpConfig::load(files->config());
    if (!config) co_return std::unexpected(config.error().to_string());
    auto available = co_await inner->rpc->get_available_models();
    if (!available) co_return std::unexpected(available.error().to_string());
    if (inner->providers_now().empty()) (void)co_await inner->refresh_login_providers();
    co_return make_catalog(*available, *config, inner->provider_names());
}

Awaitable<std::expected<void, std::string>> Backend::assign_role(std::string role, ModelRef model) const {
    auto inner = inner_;
    auto files = co_await this->files();
    if (!files) co_return std::unexpected(files.error());
    auto config = cfg::OmpConfig::load(files->config());
    if (!config) co_return std::unexpected(config.error().to_string());
    auto current = config->model_role(role);
    if (!current) co_return std::unexpected(current.error().to_string());
    std::optional<std::string> thinking;
    if (*current && !(*current)->candidates.empty()) thinking = (*current)->candidates.front().thinking();
    const Selector selector = Selector::model(model.selector(), std::move(thinking));
    if (auto set = config->set_model_role(role, std::span<const Selector>(&selector, 1)); !set) {
        co_return std::unexpected(set.error().to_string());
    }
    if (auto saved = co_await config->save_validated(*inner->validator); !saved) co_return std::unexpected(saved.error().to_string());
    co_await inner->loaded_lock.lock();
    Unlock unlock{&inner->loaded_lock};
    if (inner->loaded) {
        // The screen's files are now stale; a save must reload first.
        auto reloaded = cfg::OmpConfig::load(files->config());
        if (!reloaded) co_return std::unexpected(reloaded.error().to_string());
        inner->loaded->config = std::move(*reloaded);
    }
    co_return std::expected<void, std::string>{};
}

Awaitable<std::expected<SettingsLoad, std::string>> Backend::load_settings() const {
    auto inner = inner_;
    auto files = co_await this->files();
    if (!files) co_return std::unexpected(files.error());
    auto config = cfg::OmpConfig::load(files->config());
    if (!config) co_return std::unexpected(config.error().to_string());
    auto models = ModelsYml::load(files->models());
    if (!models) co_return std::unexpected(models.error().to_string());
    auto listed = co_await inner->rpc->get_available_models();
    std::vector<rpc::ModelInfo> available = listed ? std::move(*listed) : std::vector<rpc::ModelInfo>{};
    if (inner->providers_now().empty()) (void)co_await inner->refresh_login_providers();
    auto settings = st::settings(*config, *models, available, inner->provider_names());
    if (!settings) co_return std::unexpected(settings.error());
    settings->harnesses = inner->harness_models();
    std::vector<std::string> known_models;
    for (const auto& m : available) known_models.push_back(m.selector());
    co_await inner->loaded_lock.lock();
    Unlock unlock{&inner->loaded_lock};
    inner->loaded = Loaded{std::move(*config), std::move(*models), *settings, std::move(available)};
    co_return SettingsLoad{std::move(*settings), std::move(known_models)};
}

Awaitable<std::expected<ModelSettings, std::vector<ValidationError>>> Backend::save_settings(ModelSettings settings) const {
    auto inner = inner_;
    co_await inner->loaded_lock.lock();
    Unlock unlock{&inner->loaded_lock};
    if (!inner->loaded) co_return std::unexpected(fail("Model settings", "the settings were not loaded"));
    auto& loaded = *inner->loaded;
    auto config = loaded.config;
    auto models = loaded.models;
    auto edited = st::apply_settings(config, models, loaded.settings, settings);
    if (!edited) co_return std::unexpected(std::move(edited.error()));

    struct Edit {
        const Harness* harness;
        const HarnessModel* before;
        const HarnessModel* after;
    };
    std::vector<Edit> harness_edits;
    for (const auto& after : settings.harnesses) {
        const auto before = std::ranges::find_if(loaded.settings.harnesses, [&](const HarnessModel& b) { return b.harness == after.harness; });
        if (before == loaded.settings.harnesses.end()) continue;
        const auto* harness = inner->harness(after.harness);
        if (!harness || *before == after) continue;
        harness_edits.push_back({harness, &*before, &after});
    }
    std::vector<ValidationError> harness_errors;
    for (const auto& e : harness_edits) {
        if (auto checked = e.harness->check(*e.after); !checked) {
            harness_errors.push_back({std::string(e.harness->name()), std::move(checked.error())});
        }
    }
    if (!harness_errors.empty()) co_return std::unexpected(std::move(harness_errors));

    const auto& validator = *inner->validator;
    if (edited->config) {
        if (auto ok = co_await config.validate(validator); !ok) co_return std::unexpected(fail("config.yml", ok.error().to_string()));
    }
    if (edited->models) {
        if (auto ok = co_await models.validate(validator); !ok) co_return std::unexpected(fail("models.yml", ok.error().to_string()));
    }
    if (edited->config) {
        if (auto ok = config.save(); !ok) co_return std::unexpected(fail("config.yml", ok.error().to_string()));
    }
    if (edited->models) {
        if (auto ok = models.save(); !ok) co_return std::unexpected(fail("models.yml", ok.error().to_string()));
    }
    for (const auto& e : harness_edits) {
        if (auto ok = e.harness->write(*e.before, *e.after); !ok) co_return std::unexpected(fail(e.harness->name(), std::move(ok.error())));
    }
    auto saved = st::settings(config, models, loaded.available, inner->provider_names());
    if (!saved) co_return std::unexpected(fail("Model settings", std::move(saved.error())));
    saved->harnesses = inner->harness_models();
    loaded.config = std::move(config);
    loaded.models = std::move(models);
    loaded.settings = *saved;
    co_return std::move(*saved);
}

} // namespace loa::models
