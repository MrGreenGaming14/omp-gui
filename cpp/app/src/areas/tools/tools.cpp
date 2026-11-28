#include "areas/tools/tools.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <set>

#include <QClipboard>
#include <QGuiApplication>
#include <asio.hpp>

#include "areas/tools/listing.hpp"
#include "loa/app/adapters/ssh_terminal.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/view/panel.hpp"
#include "loa/app/watch.hpp"
#include "loa/core/schema.hpp"
#include "loa/mcp/bridge.hpp"
#include "loa/mcp/central.hpp"
#include "loa/mcp/host_tools.hpp"
#include "loa/mcp/tool_filter.hpp"
#include "loa/remote/config.hpp"
#include "loa/remote/hosts.hpp"
#include "loa/remote/tools.hpp"
#include "loa/ui/panel/tools_view.hpp"
#include "loa/ui/terminal/runner.hpp"
#include "loa/ui/terminal/view.hpp"

namespace loa::app::areas::tools {

namespace {

namespace fs = std::filesystem;
namespace bridge = mcp::bridge;
namespace central = mcp::central;
namespace ti = vp::tools_intent;
using settings::app::ProjectSettings;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

// How often the feeds look for MCP events and whether they should stop.
constexpr std::chrono::milliseconds FEED_POLL{100};
// The section of the palette's commands.
constexpr std::string_view SECTION = "Tools";
constexpr std::string_view WAITING = "Remote host question waiting: ";

loa::app::Message wrap(tools::Message m) { return State::message(std::move(m)); }

// The running executable, which serves the omp bridge as `<exe> omp-bridge`.
std::optional<fs::path> current_exe() {
#if defined(__linux__)
    std::error_code ec;
    auto exe = fs::read_symlink("/proc/self/exe", ec);
    if (ec) return std::nullopt;
    return exe;
#else
    return std::nullopt;
#endif
}

// The switched off tools of a project as panel ids.
std::set<vp::ToolId> disabled(const ProjectSettings& settings) {
    std::set<vp::ToolId> ids;
    for (const auto& name : settings.disabled_tools) ids.insert(vp::ToolId::builtin(name));
    for (const auto& [server, names] : settings.disabled_mcp_tools)
        for (const auto& tool : names) ids.insert(vp::ToolId::mcp(server, tool));
    return ids;
}

template <class T>
std::expected<T, std::string> text(mcp::Result<T> result) {
    if (result) return std::move(*result);
    return std::unexpected(result.error().to_string());
}

// Hands MCP server events to the app until stopped; bursts become one refresh.
loa::runtime::Task feed_mcp(std::shared_ptr<mcp::Manager> manager, Task::Emit send, std::stop_token stop) {
    auto events = manager->events();
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        bool changed = false;
        while (events.recv(std::chrono::milliseconds{0})) changed = true;
        if (changed) send(wrap(msg::ServersChanged{}));
        timer.expires_after(FEED_POLL);
        co_await timer.async_wait(asio::use_awaitable);
    }
}

// Hands every change of the remote question queue to the app until stopped.
loa::runtime::Task feed_prompts(std::shared_ptr<remote::PromptQueue> queue, Task::Emit send, std::stop_token stop) {
    auto subscription = queue->subscribe([send](std::uint64_t) { send(wrap(msg::PromptsChanged{})); });
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        timer.expires_after(FEED_POLL);
        co_await timer.async_wait(asio::use_awaitable);
    }
}

} // namespace

bool is_remote_decision(const core::Dom& dom, core::NodeId decision) {
    const auto* node = dom.get(decision);
    if (!node || node->kind != core::ElementKind::Decision) return false;
    const auto id = node->str_attr(core::attr::REQUEST_ID);
    return id && id->starts_with(REQUEST_PREFIX);
}

