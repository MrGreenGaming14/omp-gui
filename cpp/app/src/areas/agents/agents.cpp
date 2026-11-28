#include "agents.hpp"

#include <algorithm>
#include <array>
#include <set>

#include <QObject>

#include "loa/app/perform.hpp"
#include "loa/app/view/settings/agents.hpp"
#include "loa/mcp/bridge.hpp"
#include "loa/models/service.hpp"
#include "loa/settings/models/selector.hpp"
#include "loa/ui/settings/agents/view.hpp"

namespace loa::app::areas::agents {

namespace {

namespace fs = std::filesystem;
namespace am = settings::models::agents;
namespace vs = loa::view::settings::agents;
using settings::models::Error;
using view::settings::entry_to_view;
using view::settings::field_error_to_view;
using view::settings::fields_from_view;

constexpr std::string_view OMP = "omp";
constexpr std::string_view SECTION = "Agents";
// Palette title of "New agent".
constexpr std::string_view NEW_TITLE = "New agent";

// Where the form's tools come from: omp's own, listed through the bridge, or a fixed list
// (set by tests).
using Tools = std::variant<mcp::bridge::OmpBridgeConfig, std::vector<std::string>>;

std::vector<vs::AgentEntry> entries(const std::vector<am::AgentFile>& files) {
    std::vector<vs::AgentEntry> out;
    out.reserve(files.size());
    for (const auto& file : files) out.push_back(entry_to_view(file));
    return out;
}

std::vector<std::string> default_tools() { return {mcp::bridge::DEFAULT_TOOLS.begin(), mcp::bridge::DEFAULT_TOOLS.end()}; }

std::string fallback_note(std::string_view reason) { return "Showing omp's default tools: " + std::string(reason); }

// omp's `provider/model` ids, then its role aliases (`@default`, `@slow`). Empty when the
// catalog can't be read; the form then offers omp's default.
contracts::Awaitable<std::vector<std::string>> models(models::Backend backend) {
    auto catalog = co_await backend.catalog();
    if (!catalog) co_return std::vector<std::string>{};
    std::set<std::string> ids;
    for (const auto& info : catalog->models) ids.insert(info.model.selector());
    std::set<std::string> roles;
    for (const auto& assignment : catalog->roles) roles.insert("@" + assignment.role);
    std::vector<std::string> out(roles.begin(), roles.end());
    out.insert(out.end(), ids.begin(), ids.end());
    co_return out;
}

// Resolves omp's agent directory, lists the agents and, when `full`, reads the models and tools
// the form offers.
contracts::Awaitable<std::expected<Data, std::string>> read_agents(models::Backend backend, fs::path project, Tools tools, bool full) {
    auto files = co_await backend.files();
    if (!files) co_return std::unexpected("Can't find omp's agent directory: " + files.error());
    Data data;
    data.agent_dir = files->agent_dir;
    auto listed = am::list(data.agent_dir, project);
    data.files = std::move(listed.files);
    for (const auto& [path, error] : listed.broken) data.broken.push_back(path.string() + ": " + error.to_string());
    if (!full) co_return data;
    vs::Choices choices;
    if (auto* fixed = std::get_if<std::vector<std::string>>(&tools)) {
        choices.tools = std::move(*fixed);
    } else {
        auto specs = co_await mcp::bridge::list_omp_tools(std::get<mcp::bridge::OmpBridgeConfig>(std::move(tools)));
        if (specs && !specs->empty()) {
            for (auto& spec : *specs) choices.tools.push_back(std::move(spec.name));
        } else {
            choices.tools = default_tools();
            choices.tools_note = fallback_note(specs ? std::string("omp reported no tools") : specs.error().to_string());
        }
    }
    for (auto level : settings::models::selector::THINKING_LEVELS) choices.thinking_levels.emplace_back(level);
    choices.models = co_await models(std::move(backend));
    data.choices = std::move(choices);
    co_return data;
}

} // namespace

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto state = std::make_unique<State>();
    // The palette lists the agents it can edit.
    Task task = ctx.runtime.harnesses().runnable(OMP) ? state->read(ctx, Load{Load::Kind::Refresh, {}}) : Task::none();
    return {std::move(state), std::move(task)};
}

