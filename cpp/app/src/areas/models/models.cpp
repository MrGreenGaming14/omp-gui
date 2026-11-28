#include "areas/models/models.hpp"

#include <algorithm>
#include <array>
#include <tuple>

#include <asio/as_tuple.hpp>
#include <QHBoxLayout>
#include <QVBoxLayout>
#include <QWidget>

#include "areas/models/session.hpp"
#include "areas/sessions/sessions.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/view/settings/models.hpp"
#include "loa/core/schema.hpp"
#include "loa/models/login_updates.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/session/transcript/state.hpp"
#include "loa/ui/settings/models/view.hpp"

namespace loa::app::areas::models {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

namespace conv = loa::app::view::settings;
namespace theme = loa::ui::kit::theme;
using contracts::HarnessModel;
using loa::settings::models::selector::ModelInfo;
using loa::view::session::composer::ModelOption;
using loa::view::session::composer::PickerAction;

// Rows under the composer's model choices, by `PickerAction` id.
struct ActionRow {
    std::string_view id;
    std::string_view label;
    Page page;
};
constexpr std::array<ActionRow, 3> ACTIONS{{
    {"all", "All models…", Page::Model},
    {"settings", "Model settings…", Page::Settings},
    {"sign-in", "Sign in…", Page::SignIn},
}};

// Effort levels offered for omp before a session runs and lists its own.
constexpr std::array<std::string_view, 4> PRE_SESSION_EFFORTS{"low", "medium", "high", "xhigh"};

int px(float v) { return static_cast<int>(v + 0.5F); }

// Whether `harness` has model roles, read through the omp models backend.
bool has_roles(const Ctx& ctx, std::string_view harness) { return ctx.runtime.harnesses().traits(harness).has_roles; }

// The harness model changes go to: the active session's, else the one new sessions start on.
std::string harness_id(const Ctx& ctx) {
    const auto& sessions = ctx.runtime.sessions();
    if (const auto* active = sessions.active())
        if (auto id = active->harness_id()) return std::string(*id);
    if (sessions.harness) return *sessions.harness;
    return std::string(ctx.runtime.harnesses().roles_id());
}

std::optional<std::string> session_key(const app_runtime::Session* session) {
    return session ? std::optional<std::string>(session->id().str()) : std::nullopt;
}

// The picker's name for a harness model: its `provider/id` selector when the id is one, else
// the provider and the bare id.
ModelRef model_ref(const HarnessModel& model) {
    auto parsed = ModelRef::parse(model.id);
    if (parsed && parsed->provider == model.provider) return *parsed;
    return ModelRef{model.provider, model.id};
}

ModelInfo model_info(const HarnessModel& model, const ModelRef& reference) { return ModelInfo{reference, model.name, {}}; }

// Records on `session` the role its model was picked through, or that it no longer follows one.
std::expected<void, std::string> record_role(const app_runtime::Session& session, std::optional<std::string> role) {
    const auto& writer = session.writer();
    auto draft = writer->with_dom([&](const core::Dom& dom) {
        return role_draft(dom, role ? std::optional<std::string_view>(*role) : std::nullopt);
    });
    if (!draft) return {};
    auto committed = writer->commit(std::move(*draft));
    if (!committed) return std::unexpected(core::to_string(committed.error()));
    return {};
}

std::string adapter_error(const contracts::AdapterError& error) { return error.message(); }

// The first model and thinking level of omp model role `role`, read from `config.yml` alone.
contracts::Awaitable<std::expected<std::pair<std::string, std::optional<std::string>>, std::string>> resolve_role(
    loa::models::Backend backend, std::string role) {
    auto files = co_await backend.files();
    if (!files) co_return std::unexpected(files.error());
    auto roles = sessions::model_roles(files->config());
    if (!roles) co_return std::unexpected(roles.error());
    const auto found = std::ranges::find_if(*roles, [&](const auto& entry) { return entry.first == role; });
    if (found == roles->end()) co_return std::unexpected("omp has no model role " + role);
    if (found->second.empty()) co_return std::unexpected("No model for model role " + role);
    const auto& candidate = found->second.front();
    auto selector = loa::settings::models::selector::Selector::parse(candidate);
    if (!selector) co_return std::unexpected(selector.error().to_string());
    auto model = selector->model_name();
    if (!model) co_return std::unexpected("The role alias " + candidate + " can't be used as a model");
    co_return std::pair{std::string(*model), selector->thinking()};
}

// Reports each progress update of a login until it ends.
loa::runtime::Task login_updates(std::shared_ptr<loa::models::LoginUpdates> updates, shown::ProviderKey key, Task::Emit send) {
    for (;;) {
        auto [error, update] = co_await updates->channel.async_receive(asio::as_tuple(asio::use_awaitable));
        if (error) co_return;
        send(State::message(msg::Login{key, std::move(update)}));
    }
}

Task show_page(State& s, Page page, Ctx& ctx) { return s.show(page, ctx); }

const std::array<AreaCommand<State>, 3> COMMAND_TABLE{
    AreaCommand<State>{"models.model", "Switch model…", "Models", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return show_page(s, Page::Model, ctx); }},
    AreaCommand<State>{"models.settings", "Model settings", "Models", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return show_page(s, Page::Settings, ctx); }},
    AreaCommand<State>{"models.sign-in", "Sign in…", "Models", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return show_page(s, Page::SignIn, ctx); }},
};

} // namespace

