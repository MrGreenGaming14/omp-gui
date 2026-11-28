#include "surfaces.hpp"

#include <algorithm>
#include <array>
#include <chrono>

#include <asio.hpp>

#include <QWidget>

#include "loa/app/adapters/surface.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/view/surface.hpp"
#include "loa/app/watch.hpp"
#include "loa/core/schema.hpp"
#include "loa/mcp/manager.hpp"
#include "loa/surfaces/hub/dom.hpp"
#include "loa/ui/surface/runner.hpp"
#include "loa/ui/surface/view.hpp"

namespace loa::app::areas::surfaces {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

namespace vs = loa::view::surface;
using loa::app::view::available;
using loa::app::view::browser_status;
using loa::app::view::capture_from_view;
using loa::app::view::capture_status;

// How often the hub's and the MCP servers' event feeds are read.
constexpr auto EVENT_POLL = std::chrono::milliseconds(50);

// Rust `Debug` of an encoding, as the notice names it.
std::string_view encoding_debug(vs::Encoding encoding) {
    switch (encoding) {
    case vs::Encoding::Rgba: return "Rgba";
    case vs::Encoding::Jpeg: return "Jpeg";
    case vs::Encoding::Png: return "Png";
    case vs::Encoding::H264: return "H264";
    case vs::Encoding::Hevc: return "Hevc";
    }
    return "Rgba";
}

// Changes the `[surfaces]` table of the central config.
template <typename F>
void save(Ctx& ctx, F&& change) {
    ctx.edit_config("surface settings", [&](settings::app::Config& config) { change(config.surfaces); });
}

contracts::Awaitable<std::expected<Endpoint, std::string>> start_endpoint(std::shared_ptr<hub::SurfaceTools> tools) {
    auto endpoint = co_await hub::McpEndpoint::start(std::move(tools));
    if (!endpoint) co_return std::unexpected(endpoint.error().to_string());
    co_return std::make_shared<const hub::McpEndpoint>(std::move(*endpoint));
}

contracts::Awaitable<std::vector<hub::Listed>> list_hub(std::shared_ptr<hub::SurfaceHub> hub) { co_return co_await hub->list(); }

contracts::Awaitable<std::vector<std::pair<std::string, std::string>>> declared(std::shared_ptr<hub::SurfaceHub> hub,
                                                                                 std::shared_ptr<mcp::Manager> mcp) {
    co_return co_await hub->refresh_declared(*mcp);
}

// Opening `request` on the hub, as an `Opened` message.
Task opening(std::shared_ptr<hub::SurfaceHub> hub, hub::OpenRequest request) {
    return perform(
        [hub = std::move(hub), request = std::move(request)]() -> contracts::Awaitable<std::expected<hub::Opened, std::string>> {
            auto opened = co_await hub->open_request(request);
            if (!opened) co_return std::unexpected(opened.error().message());
            co_return std::move(*opened);
        },
        [](std::expected<hub::Opened, std::string> result) -> loa::app::Message {
            return State::message(msg::Opened{std::move(result)});
        });
}

contracts::Awaitable<std::expected<void, std::string>> close_surface(std::shared_ptr<hub::SurfaceHub> hub, SurfaceUri uri) {
    auto closed = co_await hub->close(std::move(uri));
    if (!closed) co_return std::unexpected(closed.error().message());
    co_return std::expected<void, std::string>{};
}

loa::runtime::Task hub_events(hub::HubEventReceiver events, Task::Emit send, std::stop_token stop) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        auto next = events.try_recv();
        if (!next) {
            timer.expires_after(EVENT_POLL);
            co_await timer.async_wait(asio::use_awaitable);
            continue;
        }
        if (stop.stop_requested()) co_return;
        send(State::message(msg::Hub{std::move(*next)}));
    }
}

// Server starts and tool changes don't change the surfaces a server declares.
bool rereads(const mcp::Event& event) {
    if (std::holds_alternative<mcp::event::ToolsChanged>(event)) return false;
    if (const auto* status = std::get_if<mcp::event::Status>(&event)) return status->status.kind != mcp::Status::Kind::Starting;
    return true;
}

