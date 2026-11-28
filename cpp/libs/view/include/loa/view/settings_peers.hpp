#pragma once

// The Peers panel: this node, its peers and the nodes seen nearby, grants given
// and received, and the sessions and folders peers share with this node. Tokens,
// keys and the network stay with the app.

#include <array>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <variant>
#include <vector>

#include "loa/core/value.hpp"

namespace loa::view::settings::peers {

// How the app found a node.
enum class Discovery : std::uint8_t { Lan, Tailscale, AddedByKey, Incoming };
enum class Presence : std::uint8_t { Online, Offline };
// The route of the connection in use; `None` when there is none.
enum class Transport : std::uint8_t { Direct, Relay, TcpFallback, None };
// A peer's app version against this one; protocol compatibility is reported apart.
enum class VersionRelation : std::uint8_t { Behind, Ahead, Current, Unknown };

// Major, minor and patch of an app version, ignoring a `v` prefix, pre-release and build parts.
using VersionParts = std::tuple<std::uint64_t, std::uint64_t, std::uint64_t>;

// A peer, or a node seen nearby.
struct Peer {
    std::string key;
    std::string name;
    Discovery discovery = Discovery::Lan;
    Presence presence = Presence::Offline;
    std::optional<std::uint64_t> last_seen;
    Transport transport = Transport::None;
    std::string app_version;
    // Whether a protocol is shared; `std::nullopt` until the app talked to the node.
    std::optional<bool> shared_protocol;

    // A recognizable prefix of the key (12 characters); copying takes the whole key.
    [[nodiscard]] std::string_view short_key() const noexcept;
    [[nodiscard]] std::optional<VersionParts> version() const;
    // Compares major, minor and patch with `local`, ignoring pre-release and build parts.
    [[nodiscard]] VersionRelation version_relation(std::string_view local) const;
    // When the peer was last seen, against the app's clock `now` (seconds).
    [[nodiscard]] std::string last_seen_text(std::uint64_t now) const;

    friend bool operator==(const Peer&, const Peer&) = default;
};

// This node as the panel shows it.
struct NodeCard {
    std::string key;
    std::string name;
    // Where the node key lives, e.g. "OS keychain".
    std::string key_source;
    // The endpoint's state, e.g. "UDP 0.0.0.0:31415 · TCP 0.0.0.0:31415".
    std::string endpoint;
    bool endpoint_ok = false;
    // LAN (mDNS) discovery state.
    std::string lan;
    bool lan_ok = false;
    std::string tailscale;
    bool tailscale_ok = false;
    // The tailnet can be scanned now.
    bool tailscale_available = false;

    friend bool operator==(const NodeCard&, const NodeCard&) = default;
};

// A peer's session this node's grants reach.
struct RemoteItem {
    std::string peer_key;
    std::string peer_name;
    std::string session;
    // Title, or the session id when the grant hides it.
    std::string title;
    std::string harness;
    // What the grants allow, e.g. "view, drive".
    std::string access;
    bool can_drive = false;
    bool can_fork = false;
    bool online = false;

    friend bool operator==(const RemoteItem&, const RemoteItem&) = default;
};

// A peer's folder this node's grants reach.
struct SharedFolderItem {
    std::string peer_key;
    std::string peer_name;
    // The folder's path on the peer.
    std::string path;
    bool can_write = false;
    bool online = false;

    friend bool operator==(const SharedFolderItem&, const SharedFolderItem&) = default;
};

// What a grant protects.
enum class GrantScope : std::uint8_t { Session, Prompt, Project, FutureSessions, SshHost, Surface, Sandbox, Folder };

// A resource the app offers to grant.
struct ResourceChoice {
    std::string id;
    std::string label;
    GrantScope scope = GrantScope::Session;

    friend bool operator==(const ResourceChoice&, const ResourceChoice&) = default;
};

// A granted resource; a prompt can also cover every future session.
struct GrantResource {
    std::string id;
    std::string label;
    GrantScope scope = GrantScope::Session;
    bool future_sessions = false;

    friend bool operator==(const GrantResource&, const GrantResource&) = default;
};

// What the holder may do with the resource.
enum class Permission : std::uint8_t { View, Suggest, Drive, Fork };

// Surfaces are viewed or driven (input); SSH hosts and folders read (view) or also
// written, and on SSH run (drive); a sandbox only runs tasks (drive).
[[nodiscard]] bool allowed_for(Permission permission, GrantScope scope) noexcept;

// Whether this node issued or received the grant.
enum class GrantDirection : std::uint8_t { Given, Received };

// A grant between this node and one peer.
struct Grant {
    std::string id;
    std::string peer_key;
    GrantDirection direction = GrantDirection::Given;
    GrantResource resource;
    Permission permission = Permission::Fork;
    // When the grant ends; `std::nullopt` renews until revoked.
    std::optional<std::uint64_t> expires_at;
    // The next automatic renewal, if known.
    std::optional<std::uint64_t> renews_at;
    bool reshare = false;
    // Revoked by this node, or refused renewal by its issuer.
    bool ended = false;

