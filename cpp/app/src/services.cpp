#include "loa/app/services.hpp"

#include <cstdlib>
#include <fstream>
#include <mutex>
#include <sstream>

#include "loa/bus/bus.hpp"
#include "loa/bus/store.hpp"
#include "loa/harness/registry/registry.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::app {

namespace {

// Refuses every peer request: no grants are issued on this node.
class NoGrants final : public bus::Authorize {
  public:
    [[nodiscard]] std::expected<void, bus::Denied> authorize(const bus::SessionId&, const bus::NodeKey&,
                                                             const core::Draft&, const core::Dom&) const override {
        return std::unexpected(bus::Denied{"this node has issued no grants"});
    }
};

std::string trim(std::string s) {
    const auto first = s.find_first_not_of(" \t\r\n");
    if (first == std::string::npos) return {};
    const auto last = s.find_last_not_of(" \t\r\n");
    return s.substr(first, last - first + 1);
}

} // namespace

// ---- PeerAccess ------------------------------------------------------------------------------

PeerAccess::PeerAccess() : inner_(std::make_shared<NoGrants>()) {}

void PeerAccess::set(std::shared_ptr<const bus::Authorize> authorize) {
    std::unique_lock lock(mutex_);
    inner_ = std::move(authorize);
}

std::expected<void, bus::Denied> PeerAccess::authorize(const bus::SessionId& session, const bus::NodeKey& requester,
                                                      const core::Draft& draft, const core::Dom& dom) const {
    std::shared_ptr<const bus::Authorize> inner;
    {
        std::shared_lock lock(mutex_);
        inner = inner_;
    }
    return inner->authorize(session, requester, draft, dom);
}

// ---- NodeServices ----------------------------------------------------------------------------

std::expected<NodeServices, std::string> NodeServices::open(trust::NodeIdentity identity, std::string label,
                                                            trust::KeySource key_source,
                                                            const std::filesystem::path& data) {
    // On a first run nothing has created the data directory yet.
    std::error_code ec;
    std::filesystem::create_directories(data, ec);
    if (ec) return std::unexpected("Can't create the data directory " + data.string() + ": " + ec.message());
    const auto grants_path = data / GRANTS_FILE;
    auto grants = trust::GrantStore::open(grants_path);
    if (!grants)
        return std::unexpected("Can't open the grant store " + grants_path.string() + ": " + grants.error().to_string());
    auto mcp = mcp::Manager::make(mcp::Options{});
    if (!mcp) return std::unexpected("Can't set up the MCP client: " + mcp.error().message);
    auto key = identity.public_key().to_string();
    return NodeServices{std::move(identity),
                        std::move(key),
                        std::move(label),
                        std::move(key_source),
                        std::make_shared<PeerAccess>(),
                        std::make_shared<trust::GrantStore>(std::move(*grants)),
                        std::make_shared<mcp::Manager>(std::move(*mcp))};
}

bus::NodeKey NodeServices::bus_key() const { return trust::to_bus(identity.public_key()); }

// ---- Dirs, Storage, Host ---------------------------------------------------------------------

Dirs Dirs::make(std::filesystem::path data) {
    Dirs dirs{.data = std::move(data), .sessions = {}, .omp = {}};
    dirs.sessions = dirs.data / "sessions";
    dirs.omp = dirs.data / "omp";
    return dirs;
}

Storage Storage::make(Dirs dirs) {
    auto config = settings::app::ConfigStore::in_dir(dirs.data);
    return Storage{std::move(dirs), std::move(config)};
}

Host Host::system() {
    return Host{std::make_shared<GitCli>(), std::make_shared<DialogPicker>(), std::make_shared<OmpUsage>()};
}

std::string hostname() {
    for (const char* path : {"/proc/sys/kernel/hostname", "/etc/hostname"}) {
        std::ifstream in(path);
        if (!in) continue;
        std::stringstream text;
        text << in.rdbuf();
        if (auto name = trim(text.str()); !name.empty()) return name;
    }
    for (const char* key : {"HOSTNAME", "COMPUTERNAME"}) {
        if (const char* value = std::getenv(key))
            if (auto name = trim(value); !name.empty()) return name;
    }
    return "this device";
}

std::filesystem::path data_dir() {
    if (auto dir = loa::runtime::fs::data_dir("loa")) return *dir;
    return std::filesystem::temp_directory_path() / "loa";
}

// ---- Services --------------------------------------------------------------------------------

std::expected<std::pair<Services, std::vector<std::string>>, std::string> Services::load(
    std::optional<std::string> first_arg) {
    return load_in(data_dir(), trust::load_app_identity, std::move(first_arg), Host::system());
}