loa::runtime::Task mcp_events(mcp::EventReceiver events, Task::Emit send, std::stop_token stop) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        auto next = events.recv(std::chrono::milliseconds(0));
        if (!next) {
            timer.expires_after(EVENT_POLL);
            co_await timer.async_wait(asio::use_awaitable);
            continue;
        }
        if (stop.stop_requested()) co_return;
        if (rereads(*next)) send(State::message(msg::Mcp{}));
    }
}

#if defined(LOA_SURFACES_REMOTE_VIDEO) && defined(LOA_REMOTE_VIDEO)
loa::runtime::Task switch_video(std::shared_ptr<hub::SurfaceHub> hub, bool enabled) { co_await hub->video().set_enabled(enabled); }

contracts::Awaitable<std::shared_ptr<const LiveVideo>> try_encoders(std::shared_ptr<hub::SurfaceHub> hub) {
    co_return co_await hub->video().encoders();
}
#endif

const std::array<AreaCommand<State>, 3> COMMAND_TABLE{
    AreaCommand<State>{"surfaces.browser", "Open browser surface", "Surfaces", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return s.launcher(ctx); }},
    AreaCommand<State>{"surfaces.document", "Open document surface", "Surfaces", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return s.launcher(ctx); }},
    AreaCommand<State>{"surfaces.vm", "Open VNC surface", "Surfaces", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return s.launcher(ctx); }},
};

} // namespace

std::span<const AreaCommand<State>> State::COMMANDS() { return COMMAND_TABLE; }

Tabs::Tabs() = default;
Tabs::~Tabs() = default;

State::State(std::shared_ptr<hub::SurfaceHub> hub, std::shared_ptr<hub::SurfaceTools> tools, std::shared_ptr<mcp::Manager> mcp,
             loa::ui::surface::Panel panel)
    : hub_(std::move(hub)), tools_(std::move(tools)), mcp_(std::move(mcp)), panel_(std::move(panel)),
      tabs_(std::make_shared<Tabs>()) {}

State::~State() = default;

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto config = hub::HubConfig::discover(ctx.services.storage.dirs.data / "surfaces");
    if (auto settings = ctx.read_config("surface settings")) {
        const auto& surfaces = settings->surfaces;
#if defined(LOA_SURFACES_REMOTE_VIDEO)
        config.video.enabled = surfaces.video;
#endif
        if (surfaces.browser_capture == settings::app::BrowserCapture::VirtualDisplay && config.virtual_display)
            config.capture = CaptureMode::VirtualDisplay;
    }
    return with_hub(hub::SurfaceHub::make(std::move(config)), ctx);
}

std::pair<std::unique_ptr<State>, Task> State::with_hub(std::shared_ptr<hub::SurfaceHub> hub, Ctx& ctx) {
    auto tools = hub::SurfaceTools::make(hub);
    for (const auto& spec : tools->host_tools().first) ctx.set_app_tool(loa::view::panel::ToolInfo::from(spec));
    auto options = loa::ui::surface::Options::make(adapters::decoders(), std::make_shared<adapters::DocsRenderer>(hub->config().fonts));
    loa::ui::surface::Panel panel(std::move(options), browser_status(*hub), ctx.runtime.cwd());
    Task encoders;
#if defined(LOA_SURFACES_REMOTE_VIDEO) && defined(LOA_REMOTE_VIDEO)
    // Shows the toggle and tries this machine's encoders.
    panel.set_video(vs::VideoStatus{hub->video().enabled(), std::nullopt, {}});
    encoders = perform([hub] { return try_encoders(hub); },
                       [](std::shared_ptr<const LiveVideo> e) -> loa::app::Message { return message(msg::Encoders{std::move(e)}); });
#endif
    panel.set_capture(capture_status(*hub, false));
    auto state = std::make_unique<State>(hub, tools, ctx.services.node.mcp, std::move(panel));
    auto endpoint = perform([tools] { return start_endpoint(tools); },
                            [](std::expected<Endpoint, std::string> result) -> loa::app::Message {
                                return message(msg::EndpointStarted{std::move(result)});
                            });
    std::vector<Task> tasks;
    tasks.push_back(std::move(endpoint));
    tasks.push_back(state->refresh_declared());
    tasks.push_back(state->list());
    tasks.push_back(std::move(encoders));
    return {std::move(state), Task::batch(std::move(tasks))};
}

