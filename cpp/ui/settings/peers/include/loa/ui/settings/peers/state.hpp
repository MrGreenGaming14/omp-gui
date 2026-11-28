#pragma once

// Peers: this node, its peers and the nodes seen nearby, grants given and received (with the
// form issuing one), and the sessions and folders peers share with this node. The app supplies
// a `Data` snapshot and carries out the `Intent`s; tokens, keys and the network stay there.
// Port of `crates/loa-ui-settings/src/peers/{mod,state,grant_form}.rs`.

#include <array>
#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/kit/widgets.hpp"
#include "loa/view/settings_peers.hpp"

namespace loa::ui::settings::peers {

namespace vp = loa::view::settings::peers;
using vp::Data;
using vp::Grant;
using vp::GrantDirection;
using vp::GrantInput;
using vp::GrantResource;
using vp::GrantScope;
using vp::Intent;
using vp::Peer;
using vp::Permission;
using vp::ResourceChoice;

enum class PeerSort : std::uint8_t { Name, OnlineFirst, RecentlySeen, Version };
inline constexpr std::array<PeerSort, 4> PEER_SORTS{PeerSort::Name, PeerSort::OnlineFirst, PeerSort::RecentlySeen, PeerSort::Version};

// `peers` matching `filter` by name, key or discovery, sorted by `sort` then name and key.
[[nodiscard]] std::vector<const Peer*> visible(const std::vector<Peer>& peers, std::string_view filter, PeerSort sort);

// The resource and permission of a grant, either way it went.
[[nodiscard]] std::string grant_summary(const Grant& grant);

// Why a grant cannot be issued yet.
enum class GrantFormError : std::uint8_t { MissingResource, InvalidTtl, InvalidPermission, SshNotConfirmed };
// The variant's name, as Rust's `Debug` prints it.
[[nodiscard]] std::string_view to_string(GrantFormError e) noexcept;

// The grant being written; SSH access needs an explicit confirmation first.
struct GrantForm {
    std::optional<ResourceChoice> resource;
    Permission permission = Permission::Fork;
    std::string ttl_hours = "24";
    bool reshare = false;
    bool future_sessions = false;
    bool ssh_confirmed = false;
    // A folder of this node typed in to grant; the app checks it exists.
    std::string folder_path;