Failure Failure::from(const contracts::AdapterError& error) {
    if (error.get<contracts::AdapterError::Unsupported>()) return {Kind::Unsupported, error.message()};
    return {Kind::Error, error.message()};
}

std::span<const AreaCommand<State>> State::COMMANDS() { return COMMAND_TABLE; }

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    return {std::make_unique<State>(ctx.backends.models), Task::none()};
}

Task State::read_providers() const {
    return perform([backend = backend] { return backend.providers(); },
                   [](std::vector<loa::settings::models::login::ProviderEntry> providers) -> loa::app::Message {
                       return message(msg::Providers{std::move(providers)});
                   });
}

Task State::read_roles() const {
    return perform([backend = backend] { return backend.catalog(); },
                   [](std::expected<Catalog, std::string> result) -> loa::app::Message { return message(msg::CatalogRead{std::move(result)}); });
}

Task State::show(Page page, Ctx& ctx) {
    if (page == Page::Workflows) {
        // The workflows area draws that page with its own screen.
        ctx.request(request::Forward{area_message(area_id::WORKFLOWS, to::workflows::Open{})});
        return Task::none();
    }
    page_ = page;
    ctx.show_screen(ID);
    switch (page) {
    case Page::SignIn:
        if (!std::holds_alternative<screen::signin::State>(signin_)) signin_ = Loading{};
        return read_providers();
    case Page::Model: return load_picker(ctx);
    case Page::Workflows: return Task::none();
    case Page::Settings:
        // Unsaved edits stay when the page is shown again.
        if (const auto* state = std::get_if<screen::settings::State>(&settings_); state && state->is_dirty()) return Task::none();
        settings_ = Loading{};
        return perform([backend = backend] { return backend.load_settings(); },
                       [](std::expected<loa::models::SettingsLoad, std::string> result) -> loa::app::Message {
                           return message(msg::SettingsLoaded{std::move(result)});
                       });
    }
    return Task::none();
}

Task State::load_picker(Ctx& ctx) {
    const auto harness = harness_id(ctx);
    picker_ = {harness, Loading{}};
    if (has_roles(ctx, harness)) return read_roles();
    if (const auto it = listed_.find(harness); it != listed_.end() && !it->second.empty()) {
        open_picker(ctx);
        return Task::none();
    }
    const auto* active = ctx.runtime.sessions().active();
    if (active && active->harness()) {
        return perform([session = active->harness()] { return session->models(); },
                       [harness](contracts::Result<std::vector<HarnessModel>> result) -> loa::app::Message {
                           std::expected<std::vector<HarnessModel>, std::string> out;
                           if (result) out = std::move(*result);
                           else out = std::unexpected(adapter_error(result.error()));
                           return message(msg::Listed{harness, std::move(out)});
                       });
    }
    picker_.second = Failed{std::string(ctx.runtime.harnesses().label(harness)) + " lists its models once the session runs"};
    return Task::none();
}