Task State::list() const {
    std::vector<Task> tasks;
    tasks.push_back(perform([hub = hub_] { return list_hub(hub); },
                            [](std::vector<hub::Listed> listed) -> loa::app::Message { return message(msg::Listed{std::move(listed)}); }));
    tasks.push_back(Task::done(message(msg::CapturePending{hub_->capture_pending()})));
    return Task::batch(std::move(tasks));
}

Task State::refresh_declared() const {
    if (!mcp_) return Task::none();
    return perform([hub = hub_, mcp = mcp_] { return declared(hub, mcp); },
                   [](std::vector<std::pair<std::string, std::string>> failed) -> loa::app::Message {
                       return message(msg::Declared{std::move(failed)});
                   });
}

Task State::set_capture(vs::Capture capture, Ctx& ctx) {
    if (auto set = hub_->set_capture(capture_from_view(capture)); !set) {
        ctx.notify(set.error().message());
        panel_.set_capture(capture_status(*hub_, false));
        return Task::none();
    }
    save(ctx, [capture](settings::app::SurfaceSettings& settings) {
        settings.browser_capture = capture == vs::Capture::Screencast ? settings::app::BrowserCapture::Screencast
                                                                      : settings::app::BrowserCapture::VirtualDisplay;
    });
    return list();
}

Task State::set_video(bool enabled, Ctx& ctx) {
#if defined(LOA_SURFACES_REMOTE_VIDEO) && defined(LOA_REMOTE_VIDEO)
    save(ctx, [enabled](settings::app::SurfaceSettings& settings) { settings.video = enabled; });
    return Task::job([hub = hub_, enabled](Task::Emit, loa::runtime::Executor& executor) { executor.spawn(switch_video(hub, enabled)); });
#else
    // The launcher has no live video toggle without the feature.
    (void)enabled;
    (void)ctx;
    return Task::none();
#endif
}

Task State::panel_event(vs::Intent intent, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](vs::intent::OpenBrowser& i) { return opening(hub_, hub::open_request::Url{std::move(i.url)}); },
            [&](vs::intent::OpenDocument& i) { return opening(hub_, hub::open_request::File{std::move(i.path)}); },
            [&](vs::intent::OpenVm& i) { return opening(hub_, hub::open_request::Vm{std::move(i.address), std::move(i.password)}); },
            [&](vs::intent::Open& i) { return opening(hub_, hub::open_request::Uri{std::move(i.uri)}); },
            [&](vs::intent::Close& i) {
                return perform([hub = hub_, uri = std::move(i.uri)] { return close_surface(hub, uri); },
                               [](std::expected<void, std::string> result) -> loa::app::Message {
                                   return message(msg::Closed{std::move(result)});
                               });
            },
            [&](vs::intent::SetVideo& i) { return set_video(i.enabled, ctx); },
            [&](vs::intent::SetCapture& i) { return set_capture(i.capture, ctx); },
            [&](vs::intent::EncodingUnsupported& i) {
                std::string supported;
                for (auto encoding : i.supported) supported += (supported.empty() ? "" : ", ") + std::string(encoding_debug(encoding));
                ctx.notify(i.uri.as_str() + " streams " + std::string(encoding_debug(i.encoding)) +
                           ", which the renderer can't show (it shows " + supported + ")");
                return Task::none();
            },
        },
        intent);
}