State::State(Ctx& ctx, Remote remote, SshPaths ssh)
    : mcp_(ctx.services.node.mcp),
      remote_(std::move(remote)),
      ssh_(std::move(ssh)),
      // No active project: toggles apply to the empty project key, the defaults.
      project_(ctx.runtime.project().value_or("")),
      terminal_(terminal::Metrics{}, terminal::LocalShell::login_shell(ctx.runtime.cwd())) {
    terminal_.with_remote(std::make_shared<adapters::SshPty>(remote_.service));
}

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto ssh = SshPaths::from_env();
    auto [remote, warning] = Remote::make(ctx.services.storage.dirs.data, remote::RemoteServiceConfig::from_home(ssh.home));
    if (warning) ctx.notify(*warning);
    auto state = std::make_unique<State>(ctx, std::move(remote), std::move(ssh));
    state->reload_hosts(ctx);
    state->refresh_projects(ctx);
    auto harness = state->refresh_builtin(ctx);
    state->refresh_servers(ctx);
    auto store = std::make_shared<central::ConfigServers>(ctx.services.storage.config);
    auto open = blocking(
        [store, manager = *state->mcp_]() -> std::expected<std::shared_ptr<Servers>, std::string> {
            auto opened = Servers::open(store, manager);
            if (!opened) return std::unexpected(opened.error().to_string());
            return std::shared_ptr<Servers>(std::move(*opened));
        },
        [](std::expected<std::shared_ptr<Servers>, std::string> result) { return wrap(msg::ServersOpened{std::move(result)}); });
    auto serve = Task::job([tools = *state->remote_.tools](Task::Emit send, loa::runtime::Executor& executor) mutable {
        auto started = remote::RemoteHttp::start(std::move(tools), executor);
        std::expected<std::shared_ptr<remote::RemoteHttp>, std::string> result =
            started ? std::expected<std::shared_ptr<remote::RemoteHttp>, std::string>(std::make_shared<remote::RemoteHttp>(std::move(*started)))
                    : std::unexpected(std::move(started.error()));
        send(wrap(msg::RemoteServing{std::move(result)}));
    });
    std::vector<Task> tasks;
    tasks.push_back(std::move(open));
    tasks.push_back(std::move(serve));
    tasks.push_back(std::move(harness));
    return {std::move(state), Task::batch(std::move(tasks))};
}

std::span<const AreaCommand<State>> State::COMMANDS() {
    using C = AreaCommand<State>;
    static const std::array<C, 6> commands{
        C{"tools.servers", "MCP servers", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view, Ctx& ctx) { return state.show(panel_tools::Section::Servers, ctx); }},
        C{"tools.add-server", "Add MCP server", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view, Ctx& ctx) {
              auto task = state.show(panel_tools::Section::Servers, ctx);
              (void)state.panel_.update(panel_tools::msg::FormOpen{panel_tools::FormId::Server});
              return task;
          }},
        C{"tools.import-servers", "Import MCP servers from .claude, .cursor, .codex", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view, Ctx& ctx) {
              std::vector<Task> tasks;
              tasks.push_back(state.show(panel_tools::Section::Servers, ctx));
              auto [_, event] = state.panel_.update(panel_tools::msg::ImportScan{});
              if (event) tasks.push_back(state.panel_event(std::move(*event), ctx));
              return Task::batch(std::move(tasks));
          }},
        C{"tools.hosts", "Remote hosts", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view, Ctx& ctx) { return state.show(panel_tools::Section::Hosts, ctx); }},
        C{"tools.import-hosts", "Import Remote hosts from ~/.ssh/config", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view, Ctx& ctx) {
              std::vector<Task> tasks;
              tasks.push_back(state.show(panel_tools::Section::Hosts, ctx));
              tasks.push_back(state.panel_event(ti::ImportSshHosts{}, ctx));
              return Task::batch(std::move(tasks));
          }},
        C{"tools.shell.", "", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view host, Ctx& ctx) { return state.open_shell(std::string(host), ctx); }}
            .with_listed(Listed<State>::each_([](const State&, const Ctx& ctx) {
                std::vector<std::pair<std::string, std::string>> rows;
                auto config = ctx.services.storage.config.load();
                if (!config) return rows;
                for (const auto& [host, _] : config->remote_hosts) rows.emplace_back(host, "Open a shell on " + host);
                return rows;
            })),
    };
    return commands;
}

// ---- reading the services ------------------------------------------------------------------

ProjectSettings State::settings(std::string_view project, const Ctx& ctx) {
    return ctx.services.storage.config.load().value_or(settings::app::Config{}).project(project);
}

std::optional<std::string> State::harness(const Ctx& ctx) {
    const auto& sessions = ctx.runtime.sessions();
    if (const auto* active = sessions.active())
        if (auto id = active->harness_id()) return std::string(*id);
    return sessions.harness;
}