std::span<const AreaCommand<State>> State::COMMANDS() {
    static const std::array<AreaCommand<State>, 2> commands{
        AreaCommand<State>{"agents.new", NEW_TITLE, SECTION, std::nullopt, Listed<State>::always(),
                           [](State& state, std::string_view, Ctx& ctx) { return state.open(Load{Load::Kind::New, {}}, ctx); }}
            .with_listed(Listed<State>::when_([](const State&, const Ctx& ctx) -> std::optional<std::string_view> {
                if (omp_active(ctx)) return NEW_TITLE;
                return std::nullopt;
            })),
        AreaCommand<State>{"agents.edit.", "", SECTION, std::nullopt, Listed<State>::always(),
                           [](State& state, std::string_view name, Ctx& ctx) {
                               return state.open(Load{Load::Kind::Edit, std::string(name)}, ctx);
                           }}
            .with_listed(Listed<State>::each_([](const State& state, const Ctx& ctx) {
                std::vector<std::pair<std::string, std::string>> rows;
                if (!omp_active(ctx)) return rows;
                for (auto& name : state.names()) rows.emplace_back(name, "Edit agent: " + name);
                return rows;
            })),
    };
    return commands;
}

bool State::omp_active(const Ctx& ctx) {
    const auto* harness = ctx.composer.harness();
    return harness && harness->id == OMP;
}

QWidget* State::draw(bool titled, const Emit& send) const {
    auto* view = new screen::AgentsView(titled);
    view->render(screen_);
    QObject::connect(view, &screen::AgentsView::message, view,
                     [send](const screen::Message& m) { send(message(msg::Screen{m})); });
    return view;
}

QWidget* State::view_screen(const Ctx&, const Emit& send) const { return draw(true, send); }

QWidget* State::configs_body(const Ctx& ctx, const Emit& send) const {
    if (!omp_active(ctx) || !ready_) return nullptr;
    return draw(false, send);
}

Task State::read(const Ctx& ctx, Load load) const {
    const bool full = load.kind != Load::Kind::Refresh;
    Tools tools;
    if (fixed_tools_) {
        tools = *fixed_tools_;
    } else {
        const auto* harness = ctx.runtime.harnesses().get(OMP);
        auto config = mcp::bridge::OmpBridgeConfig::make(ctx.runtime.cwd(), ctx.services.storage.dirs.data / "omp-bridge" / "tools");
        config.program = harness && harness->executable ? *harness->executable : fs::path(OMP);
        tools = std::move(config);
    }
    auto work = [backend = ctx.backends.models, project = ctx.runtime.cwd(), tools = std::move(tools), full]() {
        return read_agents(backend, project, tools, full);
    };
    return perform(std::move(work), [load = std::move(load)](std::expected<Data, std::string> result) {
        return message(msg::Loaded{load, std::move(result)});
    });
}

Task State::open(Load load, Ctx& ctx) {
    if (!omp_active(ctx)) {
        ctx.notify(std::string(OMP_ONLY));
        return Task::none();
    }
    ctx.show_screen(ID);
    return read(ctx, std::move(load));
}