Task State::run_panel(panel::Task task) const {
    if (task.empty()) return Task::none();
    return Task::job([tabs = tabs_, work = std::move(task.work)](Task::Emit send, loa::runtime::Executor& executor) mutable {
        for (auto& routed : work) {
            auto runner = tabs->runners.find(routed.uri);
            if (runner == tabs->runners.end()) {
                // Detached meanwhile: nothing to run it on.
                const auto surface = tabs->surfaces.find(routed.uri);
                if (surface == tabs->surfaces.end()) continue;
                auto post = [send](std::function<void()> run) {
                    send(message(msg::Run{std::make_shared<std::function<void()>>(std::move(run))}));
                };
                auto deliver = [send, uri = routed.uri](loa::ui::surface::Message m) {
                    send(message(msg::Panel{panel::msg::Surface{uri, std::move(m)}}));
                };
                runner = tabs->runners
                             .emplace(routed.uri, std::make_unique<loa::ui::surface::Runner>(surface->second, executor, std::move(post),
                                                                                              std::move(deliver)))
                             .first;
            }
            runner->second->run(loa::ui::surface::Task::of(std::move(routed.work)));
        }
    });
}

Task State::show(const hub::Opened& opened, Ctx& ctx) {
    tabs_->surfaces.insert_or_assign(opened.info.uri, opened.surface);
    auto attach = run_panel(panel_.attach(opened.surface, std::string(opened.origin.server())));
    if (!recorded_.contains(opened.info.uri)) record(opened, ctx);
    return attach;
}

void State::record(const hub::Opened& opened, Ctx& ctx) {
    const auto* session = ctx.runtime.sessions().active();
    if (!session) {
        if (std::ranges::find(unrecorded_, opened.info.uri) == unrecorded_.end()) unrecorded_.push_back(opened.info.uri);
        return;
    }
    const auto* root = session->dom().root();
    if (!root) return;
    auto parent = root->id;
    const auto turns = app_runtime::dom_turns(session->dom());
    if (!turns.empty())
        if (auto text = turns.back()->str_attr(core::attr::STATUS))
            if (auto status = core::parse<core::TurnStatus>(*text);
                status && (*status == core::TurnStatus::Running || *status == core::TurnStatus::Waiting))
                parent = turns.back()->id;
    auto patch = session->writer()->commit(hub::dom::opened(parent, opened.info, opened.origin));
    if (!patch) {
        ctx.notify("Can't record " + opened.info.uri.as_str() + " in the session: " + core::to_string(patch.error()));
        return;
    }
    recorded_[opened.info.uri].emplace_back(session->id(), patch->node);
}

void State::record_closed(const SurfaceUri& uri, Ctx& ctx) {
    std::erase(unrecorded_, uri);
    auto nodes = recorded_.extract(uri);
    if (nodes.empty()) return;
    for (const auto& [id, node] : nodes.mapped()) {
        const auto* session = ctx.runtime.sessions().get(id.as_str());
        if (!session) continue;
        if (auto closed = session->writer()->commit(hub::dom::closed(node)); !closed)
            ctx.notify("Can't record " + uri.as_str() + " closed in the session: " + core::to_string(closed.error()));
    }
}

void State::detach(const SurfaceUri& uri) {
    panel_.detach(uri);
    tabs_->runners.erase(uri);
    tabs_->surfaces.erase(uri);
}

Task State::launcher(Ctx& ctx) {
    ctx.open_tab(std::string(TAB));
    return run_panel(panel_.update(panel::msg::Launcher{}).first);
}