std::optional<fs::path> State::omp(const Ctx& ctx) {
    if (!ctx.runtime.harnesses().runnable(OMP)) return std::nullopt;
    const auto* harness = ctx.runtime.harnesses().get(OMP);
    if (harness && harness->executable) return *harness->executable;
    return fs::path(OMP);
}

Task State::refresh_builtin(const Ctx& ctx) {
    auto id = harness(ctx);
    shown_ = {id, ctx.app_tools_rev()};
    std::string label = id ? std::string(ctx.runtime.harnesses().label(*id)) : std::string();
    std::vector<vp::ToolInfo> harness_tools;
    std::optional<std::string> note;
    std::optional<fs::path> load;
    if (!id) {
        note = "No harness is available";
    } else if (*id == OMP) {
        if (auto it = harness_tools_.find(OMP); it != harness_tools_.end()) {
            if (it->second) harness_tools = *it->second;
            else note = "Can't read omp's tools: " + it->second.error();
        } else if (auto program = omp(ctx)) {
            note = "Reading omp's tools\u2026";
            load = std::move(program);
        } else {
            note = "omp isn't installed here";
        }
    } else {
        note = label + " doesn't report its built in tools to the app";
    }
    panel_.set_builtin(vp::BuiltinTools{label, std::move(harness_tools), ctx.app_tools, std::move(note)});
    if (!load) return Task::none();
    if (std::exchange(omp_loading_, true)) return Task::none();
    auto config = bridge::OmpBridgeConfig::make(ctx.runtime.cwd(), ctx.services.storage.dirs.data / "omp-bridge" / "tools");
    config.program = std::move(*load);
    return perform([config = std::move(config)]() { return bridge::list_omp_tools(config); },
                   [](mcp::Result<std::vector<contracts::HostToolSpec>> specs) {
                       std::expected<std::vector<vp::ToolInfo>, std::string> result;
                       if (specs) {
                           std::vector<vp::ToolInfo> tools;
                           for (const auto& spec : *specs)
                               tools.push_back({spec.name, listing::short_description(spec.description), {}});
                           result = std::move(tools);
                       } else {
                           result = std::unexpected(specs.error().to_string());
                       }
                       return wrap(msg::HarnessTools{std::string(OMP), std::move(result)});
                   });
}

Task State::follow_builtin(const Ctx& ctx) {
    if (shown_.first != harness(ctx) || shown_.second != ctx.app_tools_rev()) return refresh_builtin(ctx);
    return Task::none();
}

vp::McpServer State::bridge_row(const Ctx& ctx) const {
    const std::vector<vp::ToolInfo>* described = nullptr;
    if (auto it = harness_tools_.find(OMP); it != harness_tools_.end() && it->second) described = &*it->second;
    std::vector<vp::ToolInfo> tools;
    for (auto name : bridge::DEFAULT_TOOLS) {
        std::string description = "omp's `" + std::string(name) + "` tool";
        if (described)
            if (auto tool = std::ranges::find(*described, name, &vp::ToolInfo::name); tool != described->end())
                description = tool->description;
        tools.push_back({std::string(name), std::move(description), {}});
    }
    vp::ServerStatus status = vp::server_status::Running{};
    if (!omp(ctx)) status = vp::server_status::Error{"omp isn't installed"};
    const auto exe = current_exe();
    contracts::McpStdio transport{exe ? exe->string() : std::string("loa-app"), {"omp-bridge"}, {}};
    return listing::app_server(bridge::SERVER_NAME, std::move(transport), std::move(status), std::move(tools));
}

vp::McpServer State::remote_row() const {
    vp::ServerStatus status = vp::server_status::Starting{};
    std::string url;
    if (remote_.http) {
        status = vp::server_status::Running{};
        url = remote_.http->url();
    } else if (remote_.http_error) {
        status = vp::server_status::Error{*remote_.http_error};
    }
    std::vector<vp::ToolInfo> tools;
    for (const auto& spec : remote_.tools->specs()) tools.push_back({spec.name, listing::short_description(spec.description), {}});
    return listing::app_server(remote::SERVER_NAME, contracts::McpHttp{std::move(url), {}}, std::move(status), std::move(tools));
}

