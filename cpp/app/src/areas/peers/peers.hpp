#pragma once

// This node on the peer network (port of `loa-app/src/areas/peers/`): its key, the endpoint
// (UDP) with the TCP fallback on `[network] port`, LAN and tailnet discovery, the Peers panel
// tab (peers, grants given and received, peers' sessions and shared folders), peers' sessions
// in Recents tagged by node, a peer's session opened in the center to watch, prompt, answer
// or fork, and a peer's shared folder browsed in the center, its files opened and (with
// drive) saved.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

// First: it declares a member named `emit`, which the Qt headers below `area.hpp` define away.
#include "loa/p2p/node.hpp"

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/core/dom.hpp"
#include "loa/settings/app/schema.hpp"
#include "loa/trust/scope.hpp"
#include "loa/ui/session/decisions/state.hpp"
#include "loa/ui/session/transcript/state.hpp"
#include "loa/ui/settings/peers/state.hpp"
#include "loa/view/settings_peers.hpp"

namespace loa::app::areas::peers {

namespace panel = loa::view::settings::peers;
namespace screen = loa::ui::settings::peers;
namespace chat = loa::ui::session::transcript;
namespace cards = loa::ui::session::decisions;

// Id of the Peers panel tab.
inline constexpr std::string_view TAB = "peers";
// The app version peers see (the Rust workspace's `CARGO_PKG_VERSION`).
inline constexpr std::string_view APP_VERSION = "0.1.0";
// Patches a peer session mirror's feed may hold before it resubscribes.
inline constexpr std::size_t REMOTE_FEED_CAPACITY = 4096;

// ---- The panel over a node (`adapter.rs`) ----------------------------------------------------

// A grant resource as the panel's picker id.
[[nodiscard]] std::string resource_id(const trust::Resource& resource);
// The resource a picker id names. Future sessions start at `now_ms`.
[[nodiscard]] std::optional<trust::Resource> parse_resource(std::string_view id, std::int64_t now_ms);
[[nodiscard]] panel::GrantScope scope_kind(const trust::Resource& resource);
// The actions a panel permission gives on a resource kind.
[[nodiscard]] p2p::Result<std::vector<trust::Action>> actions(panel::Permission permission, panel::GrantScope scope);
// The strongest panel permission among `actions`.
[[nodiscard]] panel::Permission permission(const trust::ActionSet& actions);
// The scopes and end a panel grant request asks for.
[[nodiscard]] p2p::Result<std::pair<std::vector<trust::Scope>, std::optional<std::int64_t>>> grant_request(
    const panel::GrantInput& input, std::int64_t now_ms);
// The real path of a folder on this node a grant may name: an absolute path to an existing
// folder; links are resolved, so the grant names the folder they lead to.
[[nodiscard]] p2p::Result<std::string> local_folder(std::string_view path);
// A grant as the panel lists it, labelled from `labels` where they name its resource.
[[nodiscard]] std::optional<panel::Grant> grant(const p2p::GrantInfo& info,
                                                std::span<const std::pair<trust::Resource, std::string>> labels);
// The panel's snapshot of `node` at `now_ms`.
[[nodiscard]] panel::Data data(const p2p::Node& node, std::int64_t now_ms);

// What a panel intent led to, for the app to finish (clipboard, center view).
namespace outcome {
struct Done {};
// A notice for the user.
struct Notice {
    std::string text;
};
struct KeyChecked {
    std::string key;
    bool valid = false;
};
// Text to put on the clipboard.
struct Copy {
    std::string text;
};
// A live view of a peer's session to show in the center.
struct Opened {
    std::shared_ptr<p2p::RemoteView> view;
};
// A fork of a peer's session, now a local session with this id.
struct Forked {
    std::string id;
};
// A peer's shared folder, listed, to browse in the center.
struct Folder {
    std::string peer;
    std::string folder;
    std::vector<p2p::FolderEntry> entries;
    bool can_write = false;
};
} // namespace outcome
using Outcome = std::variant<outcome::Done, outcome::Notice, outcome::KeyChecked, outcome::Copy, outcome::Opened,
                             outcome::Forked, outcome::Folder>;

// Carries out a panel intent on `node`; blocks on the network.
[[nodiscard]] std::expected<Outcome, std::string> apply(p2p::Node& node, panel::Intent intent);

// ---- A peer's shared folder (`folder.rs`) ----------------------------------------------------

namespace folder_msg {
// Enter the subfolder of the current folder with this name.
struct Enter {
    std::string name;
};
// Go to the folder `depth` levels below the shared folder (0 is the shared folder).
struct Crumb {
    std::size_t depth = 0;
};
struct Refresh {};
struct Listed {
    std::string path;
    std::expected<std::vector<p2p::FolderEntry>, std::string> result;
};
// Open the file of the current folder with this name.
struct Open {
    std::string name;
};
struct Opened {
    std::string path;
    std::expected<p2p::Bytes, std::string> result;
};
// The opened text as edited.
struct Edit {
    std::string text;
};
struct Save {};
struct Saved {
    std::string path;
    std::expected<void, std::string> result;
};
struct CloseFile {};
struct Close {};
} // namespace folder_msg
using FolderMessage = std::variant<folder_msg::Enter, folder_msg::Crumb, folder_msg::Refresh, folder_msg::Listed,
                                   folder_msg::Open, folder_msg::Opened, folder_msg::Edit, folder_msg::Save,
                                   folder_msg::Saved, folder_msg::CloseFile, folder_msg::Close>;

// What an opened file shows: its text, or for anything but UTF-8 text only its size.
struct Binary {
    std::size_t size = 0;
    friend bool operator==(const Binary&, const Binary&) = default;
};
using Preview = std::variant<std::string, Binary>;

struct OpenFile {
    // Relative to the shared folder.
    std::string path;
    Preview preview;
    // Edited since it was opened or saved.
    bool dirty = false;
    bool saving = false;
};

// The shared folder on screen.
struct Browser {
    std::shared_ptr<p2p::Node> node;
    std::string peer;
    std::string peer_name;
    // The granted folder, absolute on the peer.
    std::string folder;
    // The folder shown, relative to `folder`; empty for the folder itself.
    std::string path;
    std::vector<p2p::FolderEntry> entries;
    bool can_write = false;
    std::optional<OpenFile> file;
    // A listing is on its way.
    bool loading = false;
};

// `name` inside the relative folder `dir`.
[[nodiscard]] std::string join(std::string_view dir, std::string_view name);
// "12 B", "1.5 KiB", "2.0 MiB".
[[nodiscard]] std::string size_text(std::uint64_t bytes);

// ---- A peer's session (`remote.rs`) ----------------------------------------------------------

namespace remote_msg {
struct Feed {
    app_runtime::Feed feed;
};
struct Chat {
    chat::Message message;
};
struct Card {
    cards::Message message;
};
struct Input {
    std::string text;
};
// Send the typed prompt to the owner's harness.
struct Send {};
struct Sent {
    std::expected<void, std::string> result;
};
struct Answered {
    core::NodeId decision;
    std::expected<void, std::string> result;
};
struct Fork {};
struct Close {};
} // namespace remote_msg
using RemoteMessage = std::variant<remote_msg::Feed, remote_msg::Chat, remote_msg::Card, remote_msg::Input,
                                   remote_msg::Send, remote_msg::Sent, remote_msg::Answered, remote_msg::Fork,
                                   remote_msg::Close>;

// The peer's session on screen: a live, read-only mirror kept current over `omp/session/1`.
class Remote {
  public:
    // The view of `view`; `by` is this node's key, recorded on answers.
    Remote(std::shared_ptr<p2p::RemoteView> view, std::string peer_name, std::string by);

