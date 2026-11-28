#include "areas/sandbox/sandbox.hpp"

#include <algorithm>
#include <array>
#include <chrono>

#include <QCheckBox>
#include <QHBoxLayout>
#include <QLabel>
#include <QPushButton>
#include <QScrollArea>
#include <QVBoxLayout>
#include <QWidget>
#include <asio.hpp>

#include "loa/app/perform.hpp"
#include "loa/app/watch.hpp"
#include "loa/runtime/text.hpp"
#include "loa/sandbox/isolation.hpp"
#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/panel/view.hpp"

namespace loa::app::areas::sandbox {

namespace {

namespace fs = std::filesystem;
namespace kit = ui::kit;
namespace theme = ui::kit::theme;
namespace widgets = ui::kit::widgets;
namespace run_state = sb::run_state;
using Icon = loa::view::theme::Icon;
using sb::SandboxConfig;
using sb::SandboxKind;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

// How often the feeds look for run events and peer failures, and whether they should stop.
constexpr std::chrono::milliseconds FEED_POLL{100};
constexpr std::string_view SECTION = "Sandbox";
// Characters of a run's task its row shows.
constexpr std::size_t TASK_CHARS = 72;
// The run list's share of the list width, as its greatest height.
constexpr float LIST_HEIGHT_SHARE = 0.6F;

loa::app::Message wrap(sandbox::Message m) { return State::message(std::move(m)); }

int px(float v) { return static_cast<int>(v + 0.5F); }

QVBoxLayout* column(QWidget* w, theme::SpaceRole role) {
    auto* layout = new QVBoxLayout(w);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(px(theme::sp(role)));
    return layout;
}

QHBoxLayout* row(QWidget* w, theme::SpaceRole role) {
    auto* layout = new QHBoxLayout(w);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(px(theme::sp(role)));
    return layout;
}

QLabel* icon(Icon which, theme::TextRole size, kit::Tone tone) {
    auto* label = new QLabel;
    label->setPixmap(kit::tinted(which, size, tone));
    return label;
}

std::string shown(const std::optional<fs::path>& path) { return path ? path->string() : std::string(); }

std::optional<fs::path> path(std::string_view text) {
    constexpr std::string_view SPACE = " \t\r\n";
    const auto start = text.find_first_not_of(SPACE);
    if (start == std::string_view::npos) return std::nullopt;
    return fs::path(std::string(text.substr(start, text.find_last_not_of(SPACE) - start + 1)));
}

// One text setting: its provider, its form field, and how it reads from and writes to the config.
struct Setting {
    SandboxKind kind;
    widgets::Field field;
    std::string (*read)(const SandboxConfig&);
    void (*write)(SandboxConfig&, std::string_view);
};

// The settings form, in provider order. The VM ones show with `LOA_SANDBOX_VM`, and are kept without it.
const std::array<Setting, 6> SETTINGS{
    Setting{SandboxKind::Docker, {"docker.image", "Default image", "debian:stable-slim"}, [](const SandboxConfig& c) { return c.docker_image; },
            [](SandboxConfig& c, std::string_view v) {
                auto trimmed = path(v);
                c.docker_image = trimmed ? trimmed->string() : SandboxConfig{}.docker_image;
            }},
    Setting{SandboxKind::Firecracker, {"firecracker.kernel", "Guest kernel", "/path/to/vmlinux"},
            [](const SandboxConfig& c) { return shown(c.firecracker_kernel); },
            [](SandboxConfig& c, std::string_view v) { c.firecracker_kernel = path(v); }},
    Setting{SandboxKind::Firecracker, {"firecracker.rootfs", "Root filesystem", "/path/to/rootfs.ext4"},
            [](const SandboxConfig& c) { return shown(c.firecracker_rootfs); },
            [](SandboxConfig& c, std::string_view v) { c.firecracker_rootfs = path(v); }},
    Setting{SandboxKind::Vm, {"vm.disk", "Disk image", "/path/to/disk.img"}, [](const SandboxConfig& c) { return shown(c.vm_disk); },
            [](SandboxConfig& c, std::string_view v) { c.vm_disk = path(v); }},
    Setting{SandboxKind::Vm, {"vm.kernel", "Kernel (optional)", "boot from the disk"},
            [](const SandboxConfig& c) { return shown(c.vm_kernel); }, [](SandboxConfig& c, std::string_view v) { c.vm_kernel = path(v); }},
    // Hosts separated by commas or spaces.
    Setting{SandboxKind::Ssh, {"ssh.hosts", "Hosts", "the hosts of ~/.ssh/config"},
            [](const SandboxConfig& c) {
                std::string out;
                for (const auto& host : c.ssh_hosts) out += (out.empty() ? "" : ", ") + host;
                return out;
            },
            [](SandboxConfig& c, std::string_view v) {
                c.ssh_hosts.clear();
                std::size_t start = 0;
                while (start <= v.size()) {
                    const auto end = v.find_first_of(", ", start);
                    const auto host = v.substr(start, end == std::string_view::npos ? std::string_view::npos : end - start);
                    if (!host.empty()) c.ssh_hosts.emplace_back(host);
                    if (end == std::string_view::npos) break;
                    start = end + 1;
                }
            }},
};

// The providers in the order the settings list them.
constexpr std::array<SandboxKind, 6> PROVIDERS{SandboxKind::Local, SandboxKind::Docker, SandboxKind::Firecracker,
                                               SandboxKind::Vm,    SandboxKind::Ssh,    SandboxKind::Peer};

template <class E>
std::string describe(const E& error) {
    if constexpr (requires { error.to_string(); }) return error.to_string();
    else return std::string(error.message());
}

loa::runtime::Task feed_events(std::shared_ptr<sb::SandboxEvents::Receiver> events, Task::Emit send, std::stop_token stop) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        while (auto event = events->try_recv()) send(wrap(msg::Event{std::move(*event)}));
        timer.expires_after(FEED_POLL);
        co_await timer.async_wait(asio::use_awaitable);
    }
}