void State::refresh_servers(Ctx& ctx) {
    std::vector<vp::McpServer> servers{remote_row(), bridge_row(ctx)};
    for (const auto& view : mcp_->view(mcp::AllowAll{})) servers.push_back(listing::server(view));
    refresh_peers(servers, ctx);
    panel_.set_servers(std::move(servers));
}

void State::refresh_peers(const std::vector<vp::McpServer>& servers, Ctx& ctx) {
    auto issued = ctx.services.node.grants->issued();
    if (!issued) {
        panel_.set_peers({});
        ctx.notify("Can't read the grants: " + issued.error().to_string());
        return;
    }
    std::vector<std::string> hosts;
    if (auto config = ctx.services.storage.config.load())
        for (const auto& [alias, _] : config->remote_hosts) hosts.push_back(alias);
    panel_.set_peers(listing::peer_tools(*issued, servers, hosts, core::now_ms()));
}

void State::refresh_projects(const Ctx& ctx) {
    const auto config = ctx.services.storage.config.load().value_or(settings::app::Config{});
    std::set<std::string> ids;
    for (const auto& [id, _] : config.projects) ids.insert(id);
    if (const auto& active = ctx.runtime.project()) ids.insert(*active);
    ids.insert(project_);
    ids.erase("");
    std::vector<vp::ProjectRef> projects;
    for (const auto& id : ids) projects.push_back({id, id});
    panel_.set_projects(std::move(projects));
    panel_.select_project(project_);
    const auto it = config.projects.find(project_);
    panel_.set_disabled_tools(project_, disabled(it != config.projects.end() ? it->second : ProjectSettings{}));
}

void State::reload_hosts(Ctx& ctx) {
    auto config = ctx.read_config("Remote hosts");
    if (!config) return;
    const auto& entries = config->remote_hosts;
    auto [hosts, failed] = remote::load(entries, *ssh_.resolver);
    remote_.service->hosts()->replace_all(std::move(hosts));
    std::vector<vp::RemoteHost> shown;
    for (const auto& [alias, entry] : entries) shown.push_back(app::view::host_to_view(alias, entry));
    panel_.set_hosts(std::move(shown));
    if (!failed.empty()) {
        std::string list;
        for (const auto& [name, error] : failed) list += (list.empty() ? "" : "; ") + name + ": " + error.message();
        ctx.notify("Some Remote hosts can't be reached as configured: " + list);
    }
}

void State::edit_hosts(Ctx& ctx,
                       const std::function<std::expected<std::optional<std::string>, std::string>(
                           std::map<std::string, settings::app::RemoteHost>&)>& change) {
    auto edited = ctx.services.storage.config.edit([&](settings::app::Config& config) { return change(config.remote_hosts); });
    if (!edited) {
        ctx.notify("Can't save the Remote hosts: " + edited.error().to_string());
    } else if (!*edited) {
        ctx.notify("Can't save the Remote hosts: " + edited->error());
    } else {
        if (**edited) ctx.notify(***edited);
        reload_hosts(ctx);
    }
    refresh_servers(ctx);
}

void State::set_tools(const std::string& project, const std::vector<vp::ToolId>& ids, bool enabled, Ctx& ctx) {
    ctx.edit_config("tool toggles", [&](settings::app::Config& config) {
        auto& settings = config.projects[project];
        for (const auto& id : ids) {
            std::visit(overloaded{
                           [&](const vp::tool_id::Builtin& b) {
                               if (enabled) settings.disabled_tools.erase(b.name);
                               else settings.disabled_tools.insert(b.name);
                           },
                           [&](const vp::tool_id::Mcp& m) {
                               auto& off = settings.disabled_mcp_tools[m.server];
                               if (enabled) off.erase(m.tool);
                               else off.insert(m.tool);
                               if (off.empty()) settings.disabled_mcp_tools.erase(m.server);
                           },
                       },
                       id.value);
        }
    });
    // The panel shows what is stored, also after a failed save.
    panel_.set_disabled_tools(project, disabled(settings(project, ctx)));
}

std::shared_ptr<Servers> State::servers_or_notice(Ctx& ctx) const {
    if (!servers_) ctx.notify("The MCP servers are still loading");
    return servers_;
}