    // Whether this shows `session` of `peer`.
    [[nodiscard]] bool is(std::string_view peer, std::string_view session) const;
    // The title Recents shows: the `title` attribute, else the first prompt.
    [[nodiscard]] std::string title() const;
    [[nodiscard]] bool can_drive() const;
    void feed(app_runtime::Feed feed);

    std::shared_ptr<p2p::RemoteView> view;
    std::string peer_name;
    // The UI's copy of the mirror.
    core::Dom dom;
    chat::State chat;
    cards::State cards;
    std::string input;
    // A prompt is on its way to the owner.
    bool sending = false;
    // Why the view stopped following the owner.
    std::optional<std::string> ended;

  private:
    void reload(core::Dom dom);
    void event(const bus::Event& event);
};

// ---- The area --------------------------------------------------------------------------------

namespace msg {
struct Panel {
    screen::Message message;
};
// Show the Peers tab (the sidebar's node row).
using Open = to::peers::Open;
// Re-read the node and grants without switching the panel tab, for Settings › Peers & sharing.
using Refresh = to::peers::Refresh;
// The endpoint started, or why it didn't.
struct Started {
    std::expected<void, std::string> result;
};
// The node changed or a peer asks something of this node.
struct Node {
    p2p::NodeEvent event;
};
// A panel action finished.
struct Applied {
    std::expected<Outcome, std::string> result;
};
// SSH hosts and surfaces grants can name.
struct Resources {
    std::vector<std::pair<trust::Resource, std::string>> resources;
};
// The old endpoint stopped; open the node again with the saved settings.
struct Stopped {};
// A peer's session picked in Recents.
using OpenRecent = to::peers::OpenRecent;
struct Remote {
    RemoteMessage message;
};
struct Folder {
    FolderMessage message;
};
} // namespace msg

using Message = std::variant<msg::Panel, msg::Open, msg::Refresh, msg::Started, msg::Node, msg::Applied,
                             msg::Resources, msg::Stopped, msg::OpenRecent, msg::Remote, msg::Folder>;

// Recents id of a peer's session: `peer:<node key>:<session id>`.
[[nodiscard]] std::string recent_id(std::string_view peer, std::string_view session);
// The peer key and session id of a Recents id.
[[nodiscard]] std::optional<std::pair<std::string_view, std::string_view>> parse_recent(std::string_view id);

// The node's settings from `[network]`: the port for UDP and TCP, the relay, and which
// discovery runs.
[[nodiscard]] p2p::NodeConfig node_config(const Ctx& ctx, const settings::app::NetworkSettings& network);
// The relay a node uses under `relay`: only one the user runs; there are no public relays.
[[nodiscard]] p2p::RelayConfig relay_config(const settings::app::Relay& relay);

class State final : public TypedArea<State> {
  public:
    static constexpr std::string_view ID = area_id::PEERS;
    using Message = peers::Message;