loa::runtime::Task feed_failures(std::shared_ptr<Failures> failures, Task::Emit send, std::stop_token stop) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        std::deque<adapters::Served> taken;
        {
            std::lock_guard lock(failures->mutex);
            taken.swap(failures->queue);
        }
        for (auto& served : taken) send(wrap(msg::PeerFailed{std::move(served)}));
        timer.expires_after(FEED_POLL);
        co_await timer.async_wait(asio::use_awaitable);
    }
}

} // namespace

std::string Section::get(std::string_view key) const {
    const auto it = values.find(key);
    return it == values.end() ? std::string() : it->second;
}

std::expected<void, std::string> Section::set(std::string_view key, std::string_view value) {
    if (auto it = values.find(key); it != values.end()) it->second = std::string(value);
    return {};
}

Draft Draft::of(const SandboxConfig& config) {
    Draft draft;
    for (auto kind : PROVIDERS) {
        Section section;
        section.kind = kind;
        for (const auto& setting : SETTINGS) {
            if (setting.kind != kind) continue;
            section.field_list.push_back(setting.field);
            section.values.insert_or_assign(std::string(setting.field.key), setting.read(config));
        }
        draft.sections.push_back(std::move(section));
    }
    draft.network = config.docker_network;
    return draft;
}

SandboxConfig Draft::config() const {
    SandboxConfig config;
    config.docker_network = network;
    for (const auto& setting : SETTINGS) {
        const auto section = std::ranges::find(sections, setting.kind, &Section::kind);
        setting.write(config, section != sections.end() ? section->get(setting.field.key) : std::string());
    }
    return config;
}

void Draft::edit(std::string_view key, std::string_view value) {
    for (auto& section : sections) (void)section.set(key, value);
}

std::string_view kind_label(SandboxKind kind) {
    switch (kind) {
    case SandboxKind::Local: return "Local";
    case SandboxKind::Docker: return "Docker";
    case SandboxKind::Firecracker: return "Firecracker";
    case SandboxKind::Vm: return "VM (qemu)";
    case SandboxKind::Ssh: return "SSH host";
    case SandboxKind::Peer: return "Peer";
    }
    return "";
}

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto state = std::make_unique<State>();
    state->settings_ = ctx.services.storage.dirs.data / SETTINGS_FILE;
    auto config = SandboxConfig::load(state->settings_);
    if (!config) {
        ctx.notify("Can't read the sandbox settings " + state->settings_.string() + ": " + config.error().message() + "; using the defaults");
        config = SandboxConfig{};
    }
    auto journal = std::make_shared<adapters::LibraryJournal>(ctx.runtime.library());
    state->service_ = std::make_shared<sb::SandboxService>(std::move(*config), std::move(journal));
    state->service_->with_peers(std::make_shared<adapters::NodeLink>(ctx.node));
    state->events_ = state->service_->subscribe();
    state->served_ = std::make_shared<Failures>();
    adapters::register_(ctx.node, std::make_shared<adapters::ServeStub>(state->service_->stub(), [failures = state->served_](adapters::Served served) {
                            std::lock_guard lock(failures->mutex);
                            failures->queue.push_back(std::move(served));
                        }));
    state->draft_ = Draft::of(state->service_->config());
    state->publish(ctx);
    auto probe = state->probe();
    return {std::move(state), std::move(probe)};
}