Task State::show(panel_tools::Section section, Ctx& ctx) {
    (void)panel_.update(panel_tools::msg::SectionSelected{section});
    return open(ctx);
}

Task State::open(Ctx& ctx) {
    ctx.open_tab(std::string(TAB));
    auto task = refresh_builtin(ctx);
    refresh_servers(ctx);
    refresh_projects(ctx);
    return task;
}

Task State::open_shell(std::string host, Ctx& ctx) {
    auto [_, task] = terminal_.open(terminal::Remote{std::move(host)});
    ctx.open_tab(std::string(SHELL_TAB));
    return terminal_task(std::move(task));
}

Task State::terminal_task(terminal::Task task) {
    terminal::Task pty;
    std::vector<Task> out;
    for (auto& op : task.ops) {
        if (const auto* write = std::get_if<terminal::op::WriteClipboard>(&op)) {
            if (auto* clipboard = qApp ? QGuiApplication::clipboard() : nullptr;
                clipboard && (!write->primary || clipboard->supportsSelection()))
                clipboard->setText(QString::fromStdString(write->text), write->primary ? QClipboard::Selection : QClipboard::Clipboard);
        } else if (const auto* read = std::get_if<terminal::op::ReadClipboard>(&op)) {
            std::optional<std::string> text;
            if (auto* clipboard = qApp ? QGuiApplication::clipboard() : nullptr) {
                const QString value = clipboard->text(QClipboard::Clipboard);
                if (!value.isNull()) text = value.toStdString();
            }
            out.push_back(Task::done(wrap(msg::Terminal{terminal::msg::Paste{read->tab, std::move(text)}})));
        } else {
            pty.ops.push_back(std::move(op));
        }
    }
    // Each job runs its operations on a runner of its own: the PTY loops it starts hold the
    // job's `send` only while they run.
    if (!pty.empty())
        out.push_back(Task::job([backend = terminal_.backend(), pty = std::move(pty)](Task::Emit send,
                                                                                      loa::runtime::Executor& executor) {
            const terminal::Runner runner(executor.context().get_executor(),
                                          [send](terminal::Message m) { send(wrap(msg::Terminal{std::move(m)})); });
            runner.run(pty, backend);
        }));
    return Task::batch(std::move(out));
}

