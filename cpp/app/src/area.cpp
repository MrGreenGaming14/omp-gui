#include "loa/app/area.hpp"

#include <algorithm>
#include <cctype>

#include "loa/app/services.hpp"

namespace loa::app {

Ctx::Ctx(Services services_, app_runtime::Runtime runtime_, Backends backends_, ui::shell::layout::State shell_,
         ui::session::composer::State composer_, ui::shell::Keymap keymap_)
    : services(std::move(services_)),
      runtime(std::move(runtime_)),
      backends(std::move(backends_)),
      shell(std::move(shell_)),
      composer(std::move(composer_)),
      keymap(std::move(keymap_)) {}

std::optional<settings::app::Config> Ctx::read_config(std::string_view what) {
    auto config = services.storage.config.load();
    if (config) return std::move(*config);
    notify("Can't read the " + std::string(what) + ": " + describe(config.error()));
    return std::nullopt;
}

void Ctx::set_app_tool(loa::view::panel::ToolInfo info) {
    auto it = std::ranges::find_if(app_tools, [&](const auto& tool) { return tool.name == info.name; });
    if (it != app_tools.end()) {
        if (*it == info) return;
        *it = std::move(info);
    } else {
        app_tools.push_back(std::move(info));
    }
    ++app_tools_rev_;
}

void Ctx::show_harness() {
    auto& sessions = runtime.sessions_mut();
    if (const auto* active = sessions.active())
        if (auto id = active->harness_id()) sessions.harness = std::string(*id);
    const auto& current = runtime.sessions().harness;
    composer.set_harnesses(harness::options(runtime.harnesses()),
                           current ? std::optional<std::string_view>(*current) : std::nullopt);
}

void Ctx::replace_session(std::string_view id, std::optional<SessionId> successor, bool delete_) {
    if (runtime.sessions().get(id) != nullptr) successors_.insert_or_assign(SessionId(id), std::move(successor));
    end_session(id, delete_);
}

void Ctx::end_session(std::string_view id, bool delete_) {
    const bool open = runtime.sessions().get(id) != nullptr;
    const bool was_active = runtime.sessions().is_active(id);
    auto transition = runtime.dispatch(app_runtime::command::Close{SessionId(id), delete_});
    std::erase_if(transition.events, [&](app_runtime::RuntimeEvent& event) {
        if (auto* notice = std::get_if<app_runtime::runtime_event::Notice>(&event)) {
            notice_ = notice->text;
            return true;
        }
        return false;
    });
    transitions_.push_back(std::move(transition));
    if (!open) return;
    if (delete_) shell.sidebar_mut().remove_session(id);
    if (was_active) {
        shell.sidebar_mut().set_active(std::nullopt);
        shell.set_session_status(std::nullopt);
    }
}

std::vector<ui::shell::palette::Command> Area::commands(const Ctx&) const { return {}; }
std::optional<Task> Area::run(std::string_view, Ctx&) { return std::nullopt; }
std::optional<Task> Area::slash(std::string_view, Ctx&) { return std::nullopt; }
std::vector<loa::view::session::composer::Command> Area::slash_rows() const { return {}; }
QWidget* Area::view_tab(std::string_view, const Ctx&, const Emit&) const { return nullptr; }
QWidget* Area::view_screen(const Ctx&, const Emit&) const { return nullptr; }
void Area::contribute(contracts::SessionConfig&, const Target&, const Ctx&) const {}
std::optional<HostToolSet> Area::host_tools(const contracts::SessionConfig&, const Target&, const Ctx&) const {
    return std::nullopt;
}
void Area::before_send(const SessionId&, contracts::Prompt&, Ctx&) {}
Task Area::on_session_event(const SessionEvent&, Ctx&) { return Task::none(); }
Task Area::project_activated(std::string_view, const std::filesystem::path&, Ctx&) { return Task::none(); }

std::optional<std::pair<std::string_view, std::string_view>> split_slash(std::string_view text) {
    auto is_space = [](char c) { return std::isspace(static_cast<unsigned char>(c)) != 0; };
    while (!text.empty() && is_space(text.front())) text.remove_prefix(1);
    while (!text.empty() && is_space(text.back())) text.remove_suffix(1);
    if (!text.starts_with('/')) return std::nullopt;
    auto rest = text.substr(1);
    auto split = std::ranges::find_if(rest, is_space);
    auto name = rest.substr(0, static_cast<std::size_t>(split - rest.begin()));
    auto args = rest.substr(name.size());
    while (!args.empty() && is_space(args.front())) args.remove_prefix(1);
    return std::pair{name, args};
}

bool has_active_session(const Ctx& ctx) { return ctx.runtime.sessions().active() != nullptr; }

} // namespace loa::app