void State::open_picker(const Ctx& ctx) {
    const auto& harness = picker_.first;
    Catalog catalog;
    std::map<ModelRef, std::string> ids;
    if (has_roles(ctx, harness)) {
        const auto it = listed_.find(ctx.runtime.harnesses().roles_id());
        if (it == listed_.end()) return;
        for (const auto& model : it->second) {
            auto reference = ModelRef::parse(model.id);
            catalog.models.push_back(model_info(model, reference ? *reference : model_ref(model)));
        }
        catalog.roles = roles_.value_or(std::vector<RoleAssignment>{});
        for (const auto& info : catalog.models) ids.insert_or_assign(info.model, info.model.selector());
    } else {
        const auto it = listed_.find(harness);
        if (it == listed_.end() || it->second.empty()) return;
        for (const auto& model : it->second) {
            ids.insert_or_assign(model_ref(model), model.id);
            catalog.models.push_back(model_info(model, model_ref(model)));
        }
    }
    std::optional<ModelRef> current;
    if (auto id = this->current(ctx).model) {
        const auto found = std::ranges::find_if(ids, [&](const auto& entry) { return entry.second == *id; });
        current = found != ids.end() ? std::optional<ModelRef>(found->first) : ModelRef::parse(*id);
    }
    auto [state, focus] = screen::picker::State::open(conv::catalog_to_view(catalog), shown::picker::target::Session{},
                                                      current ? std::optional(conv::model_ref_to_view(*current)) : std::nullopt);
    (void)focus;
    picker_.second = OpenPicker{std::move(state), std::move(ids)};
}

shown::SessionModel State::current(const Ctx& ctx) const {
    const auto* session = ctx.runtime.sessions().active();
    if (!session) {
        if (const auto it = pending_.find(std::nullopt); it != pending_.end() && it->second.model)
            return {it->second.model->first, it->second.model->second};
        // A new omp session runs on the default role's model.
        if (has_roles(ctx, harness_id(ctx)) && roles_)
            for (const auto& role : *roles_)
                if (role.role == "default") return {role.model.selector(), std::nullopt};
        return {};
    }
    auto recorded = session_model(session->dom());
    shown::SessionModel current = recorded ? recorded->second : shown::SessionModel{};
    if (const auto it = pending_.find(session_key(session)); it != pending_.end() && it->second.model)
        current = {it->second.model->first, it->second.model->second};
    return current;
}

Pending& State::pending_for(std::optional<std::string> key, const std::string& harness) const {
    auto [it, inserted] = pending_.try_emplace(std::move(key));
    if (inserted) it->second.harness = harness;
    return it->second;
}

Task State::choose(std::string model, std::optional<std::string> role, Ctx& ctx) {
    const auto harness = harness_id(ctx);
    const auto* session = ctx.runtime.sessions().active();
    if (!session) {
        ctx.notify("The next session starts on " + model);
        pending_.insert_or_assign(std::nullopt, Pending{harness, std::pair{std::move(model), std::move(role)}, std::nullopt});
        return Task::none();
    }
    const auto id = session->id();
    auto running = session->harness();
    if (!running) {
        ctx.notify("This session continues on " + model + " when it resumes");
        pending_for(id.str(), harness).model = std::pair{std::move(model), std::move(role)};
        sync(ctx);
        return Task::none();
    }
    return perform([running, model] { return running->set_model(contracts::ModelSelection{model, std::nullopt}); },
                   [id, model, role](contracts::Result<> result) -> loa::app::Message {
                       std::expected<void, Failure> out;
                       if (!result) out = std::unexpected(Failure::from(result.error()));
                       return message(msg::ModelSet{id, model, role, std::move(out)});
                   });
}