Task State::panel_event(vp::ToolsIntent event, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](ti::ProjectSelected& e) {
                project_ = std::move(e.project);
                refresh_projects(ctx);
                return Task::none();
            },
            [&](ti::SetToolsEnabled& e) {
                set_tools(e.project, e.tools, e.enabled, ctx);
                return Task::none();
            },
            [&](ti::Server& e) {
                auto servers = servers_or_notice(ctx);
                if (!servers) return Task::none();
                return blocking(
                    [servers, name = std::move(e.name), action = e.action]() -> std::expected<std::optional<std::string>, std::string> {
                        mcp::Result<> result;
                        switch (action) {
                        case vp::ServerAction::Start: result = servers->set_enabled(name, true); break;
                        case vp::ServerAction::Stop: result = servers->set_enabled(name, false); break;
                        case vp::ServerAction::Restart: result = servers->manager().restart(name); break;
                        case vp::ServerAction::Remove: result = servers->remove(name); break;
                        }
                        if (!result) return std::unexpected(name + ": " + result.error().to_string());
                        return std::nullopt;
                    },
                    [](std::expected<std::optional<std::string>, std::string> result) { return wrap(msg::Done{std::move(result)}); });
            },
            [&](ti::AddServer& e) {
                auto servers = servers_or_notice(ctx);
                if (!servers) return Task::none();
                ctx.notify("Connecting to " + e.server.name + "\u2026");
                return blocking(
                    [servers, spec = std::move(e.server)]() -> std::expected<std::optional<std::string>, std::string> {
                        auto added = servers->add(central::from_session_spec(spec));
                        if (!added) return std::unexpected("Can't add " + spec.name + ": " + added.error().to_string());
                        return "Added " + spec.name + " with " + std::to_string(added->size()) + " tools";
                    },
                    [](std::expected<std::optional<std::string>, std::string> result) { return wrap(msg::Done{std::move(result)}); });
            },
            [&](ti::ScanImports&) {
                auto files = std::make_shared<std::vector<std::unique_ptr<mcp::import::ImportSource>>>(
                    mcp::import::defaults(ssh_.home, ctx.runtime.cwd()));
                return blocking(
                    [files] {
                        auto [found, errors] = mcp::import::scan(*files);
                        std::vector<std::string> notes;
                        for (const auto& error : errors) notes.push_back(error.file.string() + ": " + error.message);
                        for (const auto& entry : found)
                            if (!entry.spec) notes.push_back(entry.name + " in " + entry.file.string() + ": " + entry.spec.error());
                        return msg::Scanned{std::move(found), std::move(notes)};
                    },
                    [](msg::Scanned scanned) { return wrap(std::move(scanned)); });
            },
            [&](ti::ImportServers& e) {
                auto servers = servers_or_notice(ctx);
                if (!servers) return Task::none();
                return blocking([servers, picked = listing::picked(found_, e.candidates)]() mutable {
                                    return text(servers->import_found(std::move(picked)));
                                },
                                [](std::expected<mcp::import::ImportReport, std::string> result) {
                                    return wrap(msg::Imported{std::move(result)});
                                });
            },
            [&](ti::ImportSshHosts&) {
                auto resolver = ssh_.resolver;
                edit_hosts(ctx, [&](auto& entries) -> std::expected<std::optional<std::string>, std::string> {
                    const auto added = remote::import_aliases(entries, *resolver);
                    if (added.empty()) return "No new hosts in ~/.ssh/config";
                    return "Imported " + std::to_string(added.size()) + " hosts from ~/.ssh/config, read only until you open their limits";
                });
                return Task::none();
            },
            [&](ti::AddHost& e) {
                auto [alias, entry] = app::view::host_from_view(std::move(e.host));
                edit_hosts(ctx, [&](auto& entries) -> std::expected<std::optional<std::string>, std::string> {
                    entries.insert_or_assign(alias, entry);
                    return std::nullopt;
                });
                return Task::none();
            },
            [&](ti::RemoveHost& e) {
                edit_hosts(ctx, [&](auto& entries) -> std::expected<std::optional<std::string>, std::string> {
                    entries.erase(e.host);
                    return std::nullopt;
                });
                return Task::job([service = remote_.service, alias = std::move(e.host)](Task::Emit, loa::runtime::Executor& executor) {
                    asio::post(executor.context(), [service, alias] { service->disconnect(alias); });
                });
            },
            [&](ti::SaveHostLimits& e) {
                edit_hosts(ctx, [&](auto& entries) -> std::expected<std::optional<std::string>, std::string> {
                    auto it = entries.find(e.host);
                    if (it == entries.end()) return std::unexpected(e.host + " isn't a Remote host");
                    it->second.allowed_paths = e.limits.allowed_paths;
                    it->second.read_only = e.limits.read_only;
                    it->second.commands = e.limits.commands_allowed;
                    it->second.auto_approve = e.limits.auto_approve;
                    return std::nullopt;
                });
                return Task::none();
            },
            [&](ti::OpenTerminal& e) { return open_shell(std::move(e.host), ctx); },
            [&](ti::LoadAudit& e) {
                return blocking([audit = remote_.service->audit(), host = e.host] { return Remote::audit(audit, host); },
                                [host = e.host](std::expected<std::vector<vp::AuditRow>, std::string> result) {
                                    return wrap(msg::Audit{host, std::move(result)});
                                });
            },
            [&](ti::AnswerPrompt& e) {
                if (!remote_.answer_panel(e.id, e.answer, ctx)) ctx.notify("That remote host question is no longer waiting");
                return Task::none();
            },
        },
        event);
}

std::optional<contracts::McpServerSpec> State::bridge_spec(const contracts::SessionConfig& config, const ProjectSettings& settings,
                                                           const Ctx& ctx) {
    auto program = omp(ctx);
    if (!program) return std::nullopt;
    auto exe = current_exe();
    if (!exe) return std::nullopt;
    auto bridge = bridge::OmpBridgeConfig::make(config.cwd, ctx.services.storage.dirs.data / "omp-bridge" / config.session_id);
    bridge.program = std::move(*program);
    std::erase_if(bridge.tools, [&](const std::string& tool) { return !settings.mcp_tool_enabled(bridge::SERVER_NAME, tool); });
    if (bridge.tools.empty()) return std::nullopt;
    static constexpr std::array<std::string_view, 1> PREFIX{"omp-bridge"};
    return bridge.session_spec(*exe, PREFIX);
}