std::span<const AreaCommand<State>> State::COMMANDS() {
    using C = AreaCommand<State>;
    static const std::array<C, 2> commands{
        C{"sandbox.refresh", "Recheck sandbox availability", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view, Ctx& ctx) {
              state.show_settings(ctx);
              return state.probe();
          }},
        C{"sandbox.settings", "Sandbox settings", SECTION, std::nullopt, Listed<State>::always(),
          [](State& state, std::string_view, Ctx& ctx) {
              state.show_settings(ctx);
              return Task::none();
          }},
    };
    return commands;
}

void State::show_settings(Ctx& ctx) {
    view_ = View::Settings;
    ctx.open_tab(std::string(TAB));
}

Task State::probe() {
    probing_ = true;
    return perform([service = service_]() { return service->probe(); },
                   [](std::vector<sb::ProviderStatus> statuses) { return wrap(msg::Probed{std::move(statuses)}); });
}

void State::publish(Ctx& ctx) const {
    loa::view::panel::ToolInfo info;
    info.name = std::string(sb::TASK_TOOL);
    info.description = "Runs a task as a worker agent inside a sandbox; its model calls stay on this machine. With isolation it works on a " +
                       std::string(sb::backend_name()) + " checkout and returns a patch.";
    if (statuses_)
        for (const auto& status : *statuses_) {
            loa::view::panel::DetailState state = loa::view::panel::DetailState::Ready;
            switch (status.availability) {
            case sb::Availability::Ready: state = loa::view::panel::DetailState::Ready; break;
            case sb::Availability::NotConfigured: state = loa::view::panel::DetailState::NotConfigured; break;
            case sb::Availability::Unavailable: state = loa::view::panel::DetailState::Unavailable; break;
            }
            info.details.push_back({std::string(kind_label(status.kind)), state, status.detail});
        }
    ctx.set_app_tool(std::move(info));
}

Task State::event(sb::SandboxEvent event, Ctx& ctx) {
    const auto ended = std::visit(overloaded{
                                      [](const sb::event::Started&) -> std::optional<std::uint64_t> { return std::nullopt; },
                                      [](const sb::event::Finished& e) -> std::optional<std::uint64_t> { return e.run; },
                                      [](const sb::event::Failed& e) -> std::optional<std::uint64_t> { return e.run; },
                                  },
                                  event);
    runs_.record(std::move(event));
    const auto* entry = ended ? runs_.get(*ended) : nullptr;
    if (!entry) return Task::none();
    const auto run = entry->run.run;
    const auto worker = entry->run.worker;
    // The worker is done: release its journal unless the user opened it.
    if (!ctx.runtime.sessions().get(worker))
        if (auto closed = ctx.runtime.library()->close(worker); !closed)
            ctx.notify("Can't close the worker session " + worker + ": " + describe(closed.error()));
    if (!entry->pending_patch() || !ctx.runtime.sessions().is_active(entry->run.parent)) return Task::none();
    view_ = View::Runs;
    ctx.open_tab(std::string(TAB));
    return select(run, ctx);
}

Task State::select(std::uint64_t run, Ctx& ctx) {
    selected_ = run;
    std::string_view patch;
    if (const auto* entry = runs_.get(run))
        if (const auto* finished = std::get_if<run_state::Finished>(&entry->state); finished && finished->patch) patch = *finished->patch;
    auto files = diff::parse_unified(patch);
    if (!files) {
        ctx.notify("Can't read the sandbox patch: " + files.error().message());
        files = std::vector<diff::FileDiff>{};
    }
    // The tab's diff view is drawn anew from the state, at its scroll position.
    (void)diff_.set_files(std::move(*files));
    return Task::none();
}