void State::loaded(const Load& load, std::expected<Data, std::string> result, Ctx& ctx) {
    if (!result) {
        if (load.kind != Load::Kind::Refresh) ctx.notify(std::move(result.error()));
        return;
    }
    auto& data = *result;
    agent_dir_ = std::move(data.agent_dir);
    files_ = std::move(data.files);
    screen_.set_agents(entries(files_));
    if (data.choices) {
        tools_ = data.choices->tools;
        screen_.set_choices(std::move(*data.choices));
        ready_ = true;
    }
    switch (load.kind) {
    case Load::Kind::Refresh: return;
    case Load::Kind::Edit: {
        auto found = std::ranges::find_if(files_, [&](const am::AgentFile& file) { return file.definition.name() == load.name; });
        if (found == files_.end()) {
            ctx.notify("No agent named " + load.name);
            if (ctx.screen() == ID) ctx.close_screen();
            return;
        }
        screen_.edit(static_cast<std::size_t>(found - files_.begin()));
        break;
    }
    case Load::Kind::New: screen_.new_agent(); break;
    }
    if (!data.broken.empty()) {
        std::string joined;
        for (const auto& each : data.broken) {
            if (!joined.empty()) joined += "; ";
            joined += each;
        }
        ctx.notify("Some agent files can't be read: " + joined);
    }
}

Task State::update_typed(Message message, Ctx& ctx) {
    return std::visit(
        [&](auto&& m) -> Task {
            using M = std::decay_t<decltype(m)>;
            if constexpr (std::is_same_v<M, to::agents::Open>) {
                return open(Load{Load::Kind::New, {}}, ctx);
            } else if constexpr (std::is_same_v<M, msg::Edit>) {
                return open(Load{Load::Kind::Edit, std::move(m.name)}, ctx);
            } else if constexpr (std::is_same_v<M, msg::Screen>) {
                if (auto intent = screen_.update(std::move(m.message))) screen_intent(std::move(*intent), ctx);
                return Task::none();
            } else {
                loaded(m.load, std::move(m.result), ctx);
                return Task::none();
            }
        },
        std::move(message));
}

void State::screen_intent(vs::Intent intent, Ctx& ctx) {
    std::visit(
        [&](auto&& i) {
            using I = std::decay_t<decltype(i)>;
            if constexpr (std::is_same_v<I, vs::intent::Close>) {
                ctx.close_screen();
            } else if constexpr (std::is_same_v<I, vs::intent::Check>) {
                screen_.set_errors(check(i.draft));
            } else if constexpr (std::is_same_v<I, vs::intent::Save>) {
                auto errors = check(i.draft);
                const bool valid = errors.empty();
                screen_.set_errors(std::move(errors));
                if (valid) save(std::move(i.draft), ctx);
            } else if constexpr (std::is_same_v<I, vs::intent::Replace>) {
                replace(i.draft, i.path, ctx);
            } else if constexpr (std::is_same_v<I, vs::intent::Toggle>) {
                toggle(i.path, ctx);
            } else {
                remove(i.path, ctx);
            }
        },
        std::move(intent));
}

State::Result<am::AgentDefinition> State::definition(const Draft& draft) const {
    const am::AgentFile* base = nullptr;
    if (draft.base) {
        auto found = std::ranges::find_if(files_, [&](const am::AgentFile& file) { return file.path == *draft.base; });
        if (found != files_.end()) base = &*found;
    }
    if (base) return fields_from_view(base->definition, draft.fields);
    auto blank = am::AgentDefinition::create("", "");
    if (!blank) return std::unexpected(std::move(blank.error()));
    return fields_from_view(*blank, draft.fields);
}

std::vector<vs::FieldError> State::check(const Draft& draft) const {
    std::vector<std::string> known_agents;
    for (const auto& file : files_) known_agents.emplace_back(file.definition.name());
    auto def = definition(draft);
    std::vector<vs::FieldError> out;
    if (!def) {
        out.push_back(vs::FieldError{"form", def.error().to_string()});
        return out;
    }
    for (auto& error : am::validate(*def, tools_, known_agents)) out.push_back(field_error_to_view(std::move(error)));
    return out;
}