Task State::set_effort(std::string effort, Ctx& ctx) {
    const auto harness = harness_id(ctx);
    const auto* session = ctx.runtime.sessions().active();
    if (!session) {
        pending_for(std::nullopt, harness).effort = std::move(effort);
        sync(ctx);
        return Task::none();
    }
    const auto id = session->id();
    auto running = session->harness();
    if (!running) {
        ctx.notify("This session continues with " + effort + " effort when it resumes");
        pending_for(id.str(), harness).effort = std::move(effort);
        sync(ctx);
        return Task::none();
    }
    return perform([running, effort] { return running->set_effort(effort); },
                   [id, harness, effort](contracts::Result<> result) -> loa::app::Message {
                       std::expected<void, Failure> out;
                       if (!result) out = std::unexpected(Failure::from(result.error()));
                       return message(msg::EffortSet{id, harness, effort, std::move(out)});
                   });
}

Task State::list_models(const Ctx& ctx) const {
    const auto* session = ctx.runtime.sessions().active();
    if (!session) return Task::none();
    auto harness = session->harness_id();
    auto running = session->harness();
    if (!harness || !running || listed_.contains(*harness)) return Task::none();
    return perform([running] { return running->models(); },
                   [harness = std::string(*harness)](contracts::Result<std::vector<HarnessModel>> result) -> loa::app::Message {
                       std::expected<std::vector<HarnessModel>, std::string> out;
                       if (result) out = std::move(*result);
                       else out = std::unexpected(adapter_error(result.error()));
                       return message(msg::Listed{harness, std::move(out)});
                   });
}

void State::sync(Ctx& ctx) const {
    const auto current = this->current(ctx);
    const auto* session = ctx.runtime.sessions().active();
    const auto harness = harness_id(ctx);
    std::optional<std::string> effort;
    if (const auto it = pending_.find(session_key(session)); it != pending_.end()) effort = it->second.effort;
    if (!effort && session)
        if (const auto* root = session->dom().root())
            if (auto value = root->str_attr(core::attr::EFFORT)) effort = std::string(*value);
    const bool roles = has_roles(ctx, harness);
    static const std::vector<RoleAssignment> none;
    const auto& assigned = roles && roles_ ? *roles_ : none;
    auto role_of = [&](std::string_view id) -> std::optional<std::string> {
        for (const auto& role : assigned)
            if (role.model.selector() == id) return role.role;
        return std::nullopt;
    };
    static const std::vector<HarnessModel> empty;
    const auto it = listed_.find(harness);
    const auto& listed = it == listed_.end() ? empty : it->second;
    std::vector<ModelOption> options;
    for (const auto& model : listed) {
        const bool shown = listed.size() <= COMPOSER_MODELS || current.model == model.id || role_of(model.id).has_value();
        if (shown) options.push_back(ModelOption{model.id, model.name, model.provider, role_of(model.id)});
    }
    if (current.model) {
        const auto& id = *current.model;
        const auto found = std::ranges::find_if(options, [&](const ModelOption& option) { return option.id == id; });
        if (found != options.end()) {
            if (current.role) found->role = current.role;
        } else {
            const auto slash = id.find('/');
            const std::string provider = slash == std::string::npos ? std::string() : id.substr(0, slash);
            const std::string name = slash == std::string::npos ? id : id.substr(slash + 1);
            options.push_back(ModelOption{id, name, provider, current.role});
        }
    }
    std::vector<std::string> efforts;
    if (current.model)
        if (const auto model = std::ranges::find_if(listed, [&](const HarnessModel& m) { return m.id == *current.model; });
            model != listed.end())
            efforts = model->efforts;
    // omp's catalog doesn't say which levels a model takes; before a session runs, offer omp's
    // levels.
    if (efforts.empty() && !session && roles && current.model)
        for (auto level : PRE_SESSION_EFFORTS) efforts.emplace_back(level);
    if (effort && std::ranges::find(efforts, *effort) == efforts.end()) efforts.push_back(*effort);
    std::vector<PickerAction> actions;
    if (session)
        for (const auto& action : ACTIONS) actions.push_back(PickerAction{std::string(action.id), std::string(action.label)});
    ctx.composer.set_models(std::move(options), current.model ? std::optional<std::string_view>(*current.model) : std::nullopt);
    ctx.composer.set_efforts(std::move(efforts), std::move(effort));
    ctx.composer.set_model_actions(std::move(actions));
}