Task State::update_typed(Message m, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](msg::Panel& p) {
                auto [task, event] = panel_.update(std::move(p.message));
                auto out = run_panel(std::move(task));
                if (event) out.append(panel_event(std::move(*event), ctx));
                return out;
            },
            [&](msg::Opened& o) {
                if (!o.result) {
                    panel_.failed(std::move(o.result.error()));
                    return Task::none();
                }
                auto out = show(*o.result, ctx);
                out.append(list());
                return out;
            },
            [&](msg::Closed& c) {
                (void)ctx.report("Closing the surface failed", std::move(c.result).transform([] { return true; }));
                return list();
            },
            [&](msg::Hub& h) {
                return std::visit(overloaded{
                                      [&](hub::hub_event::Opened& e) {
                                          auto opened = hub_->opened(e.info.uri);
                                          if (!opened) return Task::none();
                                          // Opened by an agent: show it.
                                          if (!panel_.surface(e.info.uri)) ctx.open_tab(std::string(TAB));
                                          auto out = show(*opened, ctx);
                                          out.append(list());
                                          return out;
                                      },
                                      [&](hub::hub_event::Closed& e) {
                                          detach(e.uri);
                                          record_closed(e.uri, ctx);
                                          return list();
                                      },
                                      [&](hub::hub_event::Declared&) { return list(); },
                                  },
                                  h.event);
            },
            [&](msg::Listed& l) {
                std::vector<vs::AvailableSurface> offered;
                for (auto& listed : l.listed)
                    if (!listed.open) offered.push_back(available(std::move(listed)));
                panel_.set_available(std::move(offered));
                return Task::none();
            },
            [&](msg::Mcp&) { return refresh_declared(); },
            [&](msg::Declared& d) {
                if (!d.failed.empty()) {
                    std::string failed;
                    for (const auto& [server, error] : d.failed) failed += (failed.empty() ? "" : "; ") + server + ": " + error;
                    ctx.notify("Can't list the surfaces of MCP servers (" + failed + ")");
                }
                return Task::none();
            },
            [&](msg::EndpointStarted& e) {
                if (e.result) endpoint_ = std::move(*e.result);
                else ctx.notify("The surface MCP servers didn't start: " + e.result.error());
                return Task::none();
            },
            [&](msg::Encoders& e) {
#if defined(LOA_REMOTE_VIDEO)
                if (e.encoders) panel_.set_video(video_status(panel_.video().enabled, *e.encoders));
#else
                (void)e;
#endif
                return Task::none();
            },
            [&](msg::CapturePending& c) {
                panel_.set_capture(capture_status(*hub_, c.pending));
                return Task::none();
            },
            [&](msg::Run& r) {
                if (r.run && *r.run) (*r.run)();
                return Task::none();
            },
        },
        m);
}

std::vector<PanelTab> State::panel_tabs() const {
    return {PanelTab{std::string(TAB), "Surfaces", loa::view::theme::Icon::Globe, false}};
}

QWidget* State::view_tab(std::string_view id, const Ctx&, const Emit& send) const {
    if (id != TAB) return nullptr;
    auto* view = new loa::ui::surface::PanelView;
    view->setAccessibleName(QStringLiteral("surfaces"));
    view->render(panel_);
    QObject::connect(view, &loa::ui::surface::PanelView::message, view,
                     [send](const panel::Message& m) { send(message(msg::Panel{m})); });
    return view;
}

void State::contribute(contracts::SessionConfig& config, const Target& target, const Ctx&) const {
    if (!target.capabilities || target.capabilities->host_tools || !target.capabilities->mcp_servers) return;
    if (endpoint_)
        for (auto& spec : endpoint_->specs()) config.mcp_servers.push_back(std::move(spec));
}

std::optional<HostToolSet> State::host_tools(const contracts::SessionConfig&, const Target& target, const Ctx&) const {
    if (!target.capabilities || !target.capabilities->host_tools) return std::nullopt;
    auto [specs, handler] = tools_->host_tools();
    return HostToolSet{std::move(specs), std::move(handler)};
}

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    if (const auto* activated = std::get_if<app_runtime::session_event::Activated>(&event); activated && activated->id) {
        for (const auto& uri : std::exchange(unrecorded_, {}))
            if (auto opened = hub_->opened(uri)) record(*opened, ctx);
    } else if (const auto* closed = std::get_if<app_runtime::session_event::Closed>(&event)) {
        for (auto& [_, nodes] : recorded_) std::erase_if(nodes, [&](const auto& entry) { return entry.first == closed->id; });
    }
    return Task::none();
}

// The hub's events; and the changes of the app's MCP servers, which re-read the declared surfaces.
Subscriptions State::subscription() const {
    Subscriptions out;
    out.push_back(subscription::Stream{
        ptr_key("surfaces.hub", hub_), [hub = hub_](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
            executor.spawn(hub_events(hub->events(), std::move(send), std::move(stop)));
        }});
    if (mcp_)
        out.push_back(subscription::Stream{
            ptr_key("surfaces.mcp", mcp_), [mcp = mcp_](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
                executor.spawn(mcp_events(mcp->events(), std::move(send), std::move(stop)));
            }});
    return out;
}

} // namespace loa::app::areas::surfaces