    // The expiry and the time to the next renewal against the app's clock `now` (seconds).
    [[nodiscard]] std::string expiry_text(std::uint64_t now) const;

    friend bool operator==(const Grant&, const Grant&) = default;
};

// A validated request for the app's grant issuer.
struct GrantInput {
    std::string peer_key;
    GrantResource resource;
    Permission permission = Permission::Fork;
    std::uint64_t ttl_seconds = 0;
    bool reshare = false;

    friend bool operator==(const GrantInput&, const GrantInput&) = default;
};

// The app's snapshot the panel shows.
struct Data {
    NodeCard node;
    std::vector<Peer> peers;
    std::vector<Peer> nearby;
    std::vector<Grant> grants;
    std::vector<ResourceChoice> resources;
    std::vector<RemoteItem> remote_sessions;
    std::vector<SharedFolderItem> shared_folders;
    std::string local_version;
    // Current UTC instant in seconds; the app refreshes it to update countdowns.
    std::uint64_t now = 0;

    friend bool operator==(const Data&, const Data&) = default;
};

// A request for the app's peer, clipboard or grant services.
namespace intent {
struct ValidateKey {
    std::string key;
    friend bool operator==(const ValidateKey&, const ValidateKey&) = default;
};
struct AddPeer {
    std::string key;
    std::optional<std::string> address;
    friend bool operator==(const AddPeer&, const AddPeer&) = default;
};
struct AddDiscovered {
    std::string key;
    friend bool operator==(const AddDiscovered&, const AddDiscovered&) = default;
};
struct CopyKey {
    std::string key;
    friend bool operator==(const CopyKey&, const CopyKey&) = default;
};
struct RemovePeer {
    std::string key;
    friend bool operator==(const RemovePeer&, const RemovePeer&) = default;
};
struct RenamePeer {
    std::string key;
    std::string name;
    friend bool operator==(const RenamePeer&, const RenamePeer&) = default;
};
struct CreateGrant {
    GrantInput input;
    friend bool operator==(const CreateGrant&, const CreateGrant&) = default;
};
struct RevokeGrant {
    std::string id;
    friend bool operator==(const RevokeGrant&, const RevokeGrant&) = default;
};
// Extend a given grant by its original duration and resend it.
struct RenewGrant {
    std::string id;
    friend bool operator==(const RenewGrant&, const RenewGrant&) = default;
};
// Probe every peer and refresh what they share.
struct Refresh {
    friend bool operator==(const Refresh&, const Refresh&) = default;
};
struct ScanTailscale {
    friend bool operator==(const ScanTailscale&, const ScanTailscale&) = default;
};
// Open a peer's session in the center view.
struct OpenRemote {
    std::string peer;
    std::string session;
    friend bool operator==(const OpenRemote&, const OpenRemote&) = default;
};
// Copy a peer's session into a new session on this node.
struct ForkRemote {
    std::string peer;
    std::string session;
    friend bool operator==(const ForkRemote&, const ForkRemote&) = default;
};
// Browse a peer's shared folder in the center view.
struct BrowseFolder {
    std::string peer;
    std::string folder;
    friend bool operator==(const BrowseFolder&, const BrowseFolder&) = default;
};
} // namespace intent

using Intent = std::variant<intent::ValidateKey, intent::AddPeer, intent::AddDiscovered, intent::CopyKey,
                            intent::RemovePeer, intent::RenamePeer, intent::CreateGrant, intent::RevokeGrant,
                            intent::RenewGrant, intent::Refresh, intent::ScanTailscale, intent::OpenRemote,
                            intent::ForkRemote, intent::BrowseFolder>;

} // namespace loa::view::settings::peers

// `loa::core::as_str` and `parse` texts (Rust `str_enum!`).
namespace loa::core {
template <>
struct StrEnum<loa::view::settings::peers::Discovery> {
    static constexpr std::array<std::string_view, 4> values{"LAN (mDNS)", "Tailscale", "Added by key",
                                                           "Connected to this node"};
};
template <>
struct StrEnum<loa::view::settings::peers::Presence> {
    static constexpr std::array<std::string_view, 2> values{"online", "offline"};
};
template <>
struct StrEnum<loa::view::settings::peers::Transport> {
    static constexpr std::array<std::string_view, 4> values{"direct", "relay", "TCP fallback", "disconnected"};
};
template <>
struct StrEnum<loa::view::settings::peers::VersionRelation> {
    static constexpr std::array<std::string_view, 4> values{"behind", "ahead", "current", "version unknown"};
};
template <>
struct StrEnum<loa::view::settings::peers::Permission> {
    static constexpr std::array<std::string_view, 4> values{"view", "suggest", "drive", "fork"};
};
template <>
struct StrEnum<loa::view::settings::peers::GrantDirection> {
    static constexpr std::array<std::string_view, 2> values{"Given", "Received"};
};
} // namespace loa::core