Task State::signin_event(shown::signin::Intent intent, Ctx& ctx) {
    namespace in = shown::signin::intent;
    return std::visit(
        overloaded{
            [&](in::SignIn& e) {
                auto provider = conv::provider_key_from_view(e.key);
                return Task::job([backend = backend, provider = std::move(provider), key = std::move(e.key)](
                                     Task::Emit send, loa::runtime::Executor& executor) mutable {
                    auto updates = backend.login(executor.context().get_executor(), std::move(provider));
                    executor.spawn(login_updates(std::move(updates), std::move(key), std::move(send)));
                });
            },
            [&](in::OpenUrl& e) {
                if (auto opened = backend.open_url(e.url); !opened)
                    ctx.notify("The browser did not open (" + opened.error() + "); copy the URL instead");
                return Task::none();
            },
            [&](in::CopyUrl& e) {
                namespace chat = loa::ui::session::transcript;
                return Task::chat(chat::Task::of(chat::op::WriteClipboard{std::move(e.url)}));
            },
            [&](in::Cancel& e) {
                backend.cancel(conv::provider_key_from_view(std::move(e.key)));
                return Task::none();
            },
            [&](in::SubmitInput& e) {
                backend.submit_input(conv::provider_key_from_view(std::move(e.key)), std::move(e.text));
                return Task::none();
            },
            [&](in::SignOut& e) {
                return perform([backend = backend, provider = conv::provider_key_from_view(e.key)] { return backend.credentials(provider); },
                               [key = e.key](std::expected<std::vector<loa::settings::models::login::Credential>, std::string> result)
                                   -> loa::app::Message { return message(msg::Credentials{key, std::move(result)}); });
            },
            [&](in::RemoveCredential& e) {
                return perform(
                    [backend = backend, provider = conv::provider_key_from_view(e.key), credential = e.credential] {
                        return backend.sign_out(provider, credential);
                    },
                    [key = e.key](std::expected<std::optional<std::string>, std::string> result) -> loa::app::Message {
                        return message(msg::SignedOut{key, std::move(result)});
                    });
            },
        },
        intent);
}

Task State::pick_role(std::string role, Ctx& ctx) {
    const auto* active = ctx.runtime.sessions().active();
    if (active && active->harness()) {
        ctx.request(request::Forward{loa::app::msg::RunCommand{"sessions.role." + role}});
        return Task::none();
    }
    const auto& active_id = ctx.runtime.sessions().active_id();
    std::optional<std::string> session = active_id ? std::optional<std::string>(active_id->str()) : std::nullopt;
    return perform([backend = backend, role] { return resolve_role(backend, role); },
                   [session = std::move(session), harness = harness_id(ctx),
                    role](std::expected<std::pair<std::string, std::optional<std::string>>, std::string> result) -> loa::app::Message {
                       return message(msg::PendingRole{session, harness, role, std::move(result)});
                   });
}

Task State::picker_event(shown::picker::Intent intent, Ctx& ctx) {
    namespace in = shown::picker::intent;
    return std::visit(overloaded{
                          [&](in::SetSessionModel& e) {
                              if (e.change.role) {
                                  ctx.close_screen();
                                  return pick_role(*e.change.role, ctx);
                              }
                              const auto* picker = std::get_if<OpenPicker>(&picker_.second);
                              if (!picker) return Task::none();
                              const auto found = picker->ids.find(conv::model_ref_from_view(e.change.model));
                              std::string model = found != picker->ids.end() ? found->second : e.change.model.selector();
                              ctx.close_screen();
                              return choose(std::move(model), std::move(e.change.role), ctx);
                          },
                          [&](in::SetRoleModel& e) {
                              return perform([backend = backend, role = std::move(e.role), model = conv::model_ref_from_view(std::move(e.model))] {
                                  return backend.assign_role(role, model);
                              },
                                             [](std::expected<void, std::string> result) -> loa::app::Message {
                                                 return message(msg::RoleAssigned{std::move(result)});
                                             });
                          },
                          [&](in::Close&) {
                              ctx.close_screen();
                              return Task::none();
                          },
                      },
                      intent);
}