std::expected<std::pair<Services, std::vector<std::string>>, std::string> Services::load_in(
    std::filesystem::path data, const LoadIdentity& load_identity, std::optional<std::string> first_arg, Host host) {
    std::vector<std::string> warnings;
    auto dirs = Dirs::make(std::move(data));
    auto loaded = load_identity(dirs.data);
    std::optional<std::pair<trust::NodeIdentity, trust::KeySource>> identity;
    if (loaded) {
        identity.emplace(std::move(*loaded));
    } else {
        const auto error = loaded.error().to_string();
        warnings.push_back("Neither the OS keychain nor the key file works (" + error +
                           "); this node uses a temporary key until restart");
        auto generated = trust::NodeIdentity::generate();
        if (!generated) return std::unexpected("Can't create a node key: " + generated.error().to_string());
        identity.emplace(std::move(*generated), trust::key_source::Memory{error});
    }
    auto node = NodeServices::open(std::move(identity->first), hostname(), std::move(identity->second), dirs.data);
    if (!node) return std::unexpected(node.error());
    auto cwd = starting_folder(std::move(first_arg), std::filesystem::temp_directory_path());
    return std::pair{make(std::move(*node), std::move(dirs), std::move(host), std::move(cwd)), std::move(warnings)};
}

Services Services::make(NodeServices node, Dirs dirs, Host host, std::filesystem::path workspace) {
    return Services{std::move(node), Storage::make(std::move(dirs)), std::move(host), std::move(workspace)};
}

std::expected<std::shared_ptr<session::Library>, std::string> open_library(const Dirs& dirs, const NodeServices& node) {
    auto store = bus::Store::make(dirs.sessions, bus::JournalOptions{});
    if (!store)
        return std::unexpected("Can't open the session store " + dirs.sessions.string() + ": " + bus::to_string(store.error()));
    return std::make_shared<session::Library>(std::move(*store), std::make_shared<bus::Bus>(), node.bus_key(),
                                              node.peer_access);
}

std::filesystem::path starting_folder(std::optional<std::string> arg, std::filesystem::path temp) {
    if (!arg) return temp;
    std::filesystem::path path(*arg);
    std::error_code ec;
    if (!std::filesystem::is_directory(path, ec)) return temp;
    auto canonical = std::filesystem::canonical(path, ec);
    if (ec) return temp;
    return canonical;
}

// ---- harness ---------------------------------------------------------------------------------

namespace harness {
namespace {

std::shared_ptr<contracts::HarnessFactory> factory(std::string_view id, const loa::harness::HarnessEnv& env) {
    for (auto& each : loa::harness::factories(env))
        if (each->id() == id) return each;
    return nullptr;
}

void register_factory(app_runtime::Harnesses& harnesses, const contracts::HarnessFactory& factory,
                      std::optional<std::filesystem::path> executable) {
    std::optional<std::filesystem::path> program = executable;
    if (!program) {
        auto text = factory.default_program();
        auto first = text.substr(0, text.find_first_of(" \t"));
        if (!first.empty()) program = std::filesystem::path(std::string(first));
    }
    if (factory.traits().server_url) program.reset();
    std::optional<std::filesystem::path> found;
    if (program) found = loa::runtime::exec::which(*program);
    auto adapter = factory.build(executable);
    if (!adapter) return;
    add(harnesses, std::move(adapter), factory.label(), found.has_value(), std::string(CHECKING), found);
    for (auto& harness : harnesses.list)
        if (harness.id == factory.id()) harness.configured = executable;
}

} // namespace

app_runtime::Harnesses detect(std::string_view node, const std::filesystem::path& omp_dir) {
    app_runtime::Harnesses harnesses;
    for (const auto& each : loa::harness::factories({std::string(node), omp_dir}))
        register_factory(harnesses, *each, std::nullopt);
    return harnesses;
}

void add(app_runtime::Harnesses& harnesses, std::shared_ptr<contracts::Adapter> adapter, std::string_view label,
         bool available, std::optional<std::string> detail, std::optional<std::filesystem::path> executable) {
    std::string id(adapter->harness());
    auto known = factory(id, {});
    app_runtime::Harness harness{
        .id = id,
        .label = std::string(label),
        .available = available,
        .detail = std::move(detail),
        .executable = std::move(executable),
        .configured = std::nullopt,
        .availability = std::nullopt,
        .traits = known ? known->traits() : contracts::HarnessTraits{},
        .program = known ? known->default_program() : std::string_view{},
    };
    harnesses.add(std::move(adapter), std::move(harness));
}

bool set_executable(app_runtime::Harnesses& harnesses, std::string_view id,
                    std::optional<std::filesystem::path> executable, std::string_view node,
                    const std::filesystem::path& omp_dir) {
    auto known = factory(id, {std::string(node), omp_dir});
    if (!known) return false;
    register_factory(harnesses, *known, std::move(executable));
    return true;
}

std::vector<std::string> set_executables(app_runtime::Harnesses& harnesses,
                                         const std::map<std::string, std::filesystem::path>& executables,
                                         std::string_view node, const std::filesystem::path& omp_dir) {
    std::vector<std::string> set;
    for (const auto& [id, executable] : executables)
        if (set_executable(harnesses, id, executable, node, omp_dir)) set.push_back(id);
    return set;
}

std::vector<loa::view::session::composer::HarnessOption> options(const app_runtime::Harnesses& harnesses) {
    std::vector<loa::view::session::composer::HarnessOption> out;
    for (const auto& harness : harnesses.list) {
        loa::view::session::composer::HarnessOption option;
        option.id = harness.id;
        option.label = harness.label;
        option.available = harness.available;
        option.detail = harness.detail;
        out.push_back(std::move(option));
    }
    return out;
}

} // namespace harness

} // namespace loa::app