void State::save(Draft draft, Ctx& ctx) {
    if (!agent_dir_) {
        screen_.set_error("form", "omp's agent directory isn't known yet");
        return;
    }
    auto def = definition(draft);
    if (!def) return show_error(def.error());
    const auto name = def->name();
    std::optional<fs::path> clash;
    for (const auto& file : files_)
        if (file.definition.name() == name && (!draft.editing || file.path != *draft.editing)) {
            clash = file.path;
            break;
        }
    if (clash) {
        screen_.ask_replace(std::move(draft), std::move(*clash));
    } else if (draft.editing) {
        replace_file(*def, *draft.editing, ctx);
    } else {
        saved(*def, am::write(user_agents(), *def, am::Overwrite::No), ctx);
    }
}

void State::replace(const Draft& draft, const fs::path& path, Ctx& ctx) {
    auto def = definition(draft);
    if (!def) return show_error(def.error());
    replace_file(*def, path, ctx);
    // Renaming an agent onto another one: the old file is gone with the rename.
    if (draft.editing && *draft.editing != path && fs::exists(*draft.editing) && screen_.editing() == path)
        if (auto removed = am::remove(*draft.editing); !removed) screen_.set_error("form", removed.error().to_string());
}

fs::path State::user_agents() const { return agent_dir_.value_or(fs::path{}) / "agents"; }

void State::replace_file(const AgentDefinition& definition, const fs::path& path, Ctx& ctx) {
    saved(definition, overwrite(definition, path), ctx);
}

State::Result<fs::path> State::overwrite(const AgentDefinition& definition, const fs::path& path) const {
    const fs::path dir = path.has_parent_path() ? path.parent_path() : user_agents();
    auto found = std::ranges::find_if(files_, [&](const am::AgentFile& file) { return file.path == path; });
    const bool enabled = found == files_.end() || found->enabled;
    auto written = am::write(dir, definition, am::Overwrite::Yes);
    if (!written) return written;
    if (enabled) {
        if (path != *written && fs::exists(path))
            if (auto removed = am::remove(path); !removed) return std::unexpected(std::move(removed.error()));
        return written;
    }
    if (fs::exists(path))
        if (auto removed = am::remove(path); !removed) return std::unexpected(std::move(removed.error()));
    return am::set_enabled(*written, false);
}

void State::saved(const AgentDefinition& definition, Result<fs::path> result, Ctx& ctx) {
    if (!result) return show_error(result.error());
    ctx.notify("Saved " + std::string(definition.name()) +
               ". omp sessions started from now on can use it; sessions already running won't.");
    relist(ctx);
    screen_.edit_path(*result);
}

void State::show_error(const Error& error) {
    std::string_view field = "form";
    if ((error.kind == Error::Kind::Invalid && error.field == "name") || error.kind == Error::Kind::AlreadyExists) field = "name";
    screen_.set_error(field, error.to_string());
}

void State::toggle(const fs::path& path, Ctx& ctx) {
    auto enabled = screen_.is_enabled(path);
    if (!enabled) return;
    const std::string name = screen_.name();
    auto result = am::set_enabled(path, !*enabled);
    if (!result) return show_error(result.error());
    const std::string_view verb = *enabled ? "Disabled" : "Enabled";
    ctx.notify(std::string(verb) + " " + name +
               ". omp sessions started from now on pick up the change; sessions already running won't.");
    relist(ctx);
    screen_.edit_path(*result);
}

void State::remove(const fs::path& path, Ctx& ctx) {
    const std::string name = screen_.name();
    auto result = am::remove(path);
    if (!result) return show_error(result.error());
    ctx.notify("Deleted " + name + ". omp sessions started from now on won't have it; sessions already running keep it.");
    relist(ctx);
    screen_.new_agent();
}

void State::relist(const Ctx& ctx) {
    if (!agent_dir_) return;
    files_ = am::list(*agent_dir_, ctx.runtime.cwd()).files;
    screen_.set_agents(entries(files_));
}

std::vector<std::string> State::names() const {
    std::vector<std::string> out;
    std::set<std::string_view> seen;
    for (const auto& file : files_)
        if (seen.insert(file.definition.name()).second) out.emplace_back(file.definition.name());
    return out;
}

} // namespace loa::app::areas::agents