Task State::refresh_models(Ctx& ctx) {
    listed_.clear();
    roles_.reset();
    sync(ctx);
    auto listed = list_models(ctx);
    if (!has_roles(ctx, harness_id(ctx))) return listed;
    std::vector<Task> tasks;
    tasks.push_back(std::move(listed));
    tasks.push_back(read_roles());
    return Task::batch(std::move(tasks));
}

Task State::update_typed(Message message, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](msg::Model& m) { return choose(std::move(m.model.id), std::move(m.model.role), ctx); },
            [&](msg::PickRole& m) { return pick_role(std::move(m.role), ctx); },
            [&](msg::PendingRole& m) {
                if (m.result) {
                    auto& [model, effort] = *m.result;
                    pending_.insert_or_assign(m.session, Pending{std::move(m.harness), std::pair{std::move(model), std::optional(std::move(m.role))},
                                                                 std::move(effort)});
                    sync(ctx);
                } else {
                    ctx.notify(std::move(m.result.error()));
                }
                return Task::none();
            },
            [&](msg::Effort& m) { return set_effort(std::move(m.effort), ctx); },
            [&](msg::Action& m) {
                for (const auto& action : ACTIONS)
                    if (action.id == m.id) return show(action.page, ctx);
                return Task::none();
            },
            [&](msg::Show& m) { return show(m.page, ctx); },
            [&](msg::Close&) {
                ctx.close_screen();
                return Task::none();
            },
            [&](msg::Listed& m) {
                auto models = m.result ? std::move(*m.result) : std::vector<HarnessModel>{};
                const bool read_roles = has_roles(ctx, m.harness) && !models.empty() && !roles_;
                // omp's own list (with efforts) replaces the catalog's.
                if (!models.empty() || !listed_.contains(m.harness)) listed_.insert_or_assign(m.harness, std::move(models));
                if (picker_.first == m.harness && std::holds_alternative<Loading>(picker_.second)) {
                    if (const auto it = listed_.find(m.harness); it != listed_.end() && !it->second.empty())
                        open_picker(ctx);
                    else
                        picker_.second = Failed{std::string(ctx.runtime.harnesses().label(m.harness)) +
                                                " doesn't list the models it can switch to"};
                }
                sync(ctx);
                return read_roles ? this->read_roles() : Task::none();
            },
            [&](msg::CatalogRead& m) {
                const std::string roles_id(ctx.runtime.harnesses().roles_id());
                const bool waiting = picker_.first == roles_id && std::holds_alternative<Loading>(picker_.second);
                if (m.result) {
                    roles_ = std::move(m.result->roles);
                    auto& listed = listed_[roles_id];
                    if (listed.empty())
                        for (const auto& info : m.result->models)
                            listed.push_back(HarnessModel{info.model.selector(), info.display_name(), info.model.provider, {}});
                    if (waiting) open_picker(ctx);
                } else if (waiting) {
                    picker_.second = Failed{"Can't read omp's models: " + m.result.error()};
                }
                sync(ctx);
                return Task::none();
            },
            [&](msg::ModelSet& m) {
                if (m.result) {
                    if (const auto* session = ctx.runtime.sessions().get(m.session.as_str()))
                        if (auto recorded = record_role(*session, m.role); !recorded)
                            ctx.notify("Switched to " + m.model + ", but the role wasn't recorded: " + recorded.error());
                } else if (m.result.error().kind == Failure::Kind::Unsupported) {
                    std::string harness(ctx.runtime.harnesses().roles_id());
                    if (const auto* session = ctx.runtime.sessions().get(m.session.as_str()))
                        if (auto id = session->harness_id()) harness = std::string(*id);
                    ctx.notify(m.result.error().text + "; new sessions start on " + m.model);
                    pending_.insert_or_assign(std::nullopt, Pending{std::move(harness), std::pair{m.model, m.role}, std::nullopt});
                } else {
                    ctx.notify("Can't switch to " + m.model + ": " + m.result.error().text);
                }
                sync(ctx);
                return Task::none();
            },
            [&](msg::EffortSet& m) {
                if (!m.result) {
                    if (m.result.error().kind == Failure::Kind::Unsupported) {
                        ctx.notify(m.result.error().text + "; new sessions start with " + m.effort + " effort");
                        pending_for(std::nullopt, m.harness).effort = m.effort;
                    } else {
                        ctx.notify("Can't set " + m.effort + " effort: " + m.result.error().text);
                    }
                }
                sync(ctx);
                return Task::none();
            },
            [&](msg::Providers& m) {
                std::vector<shown::ProviderEntry> providers;
                for (const auto& provider : m.providers) providers.push_back(conv::provider_to_view(provider));
                if (auto* state = std::get_if<screen::signin::State>(&signin_)) state->set_providers(std::move(providers));
                else signin_ = screen::signin::State(std::move(providers));
                return Task::none();
            },
            [&](msg::SignIn& m) {
                auto* state = std::get_if<screen::signin::State>(&signin_);
                if (!state) return Task::none();
                auto intent = state->update(std::move(m.message));
                return intent ? signin_event(std::move(*intent), ctx) : Task::none();
            },
            [&](msg::Login& m) {
                const bool succeeded = std::holds_alternative<contracts::login::Succeeded>(m.update);
                if (auto* state = std::get_if<screen::signin::State>(&signin_))
                    state->login_update(m.key, conv::login_update_to_view(std::move(m.update)));
                if (!succeeded) return Task::none();
                // The new sign in may bring models and change the rows.
                std::vector<Task> tasks;
                tasks.push_back(read_providers());
                tasks.push_back(refresh_models(ctx));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Credentials& m) {
                if (auto* state = std::get_if<screen::signin::State>(&signin_)) {
                    std::expected<std::vector<shown::Credential>, std::string> credentials;
                    if (m.result) {
                        credentials.emplace();
                        for (const auto& credential : *m.result) credentials->push_back(conv::credential_to_view(credential));
                    } else {
                        credentials = std::unexpected(m.result.error());
                    }
                    state->credentials(m.key, std::move(credentials));
                }
                return Task::none();
            },
            [&](msg::SignedOut& m) {
                const bool removed = m.result.has_value();
                if (auto* state = std::get_if<screen::signin::State>(&signin_)) state->signed_out(m.key, std::move(m.result));
                // The rows show what the harness still signs in with; a removed sign in may take
                // models.
                auto providers = read_providers();
                if (!removed) return providers;
                std::vector<Task> tasks;
                tasks.push_back(std::move(providers));
                tasks.push_back(refresh_models(ctx));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Picker& m) {
                auto* picker = std::get_if<OpenPicker>(&picker_.second);
                if (!picker) return Task::none();
                auto intent = picker->state.update(std::move(m.message));
                return intent ? picker_event(std::move(*intent), ctx) : Task::none();
            },
            [&](msg::RoleAssigned& m) {
                if (m.result) return refresh_models(ctx);
                ctx.notify("Can't assign the role: " + m.result.error());
                return Task::none();
            },
            [&](msg::SettingsLoaded& m) {
                if (m.result) settings_.emplace<screen::settings::State>(conv::settings_to_view(m.result->settings), m.result->known_models);
                else settings_ = Failed{"Can't read omp's model settings: " + m.result.error()};
                return Task::none();
            },
            [&](msg::Settings& m) {
                auto* state = std::get_if<screen::settings::State>(&settings_);
                if (!state) return Task::none();
                auto intent = state->update(std::move(m.message));
                if (!intent) return Task::none();
                auto* save = std::get_if<shown::settings::intent::Save>(&*intent);
                if (!save) return Task::none();
                return perform([backend = backend, settings = conv::settings_from_view(std::move(save->settings))] {
                    return backend.save_settings(settings);
                },
                               [](std::expected<loa::settings::models::settings::ModelSettings,
                                                std::vector<loa::settings::models::settings::ValidationError>>
                                      result) -> loa::app::Message { return State::message(msg::Saved{std::move(result)}); });
            },
            [&](msg::Saved& m) {
                auto* state = std::get_if<screen::settings::State>(&settings_);
                if (!state) return Task::none();
                if (m.result) {
                    state->saved(conv::settings_to_view(*m.result));
                    ctx.notify("Model settings saved");
                    return refresh_models(ctx);
                }
                std::vector<shown::ValidationError> errors;
                for (const auto& error : m.result.error()) errors.push_back(conv::validation_error_to_view(error));
                state->set_errors(std::move(errors));
                return Task::none();
            },
        },
        message);
}