Task State::apply(std::uint64_t run, Ctx& ctx) {
    const auto* entry = runs_.get(run);
    if (!entry) return Task::none();
    const auto patch = entry->pending_patch();
    if (!patch) {
        ctx.notify("This run has no patch left to apply");
        return Task::none();
    }
    applying_ = run;
    return blocking(
        [workspace = entry->run.workspace, patch = std::string(*patch)]() -> std::expected<void, std::string> {
            if (auto applied = sb::apply_patch(workspace, patch); !applied) return std::unexpected(applied.error().message());
            return {};
        },
        [run](std::expected<void, std::string> result) { return wrap(msg::Applied{run, std::move(result)}); });
}

Task State::save(Ctx& ctx) {
    auto config = draft_.config();
    if (auto saved = config.save(settings_); !saved) {
        ctx.notify("Can't save the sandbox settings " + settings_.string() + ": " + saved.error().message());
        return Task::none();
    }
    draft_ = Draft::of(config);
    service_->set_config(std::move(config));
    ctx.notify("Sandbox settings saved; new sessions use them");
    return probe();
}

std::string State::empty_text() const {
    if (!statuses_) return "Checking which sandboxes this machine can run\u2026";
    std::string ready;
    for (const auto& status : *statuses_)
        if (status.availability == sb::Availability::Ready) ready += (ready.empty() ? "" : ", ") + std::string(kind_label(status.kind));
    if (ready.empty())
        return "No sandbox provider can run on this machine, so sessions don't get sandbox_task. The Tools tab and the settings say "
               "what each one needs.";
    return "No sandbox_task runs yet. Sessions on harnesses with host tools can call it; ready here: " + ready + ".";
}

Task State::update_typed(Message message, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](msg::Event& m) { return event(std::move(m.event), ctx); },
            [&](msg::Probed& m) {
                probing_ = false;
                statuses_ = std::move(m.statuses);
                publish(ctx);
                return Task::none();
            },
            [&](msg::Refresh&) { return probe(); },
            [&](msg::Show& m) {
                view_ = m.view;
                return Task::none();
            },
            [&](msg::Select& m) { return select(m.run, ctx); },
            [&](msg::Diff& m) {
                (void)diff_.update(std::move(m.message));
                return Task::none();
            },
            [&](msg::Apply& m) { return apply(m.run, ctx); },
            [&](msg::Applied& m) {
                applying_.reset();
                if (!m.result) {
                    ctx.notify("Can't apply the sandbox patch: " + m.result.error());
                } else {
                    runs_.mark_applied(m.run);
                    if (const auto* entry = runs_.get(m.run))
                        ctx.notify("Applied the sandbox patch to " + entry->run.workspace.string());
                }
                return Task::none();
            },
            [&](msg::OpenWorker& m) {
                ctx.request(request::OpenSession{std::move(m.worker)});
                return Task::none();
            },
            [&](msg::Edit& m) {
                draft_.edit(m.key, m.value);
                return Task::none();
            },
            [&](msg::Network& m) {
                draft_.network = m.on;
                return Task::none();
            },
            [&](msg::Save&) { return save(ctx); },
            [&](msg::PeerFailed& m) {
                ctx.notify("A sandbox run for peer " + m.served.peer + " failed on this device: " + m.served.error);
                return Task::none();
            },
        },
        message);
}

std::vector<PanelTab> State::panel_tabs() const { return {PanelTab{std::string(TAB), "Sandbox", std::nullopt, false}}; }

QWidget* State::view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const {
    if (id != TAB) return nullptr;
    return draw(ctx, send);
}

