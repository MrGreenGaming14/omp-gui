// The peers area's state and behavior (port of `areas/peers/mod.rs`).

#include "areas/peers/peers.hpp"

#include <array>
#include <chrono>

#include <asio.hpp>

#include "loa/app/adapters/peer_sessions.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/watch.hpp"
#include "loa/contracts/session.hpp"
#include "loa/runtime/time.hpp"

namespace loa::app::areas::peers {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

// How long the node event feed sleeps when no event waits.
constexpr std::chrono::milliseconds EVENT_POLL{50};

std::expected<void, std::string> plain(p2p::Result<> result) {
    if (!result) return std::unexpected(result.error().to_string());
    return {};
}

// `[network]` from the central config; the defaults when it can't be read.
settings::app::NetworkSettings network_settings(Ctx& ctx) {
    auto config = ctx.read_config("network settings, using the defaults");
    return config ? config->network : settings::app::NetworkSettings{};
}

// Puts `text` on the clipboard (iced `clipboard::write`).
Task clipboard(std::string text) { return Task::chat(chat::Task::of(chat::op::WriteClipboard{std::move(text)})); }

Task copy_key(State&, std::string_view, Ctx& ctx) {
    ctx.notify("Key copied");
    return clipboard(ctx.services.node.key);
}
Task refresh_now(State& s, std::string_view, Ctx& ctx) { return s.apply(panel::intent::Refresh{}, ctx); }
Task scan_tailnet(State& s, std::string_view, Ctx& ctx) { return s.apply(panel::intent::ScanTailscale{}, ctx); }
Task restart_node(State& s, std::string_view, Ctx& ctx) { return s.restart(ctx); }

// The node's events until `stop`; a feed that fell behind reports a change.
loa::runtime::Task node_events(std::shared_ptr<p2p::Node> node, Task::Emit send, std::stop_token stop) {
    auto events = node->events();
    node.reset();
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        auto next = events.try_recv();
        if (!next) {
            timer.expires_after(EVENT_POLL);
            co_await timer.async_wait(asio::use_awaitable);
            continue;
        }
        p2p::NodeEvent event = *next ? std::move(**next) : p2p::NodeEvent{p2p::node_event::Changed{}};
        if (stop.stop_requested()) co_return;
        send(State::message(msg::Node{std::move(event)}));
    }
}

} // namespace

std::string recent_id(std::string_view peer, std::string_view session) {
    return std::string(app::peers::RECENT_PREFIX) + std::string(peer) + ":" + std::string(session);
}

std::optional<std::pair<std::string_view, std::string_view>> parse_recent(std::string_view id) {
    if (!id.starts_with(app::peers::RECENT_PREFIX)) return std::nullopt;
    id.remove_prefix(app::peers::RECENT_PREFIX.size());
    const auto colon = id.find(':');
    if (colon == std::string_view::npos) return std::nullopt;
    return std::pair{id.substr(0, colon), id.substr(colon + 1)};
}

p2p::RelayConfig relay_config(const settings::app::Relay& relay) {
    // loa-p2p reaches peers through relays the user runs; it has no public ones.
    if (const auto* url = std::get_if<settings::app::Relay::Url>(&relay.value)) return p2p::RelayConfig::self_hosted_at(url->url);
    // `Default` maps to off (no public relays), as in the Rust app (#292).
    return p2p::RelayConfig::off();
}

p2p::NodeConfig node_config(const Ctx& ctx, const settings::app::NetworkSettings& network) {
    const auto& services = ctx.services;
    auto config = p2p::NodeConfig::make(p2p::data_dir(services.storage.dirs.data), services.node.label, std::string(APP_VERSION));
    config.bind = "0.0.0.0:" + std::to_string(network.port);
    config.relay = relay_config(network.relay);
    config.mdns = network.discovery.mdns;
    if (!network.discovery.tailscale) config.tailscale.reset();
    const auto& library = ctx.runtime.library();
    config.sessions = std::shared_ptr<const bus::Store>(library, &library->store());
    return config;
}