void State::contribute(contracts::SessionConfig& config, const Target& target, const Ctx&) const {
    const std::optional<std::string> key = target.resume ? std::optional<std::string>(config.session_id) : std::nullopt;
    const auto it = pending_.find(key);
    if (it == pending_.end() || it->second.harness != target.harness) return;
    auto chosen = std::move(it->second);
    pending_.erase(it);
    if (chosen.model) {
        config.model = std::move(chosen.model->first);
        config.model_role = std::move(chosen.model->second);
    }
    if (chosen.effort) config.effort = std::move(chosen.effort);
}

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    namespace ev = app_runtime::session_event;
    const auto& sessions = ctx.runtime.sessions();
    const bool due = std::visit(overloaded{
                                    [](const ev::Activated&) { return true; },
                                    [&](const ev::Loaded& e) { return sessions.is_active(e.id.as_str()); },
                                    // The model, role and effort are attributes of `<session>`.
                                    [&](const ev::Patch& e) {
                                        if (!sessions.is_active(e.id.as_str()) || std::holds_alternative<core::op::Append>(e.patch->op))
                                            return false;
                                        const auto* session = sessions.get(e.id.as_str());
                                        const auto* root = session ? session->dom().root() : nullptr;
                                        return root && root->id == e.patch->node;
                                    },
                                    [](const auto&) { return false; },
                                },
                                event);
    if (!due) return Task::none();
    sync(ctx);
    return list_models(ctx);
}