// ---- Area ---------------------------------------------------------------------------------

Task State::update_typed(Message message, Ctx& ctx) {
    auto follow = follow_builtin(ctx);
    auto task = std::visit(
        overloaded{
            [&](msg::Panel& m) {
                // The tools page's updates ask for no widget operations.
                auto event = panel_.update(std::move(m.message)).second;
                if (event) return panel_event(std::move(*event), ctx);
                return Task::none();
            },
            [&](to::tools::Open&) { return open(ctx); },
            [&](to::tools::AnswerDecision& m) {
                auto result = remote_.answer_card(m.session, m.decision, m.answer, ctx);
                if (!result) ctx.notify(result.error());
                ctx.request(request::Forward{
                    area_message(area_id::DECISIONS, to::decisions::Answered{m.session.str(), m.decision, std::move(result)})});
                return Task::none();
            },
            [&](msg::ServersOpened& m) {
                if (m.result) {
                    servers_ = std::move(*m.result);
                    refresh_servers(ctx);
                } else {
                    ctx.notify("Can't load the MCP servers: " + m.result.error());
                }
                return Task::none();
            },
            [&](msg::RemoteServing& m) {
                if (m.result) {
                    remote_.http = std::move(*m.result);
                } else {
                    ctx.notify("The remote server can't serve MCP clients: " + m.result.error());
                    remote_.http_error = m.result.error();
                }
                refresh_servers(ctx);
                return Task::none();
            },
            [&](msg::HarnessTools& m) {
                omp_loading_ = false;
                harness_tools_.insert_or_assign(m.harness, std::move(m.result));
                auto refreshed = refresh_builtin(ctx);
                refresh_servers(ctx);
                return refreshed;
            },
            [&](msg::ServersChanged&) {
                refresh_servers(ctx);
                return Task::none();
            },
            [&](msg::PromptsChanged&) {
                auto [prompts, fresh] = remote_.sync(ctx);
                if (fresh && !prompts.empty()) {
                    ctx.notify(std::string(WAITING) + prompts.back().title);
                    (void)panel_.update(panel_tools::msg::SectionSelected{panel_tools::Section::Hosts});
                    ctx.open_tab(std::string(TAB));
                } else if (prompts.empty() && ctx.notice() && ctx.notice()->starts_with(WAITING)) {
                    // Every question is answered or cancelled: the notice is stale.
                    ctx.clear_notice();
                }
                panel_.set_prompts(std::move(prompts));
                return Task::none();
            },
            [&](msg::Scanned& m) {
                panel_.set_import_candidates(listing::candidates(m.found));
                found_ = std::move(m.found);
                if (!m.errors.empty()) {
                    std::string list;
                    for (const auto& error : m.errors) list += (list.empty() ? "" : "; ") + error;
                    ctx.notify("Some entries can't be imported: " + list);
                }
                return Task::none();
            },
            [&](msg::Imported& m) {
                if (!m.result) {
                    ctx.notify("Can't import the servers: " + m.result.error());
                    return Task::none();
                }
                for (auto& [source, name, outcome] : listing::outcomes(*m.result)) panel_.set_import_outcome(source, name, std::move(outcome));
                refresh_servers(ctx);
                return Task::none();
            },
            [&](msg::Audit& m) {
                if (m.result) panel_.set_audit(std::move(m.host), std::move(*m.result));
                else ctx.notify("Can't read the audit log of " + m.host + ": " + m.result.error());
                return Task::none();
            },
            [&](msg::Done& m) {
                if (!m.result) ctx.notify(m.result.error());
                else if (*m.result) ctx.notify(**m.result);
                refresh_servers(ctx);
                return Task::none();
            },
            [&](msg::Terminal& m) { return terminal_task(terminal_.update(std::move(m.message))); },
        },
        message);
    std::vector<Task> tasks;
    tasks.push_back(std::move(follow));
    tasks.push_back(std::move(task));
    return Task::batch(std::move(tasks));
}

std::vector<PanelTab> State::panel_tabs() const {
    const auto& pane = ui::panel::pane(panel_);
    return {PanelTab{std::string(pane.id), std::string(pane.title), pane.icon, true},
            PanelTab{std::string(SHELL_TAB), "Remote shell", loa::view::theme::Icon::Terminal, false}};
}