// ---- State ---------------------------------------------------------------------------------

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    const auto network = network_settings(ctx);
    return with_config(ctx, node_config(ctx, network));
}

std::pair<std::unique_ptr<State>, Task> State::with_config(Ctx& ctx, p2p::NodeConfig config) {
    auto state = std::make_unique<State>();
    auto task = state->open(ctx, std::move(config));
    return {std::move(state), std::move(task)};
}

std::span<const AreaCommand<State>> State::COMMANDS() {
    using L = Listed<State>;
    static const std::array<AreaCommand<State>, 4> commands{{
        {"peers.copy_key", "Copy this node's key", "Peers", std::nullopt, L::always(), &copy_key},
        {"peers.refresh", "Check peers now", "Peers", std::nullopt, L::always(), &refresh_now},
        {"peers.scan_tailnet", "Scan the tailnet for nodes", "Peers", std::nullopt, L::always(), &scan_tailnet},
        {"peers.restart", "Restart the peer endpoint with the network settings", "Peers", std::nullopt, L::always(),
         &restart_node},
    }};
    return commands;
}

Task State::reopen(Ctx& ctx, p2p::NodeConfig config) {
    remote_.reset();
    folder_.reset();
    node_.reset();
    ctx.node.set(nullptr);
    return open(ctx, std::move(config));
}

Task State::open(Ctx& ctx, p2p::NodeConfig config) {
    ++generation_;
    auto& services = ctx.services;
    auto sessions = std::make_shared<adapters::LibrarySessions>(ctx.runtime.library());
    auto opened = p2p::Node::open_with_grants(std::move(config), services.node.identity, services.node.key_source,
                                              std::move(sessions), services.node.grants);
    if (!opened) {
        node_.reset();
        ctx.node.set(nullptr);
        ctx.notify("The peer node can't open its stores: " + opened.error().to_string());
        refresh(ctx);
        return Task::none();
    }
    auto node = std::move(*opened);
    services.node.peer_access->set(node->authorizer());
    ctx.node.set(node);
    node_ = node;
    refresh(ctx);
    std::vector<Task> tasks;
    tasks.push_back(blocking([node] { return plain(node->start()); },
                             [](std::expected<void, std::string> result) { return message(msg::Started{std::move(result)}); }));
    tasks.push_back(resources(ctx));
    return Task::batch(std::move(tasks));
}

void State::refresh(Ctx& ctx) {
    const auto& local = ctx.services.node;
    auto& sidebar = ctx.shell.sidebar_mut();
    if (!node_) {
        sidebar.set_nodes({loa::view::shell::NodeStatus{local.key, local.label, false, true}});
        return;
    }
    panel_.set_data(data(*node_, runtime::time::now_ms()));
    const bool online = std::holds_alternative<p2p::endpoint::Listening>(node_->status().endpoint);
    std::vector<loa::view::shell::NodeStatus> nodes{{local.key, local.label, online, true}};
    for (const auto& peer : node_->peers())
        nodes.push_back({peer.key.to_string(), node_->peer_name(peer.key), peer.online, false});
    sidebar.set_nodes(std::move(nodes));
    auto sessions = node_->remote_sessions();
    std::vector<std::string> ids;
    ids.reserve(sessions.size());
    for (const auto& session : sessions) ids.push_back(recent_id(session.peer.to_string(), session.id));
    for (const auto& stale : listed_)
        if (std::ranges::find(ids, stale) == ids.end()) sidebar.remove_session(stale);
    for (std::size_t i = 0; i < sessions.size(); ++i) {
        auto& session = sessions[i];
        contracts::SessionSummary summary;
        summary.id = ids[i];
        summary.title = session.title.empty() ? "Session " + session.id : std::move(session.title);
        summary.node = session.peer.to_string();
        summary.updated = session.created_ms;
        sidebar.upsert_session(std::move(summary));
    }
    listed_ = std::move(ids);
}