QWidget* State::page_body(Page page, const Emit& send) const {
    switch (page) {
    case Page::SignIn: {
        if (const auto* failed = std::get_if<Failed>(&signin_)) return screen::status(failed->error);
        const auto* state = std::get_if<screen::signin::State>(&signin_);
        if (!state) return screen::status("Reading sign in status…");
        auto* view = new screen::SignInView;
        view->render(*state);
        QObject::connect(view, &screen::SignInView::message, view,
                         [send](const screen::signin::Message& m) { send(message(msg::SignIn{m})); });
        return view;
    }
    case Page::Model: {
        if (const auto* failed = std::get_if<Failed>(&picker_.second)) return screen::status(failed->error);
        const auto* picker = std::get_if<OpenPicker>(&picker_.second);
        if (!picker) return screen::status("Reading models…");
        auto* view = new screen::PickerView;
        view->render(picker->state);
        view->apply(picker->state.scroll());
        QObject::connect(view, &screen::PickerView::message, view,
                         [send](const screen::picker::Message& m) { send(message(msg::Picker{m})); });
        auto* center = new QWidget;
        auto* row = new QHBoxLayout(center);
        const int pad = px(theme::sp(theme::SpaceRole::Xl));
        row->setContentsMargins(0, pad, 0, pad);
        row->addStretch(1);
        row->addWidget(view, 0, Qt::AlignTop);
        row->addStretch(1);
        return center;
    }
    case Page::Settings: {
        if (const auto* failed = std::get_if<Failed>(&settings_)) return screen::status(failed->error);
        const auto* state = std::get_if<screen::settings::State>(&settings_);
        if (!state) return screen::status("Reading omp's model settings…");
        auto* view = new screen::SettingsView;
        view->render(*state);
        QObject::connect(view, &screen::SettingsView::message, view,
                         [send](const screen::settings::Message& m) { send(message(msg::Settings{m})); });
        return view;
    }
    case Page::Workflows: return screen::status("Reading workflows…");
    }
    return nullptr;
}

QWidget* State::view_screen(const Ctx&, const Emit& send) const {
    auto* frame = screen::frame(static_cast<screen::Page>(page_), page_body(page_, send),
                                [send](screen::Page page) { send(message(msg::Show{static_cast<Page>(page)})); },
                                [send] { send(message(msg::Close{})); });
    frame->setAccessibleName(QStringLiteral("models"));
    return frame;
}

} // namespace loa::app::areas::models