    // Another resource drops the SSH consent, the future sessions choice and the permission.
    void select_resource(std::optional<ResourceChoice> resource);
    // The typed folder as a resource; `std::nullopt` while empty.
    [[nodiscard]] std::optional<ResourceChoice> folder_choice() const;
    // Whether the resource reaches this node's SSH connection.
    [[nodiscard]] bool is_ssh() const noexcept;
    // The exact issuance request for `peer_key`, without minting anything.
    [[nodiscard]] std::expected<GrantInput, GrantFormError> build(std::string_view peer_key) const;
};

// A step waiting for the user's confirmation.
namespace confirm {
struct RemovePeer {
    std::string key;
    friend bool operator==(const RemovePeer&, const RemovePeer&) = default;
};
struct RevokeGrant {
    std::string id;
    friend bool operator==(const RevokeGrant&, const RevokeGrant&) = default;
};
// SSH access in the grant being written.
struct Ssh {
    friend bool operator==(const Ssh&, const Ssh&) = default;
};
} // namespace confirm
using Confirm = std::variant<confirm::RemovePeer, confirm::RevokeGrant, confirm::Ssh>;

// Message payloads compare equal field by field (`Msg` is comparable).
#define LOA_PEERS_EQ(T) friend bool operator==(const T&, const T&) = default;
namespace local {
struct Filter { std::string value; LOA_PEERS_EQ(Filter) };
struct Sort { PeerSort sort = PeerSort::Name; LOA_PEERS_EQ(Sort) };
struct Key { std::string value; LOA_PEERS_EQ(Key) };
struct Address { std::string value; LOA_PEERS_EQ(Address) };
struct AddPeer { LOA_PEERS_EQ(AddPeer) };
struct Ask { peers::Confirm confirm; LOA_PEERS_EQ(Ask) };
struct Confirm { LOA_PEERS_EQ(Confirm) };
// Drops the pending confirmation and rename.
struct Cancel { LOA_PEERS_EQ(Cancel) };
struct BeginRename { std::string key; LOA_PEERS_EQ(BeginRename) };
struct RenameText { std::string name; LOA_PEERS_EQ(RenameText) };
struct SaveRename { LOA_PEERS_EQ(SaveRename) };
struct SelectPeer { std::string key; LOA_PEERS_EQ(SelectPeer) };
struct ResourceFilter { std::string value; LOA_PEERS_EQ(ResourceFilter) };
struct SelectResource { std::string id; LOA_PEERS_EQ(SelectResource) };
struct SetPermission { Permission permission = Permission::Fork; LOA_PEERS_EQ(SetPermission) };
struct Ttl { std::string value; LOA_PEERS_EQ(Ttl) };
struct Reshare { bool on = false; LOA_PEERS_EQ(Reshare) };
struct FutureSessions { bool on = false; LOA_PEERS_EQ(FutureSessions) };
struct FolderPath { std::string path; LOA_PEERS_EQ(FolderPath) };
// Picks the typed folder as the grant's resource.
struct UseFolder { LOA_PEERS_EQ(UseFolder) };
struct SubmitGrant { LOA_PEERS_EQ(SubmitGrant) };
} // namespace local
#undef LOA_PEERS_EQ

using Local = std::variant<local::Filter, local::Sort, local::Key, local::Address, local::AddPeer, local::Ask, local::Confirm,
                           local::Cancel, local::BeginRename, local::RenameText, local::SaveRename, local::SelectPeer,
                           local::ResourceFilter, local::SelectResource, local::SetPermission, local::Ttl, local::Reshare,
                           local::FutureSessions, local::FolderPath, local::UseFolder, local::SubmitGrant>;
using Message = loa::ui::kit::widgets::Msg<Local, Intent>;

// The panel's interaction state and reducer; requests reach the app only while the snapshot
// still backs them.
struct State : loa::ui::kit::widgets::Screen<Local, Intent> {
    Data data;
    std::string filter;
    PeerSort sort = PeerSort::Name;
    std::string add_key;
    std::string add_address;
    // The app's verdict on a typed key, tied to the exact text it checked.
    std::optional<std::pair<std::string, bool>> key_validation;
    std::optional<std::string> selected_peer;
    std::string resource_filter;
    GrantForm grant_form;
    std::optional<Confirm> confirm;
    // The peer being renamed and the name typed so far.
    std::optional<std::pair<std::string, std::string>> rename;

    // Takes a new snapshot, keeping the input that it still backs.
    void set_data(Data data);
    // Takes the app's verdict on `key` while it is still the typed one.
    void set_key_validation(std::string_view key, bool valid);
    [[nodiscard]] std::vector<const Peer*> visible_peers() const;
    // Nearby nodes not added yet, filtered and sorted like the peers.
    [[nodiscard]] std::vector<const Peer*> visible_nearby() const;
    [[nodiscard]] bool key_valid() const;

    std::optional<Intent> reduce(Local m) override;
    // Direct requests hold while the snapshot lists what they name.
    [[nodiscard]] bool guard(const Intent& e) const override;

  private:
    [[nodiscard]] bool is_peer(std::string_view key) const;
    [[nodiscard]] bool given(std::string_view id) const;
    void select_resource(std::optional<ResourceChoice> resource);
};

} // namespace loa::ui::settings::peers

namespace loa::core {
template <>
struct StrEnum<loa::ui::settings::peers::PeerSort> {
    static constexpr std::array<std::string_view, 4> values{"Name", "Online first", "Recently seen", "App version"};
};
} // namespace loa::core
