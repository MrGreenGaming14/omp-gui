#pragma once

// Long-lived services created once at startup and shared by every area through `Ctx` (port of
// `loa-app/src/services.rs`, `services/host.rs` and `services/harness.rs`).

#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <shared_mutex>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/app/usage.hpp"
#include "loa/app/workspace.hpp"
#include "loa/app_runtime/harnesses.hpp"
#include "loa/bus/writer.hpp"
#include "loa/mcp/manager.hpp"
#include "loa/session/library.hpp"
#include "loa/settings/app/store.hpp"
#include "loa/trust/key.hpp"
#include "loa/trust/keystore.hpp"
#include "loa/trust/store.hpp"
#include "loa/view/session.hpp"

namespace loa::app {

// The access check every session writer asks about peer requests; refuses all until `set`.
class PeerAccess final : public bus::Authorize {
  public:
    PeerAccess();
    void set(std::shared_ptr<const bus::Authorize> authorize);
    [[nodiscard]] std::expected<void, bus::Denied> authorize(const bus::SessionId& session, const bus::NodeKey& requester,
                                                             const core::Draft& draft,
                                                             const core::Dom& dom) const override;

  private:
    mutable std::shared_mutex mutex_;
    std::shared_ptr<const bus::Authorize> inner_;
};

// This node: its identity, the grants it issued and received, and the MCP servers it runs.
struct NodeServices {
    trust::NodeIdentity identity;
    // Public key as text, written to `<session node>`.
    std::string key;
    // Human name, the machine's host name.
    std::string label;
    trust::KeySource key_source;
    // Decides peer requests on sessions this node owns.
    std::shared_ptr<PeerAccess> peer_access;
    // `<data>/grants.db`.
    std::shared_ptr<trust::GrantStore> grants;
    std::shared_ptr<mcp::Manager> mcp;

    // The node over `identity`, its grant store in `data` (created on a first run).
    [[nodiscard]] static std::expected<NodeServices, std::string> open(trust::NodeIdentity identity, std::string label,
                                                                       trust::KeySource key_source,
                                                                       const std::filesystem::path& data);
    // The node key the session bus checks writers against.
    [[nodiscard]] bus::NodeKey bus_key() const;
};

// Where the app keeps its data.
struct Dirs {
    // `$XDG_DATA_HOME/loa`, else `~/.local/share/loa`.
    std::filesystem::path data;
    // One journal directory per session.
    std::filesystem::path sessions;
    // omp's per session state (memory overlay, context files).
    std::filesystem::path omp;

    [[nodiscard]] static Dirs make(std::filesystem::path data);
};

// The file names under `Dirs::data`; their layout matches the Rust app.
inline constexpr std::string_view CONFIG_FILE = "config.toml";
inline constexpr std::string_view GRANTS_FILE = "grants.db";

// What the app keeps on disk besides the session library: the central config.
struct Storage {
    Dirs dirs;
    // `<data>/config.toml`; each area saves only the table it owns.
    settings::app::ConfigStore config;

    [[nodiscard]] static Storage make(Dirs dirs);
};

// The external programs the app asks: git, the desktop folder dialog, `omp usage`.
struct Host {
    std::shared_ptr<const GitProbe> git;
    std::shared_ptr<const FolderPicker> picker;
    std::shared_ptr<const UsageSource> usage;

    // The command line adapters of this machine.
    [[nodiscard]] static Host system();
};

// Name of this machine: the kernel host name, else `HOSTNAME` or `COMPUTERNAME`, else "this device".
[[nodiscard]] std::string hostname();
// The app's data directory, `loa` under the platform data dir, else under the temp directory.
[[nodiscard]] std::filesystem::path data_dir();

struct Services {
    NodeServices node;
    Storage storage;
    // The processes the app asks about the machine: git, the folder dialog, `omp usage`.
    Host host;
    // The folder the app was launched in, the workspace project's folder.
    std::filesystem::path workspace;

    // The services of this machine, with warnings to show the user. `first_arg` is the first
    // command line argument.
    [[nodiscard]] static std::expected<std::pair<Services, std::vector<std::string>>, std::string> load(
        std::optional<std::string> first_arg);
    // `load` with an explicit data directory and identity loader, for tests.
    using LoadIdentity = std::function<trust::Result<std::pair<trust::NodeIdentity, trust::KeySource>>(
        const std::filesystem::path&)>;
    [[nodiscard]] static std::expected<std::pair<Services, std::vector<std::string>>, std::string> load_in(
        std::filesystem::path data, const LoadIdentity& load_identity, std::optional<std::string> first_arg, Host host);
    // Services over explicit parts, launched in `workspace`; peer requests are refused until grants exist.
    [[nodiscard]] static Services make(NodeServices node, Dirs dirs, Host host, std::filesystem::path workspace);
};

// Opens the session store in `dirs`, its writers checked by `node`.
[[nodiscard]] std::expected<std::shared_ptr<session::Library>, std::string> open_library(const Dirs& dirs,
                                                                                         const NodeServices& node);

// The folder the app starts in: the folder given as the first argument, else the temp folder.
[[nodiscard]] std::filesystem::path starting_folder(std::optional<std::string> arg, std::filesystem::path temp);

// The runtime's harnesses, built from the `loa::harness::factories` registry, and the
// composer's picker over them.
namespace harness {
// Picker note while a harness's probe hasn't answered.
inline constexpr std::string_view CHECKING = "checking…";

// Registers every harness of the registry on its default executable.
[[nodiscard]] app_runtime::Harnesses detect(std::string_view node, const std::filesystem::path& omp_dir);
// Registers `adapter` and lists it as `label`, `available` (or not, for `detail`) until
// `Harnesses::set_availability` reports a probe; a harness the registry knows gets its traits
// and default program.
void add(app_runtime::Harnesses& harnesses, std::shared_ptr<contracts::Adapter> adapter, std::string_view label,
         bool available, std::optional<std::string> detail, std::optional<std::filesystem::path> executable);
// (Re)registers the known harness `id` running `executable`, or its default when `nullopt`.
bool set_executable(app_runtime::Harnesses& harnesses, std::string_view id,
                    std::optional<std::filesystem::path> executable, std::string_view node,
                    const std::filesystem::path& omp_dir);
// Registers each known harness of `executables` (`[harness] executables`); the ids it registered.
std::vector<std::string> set_executables(app_runtime::Harnesses& harnesses,
                                         const std::map<std::string, std::filesystem::path>& executables,
                                         std::string_view node, const std::filesystem::path& omp_dir);
// The picker options, in display order.
[[nodiscard]] std::vector<loa::view::session::composer::HarnessOption> options(const app_runtime::Harnesses& harnesses);
} // namespace harness

} // namespace loa::app