Task State::resources(Ctx& ctx) const {
    if (!node_) return Task::none();
    std::vector<std::string> hosts;
    if (auto config = ctx.read_config("SSH hosts"))
        for (const auto& [host, _] : config->remote_hosts) hosts.push_back(host);
    auto mcp = ctx.services.node.mcp;
    return blocking(
        [hosts = std::move(hosts), mcp] {
            std::vector<std::pair<trust::Resource, std::string>> found;
            for (const auto& host : hosts) found.emplace_back(trust::resource::Ssh{host}, "SSH " + host);
            if (mcp)
                for (const auto& spec : mcp->specs()) {
                    // Servers that aren't running or declare no surfaces offer none.
                    auto surfaces = mcp->surfaces(spec.name);
                    if (!surfaces) continue;
                    for (const auto& [uri, _] : *surfaces)
                        found.emplace_back(trust::resource::Surface{uri.as_str()}, "Surface " + uri.as_str());
                }
            return found;
        },
        [](std::vector<std::pair<trust::Resource, std::string>> found) { return message(msg::Resources{std::move(found)}); });
}

Task State::apply(panel::Intent intent, Ctx& ctx) {
    if (!node_) {
        ctx.notify("The peer node isn't running; see the notice from startup");
        return Task::none();
    }
    return blocking([node = node_, intent = std::move(intent)]() mutable { return peers::apply(*node, std::move(intent)); },
                    [](std::expected<Outcome, std::string> result) { return message(msg::Applied{std::move(result)}); });
}

Task State::applied(Outcome result, Ctx& ctx) {
    Task task = std::visit(
        overloaded{
            [](outcome::Done&) { return Task::none(); },
            [&](outcome::Notice& n) {
                ctx.notify(std::move(n.text));
                return Task::none();
            },
            [&](outcome::KeyChecked& k) {
                panel_.set_key_validation(k.key, k.valid);
                return Task::none();
            },
            [&](outcome::Copy& c) {
                ctx.notify("Key copied");
                return clipboard(std::move(c.text));
            },
            [&](outcome::Opened& o) {
                const auto name = node_ ? node_->peer_name(o.view->peer()) : p2p::short_key(o.view->peer());
                remote_.emplace(std::move(o.view), name, ctx.services.node.key);
                folder_.reset();
                ctx.show_screen(ID);
                return Task::none();
            },
            [&](outcome::Forked& f) {
                ctx.request(request::OpenSession{std::move(f.id)});
                ctx.notify("Forked into a session on this node; it runs on this node's harness and keys");
                return Task::none();
            },
            [&](outcome::Folder& f) {
                if (node_) {
                    auto key = p2p::Node::parse_key(f.peer);
                    auto name = key ? node_->peer_name(*key) : f.peer;
                    folder_.emplace(Browser{node_, std::move(f.peer), std::move(name), std::move(f.folder), {},
                                            std::move(f.entries), f.can_write, std::nullopt, false});
                    remote_.reset();
                    ctx.show_screen(ID);
                }
                return Task::none();
            },
        },
        result);
    refresh(ctx);
    return task;
}

void State::node_event(p2p::NodeEvent event, Ctx& ctx) {
    std::visit(overloaded{
                   [&](p2p::node_event::Changed&) { refresh(ctx); },
                   [&](p2p::PromptRequested& e) {
                       const auto name = node_ ? node_->peer_name(e.peer) : p2p::short_key(e.peer);
                       ctx.request(request::OpenSession{e.session});
                       ctx.request(request::Prompt{contracts::Prompt{std::move(e.text), {}, e.peer.to_string()}});
                       ctx.notify(name + " sent a prompt to this session");
                   },
                   [&](p2p::AnswerRequested& e) {
                       to::decisions::Answer answer{std::move(e.session), e.decision,
                                                    contracts::Answer{std::move(e.values), std::move(e.reason), e.peer.to_string()}};
                       ctx.request(request::Forward{area_message(area_id::DECISIONS, std::move(answer))});
                   },
               },
               event);
}