QWidget* State::view_tab(std::string_view id, const Ctx&, const Emit& send) const {
    if (id == TAB) {
        auto* view = new ui::panel::ToolsView;
        view->render(panel_);
        QObject::connect(view, &ui::panel::ToolsView::message, view,
                         [send](const panel_tools::Message& m) { send(wrap(msg::Panel{m})); });
        return view;
    }
    if (id == SHELL_TAB) {
        auto* view = new terminal::TerminalView;
        view->render(terminal_);
        QObject::connect(view, &terminal::TerminalView::message, view,
                         [send](const terminal::Message& m) { send(wrap(msg::Terminal{m})); });
        return view;
    }
    return nullptr;
}

void State::contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const {
    const auto* capabilities = target.capabilities;
    // A harness that takes neither MCP servers nor host tools runs without the app's tools
    // (omp2 refuses any tool config).
    if (!capabilities || (!capabilities->mcp_servers && !capabilities->host_tools)) return;
    const auto project = config.project.value_or(ctx.runtime.project().value_or(""));
    const auto settings = State::settings(project, ctx);
    for (auto& name : central::session_disabled_tools(settings))
        if (std::ranges::find(config.disabled_tools, name) == config.disabled_tools.end()) config.disabled_tools.push_back(std::move(name));
    if (!capabilities->mcp_servers) return;
    for (const auto& spec : mcp_->specs())
        if (spec.enabled) config.mcp_servers.push_back(central::session_spec(spec));
    if (remote_.http) config.mcp_servers.push_back(remote_.http->session_spec(config.session_id));
    if (target.harness != OMP)
        if (auto spec = bridge_spec(config, settings, ctx)) config.mcp_servers.push_back(std::move(*spec));
}

std::optional<HostToolSet> State::host_tools(const contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const {
    // Harnesses with MCP support reach the same tools as servers.
    if (!target.capabilities || !target.capabilities->host_tools || target.capabilities->mcp_servers) return std::nullopt;
    const auto project = config.project.value_or(ctx.runtime.project().value_or(""));
    const auto settings = State::settings(project, ctx);
    auto router = std::make_shared<contracts::HostToolRouter>();
    auto mcp = std::make_shared<mcp::host_tools::McpHostTools>(*mcp_, std::make_shared<central::ProjectFilter>(settings));
    for (const auto& spec : mcp->specs()) (void)router->add(spec, mcp);
    for (auto& spec : remote_.tools->specs())
        if (settings.mcp_tool_enabled(remote::SERVER_NAME, spec.name)) (void)router->add(std::move(spec), remote_.tools);
    if (router->empty()) return std::nullopt;
    auto specs = std::vector<contracts::HostToolSpec>(router->specs().begin(), router->specs().end());
    return HostToolSet{std::move(specs), std::move(router)};
}

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    namespace se = app_runtime::session_event;
    if (const auto* loaded = std::get_if<se::Loaded>(&event)) remote_.cancel_stale(loaded->id, ctx);
    if (const auto* closed = std::get_if<se::Closed>(&event)) remote_.forget(closed->id.as_str());
    if (std::holds_alternative<se::Activated>(event) || std::holds_alternative<se::Loaded>(event) ||
        std::holds_alternative<se::Closed>(event))
        return follow_builtin(ctx);
    return Task::none();
}

Task State::project_activated(std::string_view project, const fs::path& cwd, Ctx& ctx) {
    terminal_.set_new_tab(terminal::LocalShell::login_shell(cwd));
    project_ = std::string(project);
    refresh_projects(ctx);
    return Task::none();
}

Subscriptions State::subscription() const {
    Subscriptions subs;
    subs.push_back(subscription::Stream{"tools.mcp", [manager = mcp_](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
                                            executor.spawn(feed_mcp(manager, std::move(send), std::move(stop)));
                                        }});
    subs.push_back(subscription::Stream{ptr_key("tools.prompts", remote_.queue),
                                        [queue = remote_.queue](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
                                            executor.spawn(feed_prompts(queue, std::move(send), std::move(stop)));
                                        }});
    return subs;
}

} // namespace loa::app::areas::tools