std::optional<HostToolSet> State::host_tools(const contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const {
    // The worker runs on the session's harness, through its host tools.
    if (!target.capabilities || !target.capabilities->host_tools) return std::nullopt;
    auto adapter = ctx.runtime.harnesses().registry.get(target.harness);
    if (!adapter) return std::nullopt;
    auto tool = service_->host_tool(std::move(adapter), config);
    if (!tool) return std::nullopt;
    return HostToolSet{{std::move(tool->first)}, std::move(tool->second)};
}

Task State::on_session_event(const SessionEvent& event, Ctx&) {
    // The patch shown belongs to the session the user just left.
    const auto* activated = std::get_if<app_runtime::session_event::Activated>(&event);
    if (!activated || !activated->id) return Task::none();
    const auto* entry = selected_ ? runs_.get(*selected_) : nullptr;
    if (!entry || entry->run.parent != activated->id->as_str()) {
        selected_.reset();
        (void)diff_.set_files({});
    }
    return Task::none();
}

Subscriptions State::subscription() const {
    Subscriptions subs;
    subs.push_back(subscription::Stream{ptr_key("sandbox.events", events_),
                                        [events = events_](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
                                            executor.spawn(feed_events(events, std::move(send), std::move(stop)));
                                        }});
    subs.push_back(subscription::Stream{ptr_key("sandbox.served", served_),
                                        [served = served_](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
                                            executor.spawn(feed_failures(served, std::move(send), std::move(stop)));
                                        }});
    return subs;
}

// ---- view ----------------------------------------------------------------------------------

QWidget* State::draw(const Ctx& ctx, const Emit& send) const {
    auto* root = new QWidget;
    root->setObjectName(QStringLiteral("Sandbox"));
    root->setAccessibleName(QStringLiteral("Sandbox"));
    auto* layout = column(root, theme::SpaceRole::Xs);
    auto* header = new QWidget;
    header->setContentsMargins(px(theme::sp(theme::SpaceRole::M)), px(theme::sp(theme::SpaceRole::S)), px(theme::sp(theme::SpaceRole::M)),
                               px(theme::sp(theme::SpaceRole::S)));
    auto* head = row(header, theme::SpaceRole::Xs);
    auto tab = [&](std::string_view label, View view) {
        return widgets::Tab{label, std::nullopt, view_ == view, [send, view] { send(wrap(msg::Show{view})); }, nullptr};
    };
    head->addWidget(widgets::tab_strip({tab("Runs", View::Runs), tab("Settings", View::Settings)}));
    if (const auto running = runs_.running(); running > 0)
        head->addWidget(widgets::badge(std::to_string(running) + " running", kit::Tone::Normal));
    head->addStretch(1);
    layout->addWidget(header);
    layout->addWidget(view_ == View::Runs ? runs_view(ctx, send) : settings_view(send), 1);
    return root;
}

QWidget* State::runs_view(const Ctx& ctx, const Emit& send) const {
    if (runs_.entries().empty()) {
        auto* empty = widgets::label(empty_text(), theme::TextRole::Label, kit::Tone::Muted);
        empty->setWordWrap(true);
        empty->setContentsMargins(px(theme::sp(theme::SpaceRole::M)), px(theme::sp(theme::SpaceRole::Xs)), px(theme::sp(theme::SpaceRole::M)),
                                  px(theme::sp(theme::SpaceRole::Xs)));
        return empty;
    }
    auto* body = new QWidget;
    auto* layout = column(body, theme::SpaceRole::S);
    auto* rows = new QWidget;
    auto* list = column(rows, theme::SpaceRole::Xs);
    list->setContentsMargins(px(theme::sp(theme::SpaceRole::S)), 0, px(theme::sp(theme::SpaceRole::S)), 0);
    const auto entries = runs_.entries();
    for (auto it = entries.rbegin(); it != entries.rend(); ++it) list->addWidget(run_row(*it, ctx, send));
    list->addStretch(1);
    auto* scroll = new QScrollArea;
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setWidget(rows);
    scroll->setMaximumHeight(px(theme::width(theme::LayoutRole::ListWidth) * LIST_HEIGHT_SHARE));
    layout->addWidget(scroll);
    const auto* selected = selected_ ? runs_.get(*selected_) : nullptr;
    if (selected) layout->addWidget(detail(*selected, send));
    if (selected && !diff_.files().empty()) {
        auto* view = new ui::panel::DiffView;
        view->render(diff_);
        QObject::connect(view, &ui::panel::DiffView::message, view,
                         [send](const diff::Message& m) { send(wrap(msg::Diff{m})); });
        layout->addWidget(view, 1);
    } else {
        layout->addStretch(1);
    }
    return body;
}

QWidget* State::run_row(const sb::RunEntry& entry, const Ctx& ctx, const Emit& send) const {
    const auto& run = entry.run;
    Icon which = Icon::Check;
    kit::Tone tone = kit::Tone::Ok;
    std::string summary;
    std::visit(overloaded{
                   [&](const run_state::Running&) {
                       which = Icon::CircleDot;
                       tone = kit::Tone::Accent;
                       summary = "Running\u2026";
                   },
                   [&](const run_state::Failed&) {
                       which = Icon::TriangleAlert;
                       tone = kit::Tone::Danger;
                       summary = "Failed";
                   },
                   [&](const run_state::Finished& f) {
                       if (!f.patch) summary = "Done in the workspace";
                       else if (f.files.empty()) summary = "Done, no changes";
                       else {
                           std::size_t added = 0, removed = 0;
                           for (const auto& file : f.files) added += file.added, removed += file.removed;
                           summary = "+" + std::to_string(added) + " \u2212" + std::to_string(removed) + " \u00b7 " +
                                     std::to_string(f.files.size()) + (f.files.size() == 1 ? " file" : " files") + " \u00b7 " +
                                     (f.applied ? "applied" : "patch");
                       }
                   },
               },
               entry.state);
    std::string session = run.parent;
    if (const auto* open = ctx.runtime.sessions().get(run.parent); open && open->summary()) session = open->summary()->title;
    auto* button = new QPushButton;
    button->setAccessibleName(QString::fromStdString("Sandbox run " + std::to_string(run.run)));
    kit::qt::style_button(*button, kit::ButtonKind::row(selected_ == run.run));
    QObject::connect(button, &QPushButton::clicked, button, [send, id = run.run] { send(wrap(msg::Select{id})); });
    auto* content = column(button, theme::SpaceRole::Xs);
    content->setContentsMargins(px(theme::sp(theme::SpaceRole::S)), px(theme::sp(theme::SpaceRole::Xs)), px(theme::sp(theme::SpaceRole::S)),
                                px(theme::sp(theme::SpaceRole::Xs)));
    auto* title = new QWidget;
    auto* line = row(title, theme::SpaceRole::S);
    line->addWidget(icon(which, theme::TextRole::Body, tone));
    line->addWidget(widgets::badge(kind_label(run.kind), kit::Tone::Normal));
    line->addWidget(widgets::label(loa::runtime::text::first_line(run.task, TASK_CHARS), theme::TextRole::Label, kit::Tone::Normal), 1);
    content->addWidget(title);
    content->addWidget(widgets::label(summary + " \u00b7 " + session, theme::TextRole::Caption, kit::Tone::Muted));
    for (auto* child : button->findChildren<QWidget*>()) child->setAttribute(Qt::WA_TransparentForMouseEvents);
    return button;
}

QWidget* State::detail(const sb::RunEntry& entry, const Emit& send) const {
    const auto& run = entry.run;
    auto* body = new QWidget;
    auto* layout = column(body, theme::SpaceRole::Xs);
    auto* task = widgets::label(run.task, theme::TextRole::Label, kit::Tone::Normal);
    task->setWordWrap(true);
    layout->addWidget(task);
    const std::string isolation = run.isolated ? "isolated " + std::string(sb::backend_name()) + " checkout of" : "directly in";
    layout->addWidget(widgets::label(std::string(kind_label(run.kind)) + " sandbox, " + isolation + " " + run.workspace.string(),
                                     theme::TextRole::Mono, kit::Tone::Muted));
    if (const auto* failed = std::get_if<run_state::Failed>(&entry.state)) {
        layout->addWidget(widgets::label(failed->error, theme::TextRole::Label, kit::Tone::Danger));
    } else if (const auto* finished = std::get_if<run_state::Finished>(&entry.state);
               finished && finished->final_message && finished->final_message->find_first_not_of(" \t\r\n") != std::string::npos) {
        auto* message = widgets::label(*finished->final_message, theme::TextRole::Label, kit::Tone::Muted);
        message->setWordWrap(true);
        layout->addWidget(message);
    }
    auto* actions = new QWidget;
    auto* bar = row(actions, theme::SpaceRole::Xs);
    const bool applying = applying_ == run.run;
    const auto* finished = std::get_if<run_state::Finished>(&entry.state);
    if (finished && finished->applied) {
        bar->addWidget(widgets::label("Patch applied", theme::TextRole::Label, kit::Tone::Ok));
    } else if (entry.pending_patch()) {
        widgets::Callback on;
        if (!applying) on = [send, id = run.run] { send(wrap(msg::Apply{id})); };
        bar->addWidget(widgets::text_button(applying ? "Applying\u2026" : "Apply patch", kit::ButtonKind::secondary(), std::move(on)));
    }
    bar->addWidget(widgets::text_button("Open worker session", kit::ButtonKind::secondary(),
                                        [send, worker = run.worker] { send(wrap(msg::OpenWorker{worker})); }));
    bar->addStretch(1);
    layout->addWidget(actions);
    return widgets::card(body);
}

QWidget* State::settings_view(const Emit& send) const {
    auto* content = new QWidget;
    auto* layout = column(content, theme::SpaceRole::S);
    layout->setContentsMargins(px(theme::sp(theme::SpaceRole::M)), 0, px(theme::sp(theme::SpaceRole::M)), 0);
    const std::map<std::string, std::string, std::less<>> errors;
    for (const auto& section : draft_.sections) {
#ifndef LOA_SANDBOX_VM
        if (section.kind == SandboxKind::Firecracker || section.kind == SandboxKind::Vm) continue;
#endif
        auto* settings = new QWidget;
        auto* fields = column(settings, theme::SpaceRole::S);
        if (!section.field_list.empty())
            fields->addWidget(widgets::form_view(
                section, errors, [send](std::string_view key, std::string value) { send(wrap(msg::Edit{std::string(key), std::move(value)})); },
                [send] { send(wrap(msg::Save{})); }));
        if (section.kind == SandboxKind::Docker) {
            auto* network = new QCheckBox(QStringLiteral("Containers may use the network"));
            network->setFont(kit::qt::font(theme::TextRole::Label));
            network->setChecked(draft_.network);
            QObject::connect(network, &QCheckBox::toggled, network, [send](bool on) { send(wrap(msg::Network{on})); });
            fields->addWidget(network);
        }
        layout->addWidget(provider(section.kind, settings));
    }
    const bool changed = draft_.config() != service_->config();
    auto* actions = new QWidget;
    auto* bar = row(actions, theme::SpaceRole::Xs);
    bar->addStretch(1);
    widgets::Callback recheck;
    if (!probing_) recheck = [send] { send(wrap(msg::Refresh{})); };
    bar->addWidget(widgets::text_button(probing_ ? "Checking\u2026" : "Recheck", kit::ButtonKind::secondary(), std::move(recheck)));
    widgets::Callback save;
    if (changed) save = [send] { send(wrap(msg::Save{})); };
    bar->addWidget(widgets::text_button("Save", kit::ButtonKind::secondary(), std::move(save)));
    layout->addWidget(actions);
    layout->addStretch(1);
    auto* scroll = new QScrollArea;
    scroll->setAccessibleName(QStringLiteral("Sandbox settings"));
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setWidget(content);
    return scroll;
}

QWidget* State::provider(SandboxKind kind, QWidget* settings) const {
    const sb::ProviderStatus* status = nullptr;
    if (statuses_)
        if (auto it = std::ranges::find(*statuses_, kind, &sb::ProviderStatus::kind); it != statuses_->end()) status = &*it;
    Icon which = Icon::CircleDot;
    kit::Tone tone = kit::Tone::Muted;
    std::string_view word = "Checking\u2026";
    if (status) {
        switch (status->availability) {
        case sb::Availability::Ready: which = Icon::Check, tone = kit::Tone::Ok, word = "Ready"; break;
        case sb::Availability::NotConfigured: word = "Not configured"; break;
        case sb::Availability::Unavailable: which = Icon::TriangleAlert, tone = kit::Tone::Danger, word = "Unavailable"; break;
        }
    }
    auto* box = new QWidget;
    box->setAccessibleName(QString::fromStdString(std::string(kind_label(kind))));
    kit::qt::style_container(*box, kit::Surface::Card);
    auto* layout = column(box, theme::SpaceRole::Xs);
    const int pad = px(theme::sp(theme::SpaceRole::M));
    layout->setContentsMargins(pad, pad, pad, pad);
    auto* title = new QWidget;
    auto* line = row(title, theme::SpaceRole::S);
    line->addWidget(icon(which, theme::TextRole::Body, tone));
    line->addWidget(widgets::label(kind_label(kind), theme::TextRole::Body, kit::Tone::Normal));
    line->addWidget(widgets::label(word, theme::TextRole::Label, tone));
    line->addStretch(1);
    layout->addWidget(title);
    if (status) {
        auto* detail = widgets::label(status->detail, theme::TextRole::Caption, kit::Tone::Muted);
        detail->setWordWrap(true);
        layout->addWidget(detail);
    }
    layout->addWidget(settings);
    return box;
}

} // namespace loa::app::areas::sandbox