    // Opens the node over `[network]`; the task starts its endpoint.
    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    // Opens the node with `config`; the task starts its endpoint.
    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> with_config(Ctx& ctx, p2p::NodeConfig config);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    State() = default;

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] std::vector<PanelTab> panel_tabs() const override;
    [[nodiscard]] QWidget* view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const override;
    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    // Drops the node (closing its endpoint) and opens one with `config` in its place.
    Task reopen(Ctx& ctx, p2p::NodeConfig config);

    [[nodiscard]] const std::shared_ptr<p2p::Node>& node() const noexcept { return node_; }
    [[nodiscard]] const screen::State& panel() const noexcept { return panel_; }
    [[nodiscard]] const std::optional<Remote>& remote() const noexcept { return remote_; }
    [[nodiscard]] const std::optional<Browser>& folder() const noexcept { return folder_; }
    // Recents ids of the peers' sessions listed in the sidebar.
    [[nodiscard]] const std::vector<std::string>& listed() const noexcept { return listed_; }

    // Runs a panel intent on the node.
    Task apply(panel::Intent intent, Ctx& ctx);
    // Opens the node again with the saved `[network]` settings.
    Task restart(Ctx& ctx);

  private:
    Task open(Ctx& ctx, p2p::NodeConfig config);
    void refresh(Ctx& ctx);
    [[nodiscard]] Task resources(Ctx& ctx) const;
    Task applied(Outcome outcome, Ctx& ctx);
    void node_event(p2p::NodeEvent event, Ctx& ctx);
    Task remote_update(RemoteMessage message, Ctx& ctx);
    Task folder_update(FolderMessage message, Ctx& ctx);
    [[nodiscard]] QWidget* remote_view(const Remote& remote, const Ctx& ctx, const Emit& send) const;
    [[nodiscard]] static QWidget* folder_view(const Browser& browser, const Emit& send);

    screen::State panel_;
    // Null when its stores couldn't be opened; the reason was shown.
    std::shared_ptr<p2p::Node> node_;
    // Bumped each time the node is opened again, so its event feed restarts.
    std::uint64_t generation_ = 0;
    // A peer's session shown in the center.
    std::optional<Remote> remote_;
    // A peer's shared folder shown in the center.
    std::optional<Browser> folder_;
    std::vector<std::string> listed_;
};

} // namespace loa::app::areas::peers