Task State::restart(Ctx& ctx) {
    remote_.reset();
    folder_.reset();
    if (ctx.screen() == ID) ctx.close_screen();
    auto node = std::move(node_);
    node_.reset();
    if (!node) return Task::done(message(msg::Stopped{}));
    return blocking(
        [node] {
            node->stop();
            return true;
        },
        [](bool) { return message(msg::Stopped{}); });
}

Task State::update_typed(Message m, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](msg::Panel& p) {
                auto intent = panel_.update(std::move(p.message));
                return intent ? apply(std::move(*intent), ctx) : Task::none();
            },
            [&](msg::Open&) {
                ctx.open_tab(std::string(TAB));
                refresh(ctx);
                return resources(ctx);
            },
            [&](msg::Refresh&) {
                refresh(ctx);
                return resources(ctx);
            },
            [&](msg::Started& s) {
                if (!s.result) ctx.notify("The peer endpoint didn't start: " + s.result.error());
                refresh(ctx);
                return Task::none();
            },
            [&](msg::Node& n) {
                node_event(std::move(n.event), ctx);
                return Task::none();
            },
            [&](msg::Applied& a) {
                if (a.result) return applied(std::move(*a.result), ctx);
                ctx.notify(std::move(a.result.error()));
                refresh(ctx);
                return Task::none();
            },
            [&](msg::Resources& r) {
                if (node_) node_->set_resources(std::move(r.resources));
                refresh(ctx);
                return Task::none();
            },
            [&](msg::Stopped&) {
                const auto network = network_settings(ctx);
                return open(ctx, node_config(ctx, network));
            },
            [&](msg::OpenRecent& o) {
                auto parsed = parse_recent(o.id);
                if (!parsed) return Task::none();
                auto [peer, session] = *parsed;
                if (remote_ && remote_->is(peer, session) && !remote_->ended) {
                    folder_.reset();
                    ctx.show_screen(ID);
                    return Task::none();
                }
                return apply(panel::intent::OpenRemote{std::string(peer), std::string(session)}, ctx);
            },
            [&](msg::Remote& r) { return remote_update(std::move(r.message), ctx); },
            [&](msg::Folder& f) { return folder_update(std::move(f.message), ctx); },
        },
        m);
}

std::vector<PanelTab> State::panel_tabs() const { return {PanelTab{std::string(TAB), "Peers", std::nullopt, false}}; }

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    // New sessions and prompts become resources grants can name.
    if (std::holds_alternative<app_runtime::session_event::Loaded>(event) ||
        std::holds_alternative<app_runtime::session_event::TurnStarted>(event) ||
        std::holds_alternative<app_runtime::session_event::Closed>(event))
        refresh(ctx);
    return Task::none();
}

Subscriptions State::subscription() const {
    Subscriptions out;
    if (node_) {
        auto node = node_;
        out.push_back(subscription::Stream{
            "peers.node:" + node->key().to_string() + ":" + std::to_string(generation_),
            [node](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
                executor.spawn(node_events(node, std::move(send), std::move(stop)));
            }});
    }
    if (remote_) {
        auto writer = remote_->view->writer();
        out.push_back(watch_feed(
            ptr_key("peers.remote", writer),
            [writer]() -> std::unique_ptr<app_runtime::Source<app_runtime::Feed>> {
                return std::make_unique<app_runtime::WriterFeed>(writer, bus::Policy::every_patch(REMOTE_FEED_CAPACITY));
            },
            [](app_runtime::Feed feed) -> std::optional<app::Message> {
                return message(msg::Remote{remote_msg::Feed{std::move(feed)}});
            }));
    }
    return out;
}

} // namespace loa::app::areas::peers
