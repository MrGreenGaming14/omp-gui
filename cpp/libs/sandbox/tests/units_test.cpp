#include <doctest/doctest.h>

#include <archive.h>
#include <archive_entry.h>

#include <asio.hpp>

#include <fstream>
#include <memory>
#include <sstream>

#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/sandbox/providers.hpp"
#include "loa/sandbox/runs.hpp"
#include "loa/sandbox/service.hpp"
#include "loa/sandbox/sync.hpp"
#include "loa/sandbox/tool.hpp"
#include "loa/sandbox/worker.hpp"

namespace fs = std::filesystem;
namespace sb = loa::sandbox;
namespace sd = loa::sandbox::sync::detail;
namespace json = loa::runtime::json;

namespace {

template <typename T>
T block_on(loa::contracts::Awaitable<T> task) {
    asio::io_context io;
    std::optional<T> out;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> { out = co_await std::move(task); }, asio::detached);
    io.run();
    REQUIRE(out);
    return std::move(*out);
}

void write_file(const fs::path& path, std::string_view text) { std::ofstream(path, std::ios::binary) << text; }

std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream text;
    text << in.rdbuf();
    return text.str();
}

// A GNU tar of empty members (Rust `tar::Builder` with `Header::new_gnu`).
std::string tarball(const std::vector<std::pair<std::string, unsigned>>& entries) {
    std::string out;
    std::unique_ptr<struct archive, decltype(&archive_write_free)> writer(archive_write_new(), archive_write_free);
    archive_write_set_format_gnutar(writer.get());
    archive_write_open(
        writer.get(), &out, nullptr,
        [](struct archive*, void* data, const void* buf, size_t len) -> la_ssize_t {
            static_cast<std::string*>(data)->append(static_cast<const char*>(buf), len);
            return static_cast<la_ssize_t>(len);
        },
        nullptr);
    for (const auto& [path, type] : entries) {
        archive_entry* entry = archive_entry_new();
        archive_entry_set_pathname(entry, path.c_str());
        archive_entry_set_filetype(entry, type);
        archive_entry_set_perm(entry, 0644);
        archive_entry_set_size(entry, 0);
        archive_write_header(writer.get(), entry);
        archive_entry_free(entry);
    }
    archive_write_close(writer.get());
    return out;
}

constexpr bool WINDOWS = false;

} // namespace

TEST_CASE("sandbox kinds keep the Rust names and order") {
    std::vector<std::string_view> names;
    for (const auto kind : sb::SANDBOX_KINDS) names.push_back(sb::as_str(kind));
    CHECK(names == std::vector<std::string_view>{"local", "docker", "firecracker", "vm", "ssh", "peer"});
    CHECK(sb::parse_kind("vm") == sb::SandboxKind::Vm);
    CHECK_FALSE(sb::parse_kind("qemu"));
}

TEST_CASE("SandboxConfig JSON round-trips with the Rust field names") {
    const auto defaults = sb::SandboxConfig{}.to_json();
    CHECK(json::dump(defaults) ==
          R"({"docker_image":"debian:stable-slim","docker_network":false,"firecracker_kernel":null,"firecracker_rootfs":null,"vm_disk":null,"vm_kernel":null,"ssh_hosts":[]})");
    sb::SandboxConfig config;
    config.docker_image = "alpine";
    config.docker_network = true;
    config.firecracker_kernel = "/k";
    config.firecracker_rootfs = "/r";
    config.vm_disk = "/d.img";
    config.vm_kernel = "/vmlinuz";
    config.ssh_hosts = {"a", "user@b"};
    auto back = sb::SandboxConfig::from_json(config.to_json());
    REQUIRE(back);
    CHECK(*back == config);

    // `#[serde(default)]`: missing fields keep their defaults, unknown ones are ignored.
    auto partial = sb::SandboxConfig::from_json(json::parse(R"({"ssh_hosts":["h"],"future":1})").value());
    REQUIRE(partial);
    CHECK(partial->docker_image == "debian:stable-slim");
    CHECK(partial->ssh_hosts == std::vector<std::string>{"h"});
    auto wrong = sb::SandboxConfig::from_json(json::parse(R"({"docker_network":"yes"})").value());
    REQUIRE_FALSE(wrong);
    CHECK(wrong.error().message().starts_with("invalid sandbox settings: "));

    auto dir = loa::runtime::TempDir::create().value();
    const auto path = dir.path() / "sandbox.json";
    CHECK(sb::SandboxConfig::load(path).value() == sb::SandboxConfig{});
    REQUIRE(config.save(path));
    CHECK(sb::SandboxConfig::load(path).value() == config);
    CHECK(read_file(path).starts_with("{\n  \"docker_image\": \"alpine\""));
}

TEST_CASE("patch_files counts lines per file and spots binary changes") {
    const std::string patch =
        "diff --git a/notes/hello.txt b/notes/hello.txt\nnew file mode 100644\n--- /dev/null\n+++ b/notes/hello.txt\n"
        "@@ -0,0 +1,2 @@\n+one\n+two\n"
        "diff --git a/tracked.txt b/tracked.txt\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-original\n+changed\n"
        "diff --git a/img.png b/img.png\nGIT binary patch\nliteral 3\n";
    const auto files = sb::patch_files(patch);
    REQUIRE(files.size() == 3);
    CHECK(files[0] == sb::PatchFile{"notes/hello.txt", 2, 0, false});
    CHECK(files[1] == sb::PatchFile{"tracked.txt", 1, 1, false});
    CHECK(files[2] == sb::PatchFile{"img.png", 0, 0, true});
    CHECK(sb::patch_files("").empty());
}

TEST_CASE("the run log follows events and tracks applied patches") {
    sb::RunLog log;
    sb::SandboxRun run{1, "parent", "parent-sandbox-1", sb::SandboxKind::Local, "task", true, "/ws"};
    log.record(sb::event::Started{run});
    log.record(sb::event::Started{sb::SandboxRun{2, "parent", "w2", sb::SandboxKind::Docker, "t", false, "/ws"}});
    CHECK(log.running() == 2);
    log.record(sb::event::Finished{1, "done", "diff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n"});
    log.record(sb::event::Failed{2, "boom"});
    log.record(sb::event::Failed{9, "unknown run"});
    CHECK(log.running() == 0);
    REQUIRE(log.entries().size() == 2);
    const auto* first = log.get(1);
    REQUIRE(first);
    CHECK(first->pending_patch().has_value());
    const auto& finished = std::get<sb::run_state::Finished>(first->state);
    CHECK(finished.files == std::vector<sb::PatchFile>{{"x", 1, 1, false}});
    log.mark_applied(1);
    CHECK_FALSE(log.get(1)->pending_patch());
    CHECK(std::get<sb::run_state::Failed>(log.get(2)->state).error == "boom");
}

TEST_CASE("sandbox_task arguments decode like serde and the schema offers every kind") {
    auto args = sb::SandboxTaskArgs::from_json(
        json::parse(R"({"task":"t","kind":"docker","options":{"image":"alpine","cpus":2},"isolation":true,"agent":"a"})").value());
    REQUIRE(args);
    CHECK(args->kind == sb::SandboxKind::Docker);
    CHECK(args->options.image == "alpine");
    CHECK(args->options.cpus == 2u);
    CHECK(args->isolation);
    CHECK(args->agent == "a");
    CHECK_FALSE(args->model_role);
    auto unknown = sb::SandboxTaskArgs::from_json(json::parse(R"({"task":"t","kind":"local","extra":1})").value());
    REQUIRE_FALSE(unknown);
    CHECK(unknown.error().starts_with("unknown field `extra`, expected one of `task`, `kind`"));
    auto bad_kind = sb::SandboxTaskArgs::from_json(json::parse(R"({"task":"t","kind":"qemu"})").value());
    REQUIRE_FALSE(bad_kind);
    CHECK(bad_kind.error() == "unknown variant `qemu`, expected one of `local`, `docker`, `firecracker`, `vm`, `ssh`, `peer`");
    auto bad_option = sb::SandboxTaskArgs::from_json(json::parse(R"({"task":"t","kind":"local","options":{"gpu":true}})").value());
    REQUIRE_FALSE(bad_option);

    auto schema = json::parse(sb::SandboxTaskArgs::schema()).value();
    CHECK(*schema.find("title") == json::Value("SandboxTaskArgs"));
    CHECK(schema.find("properties")->find("kind")->find("enum")->as_array()->size() == 6);
    CHECK(*schema.find("required") == json::Value(json::Array{"task", "kind"}));
    CHECK(schema.find("$defs")->find("SandboxOptions") != nullptr);

    auto result = sb::SandboxTaskResult{"w", std::nullopt, "p"};
    CHECK(json::dump(result.to_json()) == R"({"session_id":"w","final_message":null,"patch":"p"})");
    CHECK(sb::SandboxTaskResult::from_json(result.to_json()).value() == result);
}

TEST_CASE("final_message is the last assistant text that isn't thinking") {
    loa::core::MemorySink sink;
    namespace core = loa::core;
    auto session = sink.commit(core::Draft::insert(core::NodeId::DOCUMENT, core::ElementKind::Session,
                                                   {{"id", core::Value("s")}, {"harness", core::Value("x")}, {"node", core::Value("n")},
                                                    {"created", core::Value(0)}}))
                       .value();
    auto turn = sink.commit(core::Draft::insert(session.node, core::ElementKind::Turn, {{"status", core::Value("running")}})).value();
    const auto message = [&](std::string_view role, std::string_view kind, std::string text) {
        auto m = sink.commit(core::Draft::insert(turn.node, core::ElementKind::Message,
                                                 {{"role", core::Value(role)}, {"kind", core::Value(kind)}}, std::move(text)));
        REQUIRE_MESSAGE(m, core::to_string(m.error()));
    };
    CHECK_FALSE(sb::final_message(sink.dom()));
    message("assistant", "text", "first");
    message("assistant", "thinking", "hmm");
    message("user", "text", "q");
    CHECK(sb::final_message(sink.dom()) == "first");
    message("assistant", "text", "last");
    CHECK(sb::final_message(sink.dom()) == "last");
}

TEST_CASE("names that escape or collide with the root are refused") {
    CHECK_FALSE(sd::check_name(".."));
    CHECK_FALSE(sd::check_name("a/b"));
    CHECK_FALSE(sd::check_name("a\\b"));
    CHECK(sd::check_name("src"));
    CHECK(sd::member_names("./src/main.rs/").value() == std::vector<std::string>{"src", "main.rs"});
    CHECK(sd::member_names("./").value().empty());
    CHECK_FALSE(sd::member_names("../source"));
    CHECK_FALSE(sd::member_names("/etc/passwd"));
    CHECK_FALSE(sd::member_names("a//b"));
}

TEST_CASE("Windows cannot hold these names") {
    for (std::string_view name : {"a:b", "c:", "file.txt:stream", "trailing.", "trailing ", "CON", "con", "Prn.txt", "aux", "NUL.tar.gz",
                                  "COM1", "lpt9.log", "CON .txt", "COM\u00b9", "CONIN$", "a<b", "a*b", "bell\x07", "GIT~1"}) {
        CHECK_MESSAGE(sd::windows_name_problem(name).has_value(), name);
    }
    for (std::string_view name : {"main.rs", ".gitignore", "console", "COM0", "COM10", "LPT", "a b", ".git", "\u00fcn\u00ef.txt"}) {
        CHECK_MESSAGE(!sd::windows_name_problem(name).has_value(), name);
    }
}

TEST_CASE("colliding and .git names are checked on Windows only") {
    CHECK(sd::check_collisions({"a.txt", "A.TXT"}, "src").has_value() == !WINDOWS);
    CHECK(sd::check_collisions({"a.txt", "b.txt"}, "src"));
    CHECK(sd::check_guest_name(".git", true));
    CHECK(sd::check_guest_name(".GIT", true).has_value() == !WINDOWS);
    CHECK(sd::check_guest_name(".git", false).has_value() == !WINDOWS);
    CHECK(sd::check_guest_name("CON", false).has_value() == !WINDOWS);
}

// A whole archive is refused before anything is unpacked when one member can't be held here.
TEST_CASE("archives are checked member by member") {
    CHECK(sd::check_archive(tarball({{"./", AE_IFDIR}, {"./src/", AE_IFDIR}, {"./src/a.rs", AE_IFREG}})));
    CHECK(sd::check_archive(tarball({{"./a.txt", AE_IFREG}, {"./A.TXT", AE_IFREG}})).has_value() == !WINDOWS);
    CHECK(sd::check_archive(tarball({{"./CON", AE_IFREG}})).has_value() == !WINDOWS);
    CHECK_FALSE(sd::check_archive(tarball({{"./fifo", AE_IFIFO}})));
}

// Unpacking needs GNU tar: `--transform` builds the escaping member.
TEST_CASE("unpack keeps .git and refuses escapes") {
    auto root = loa::runtime::TempDir::create().value();
    const auto source = root.path() / "source", workspace = root.path() / "ws";
    fs::create_directories(source / ".git");
    write_file(source / ".git/config", "[core]\n\tfsmonitor = evil\n");
    write_file(source / "new.txt", "new");
    fs::create_directories(workspace / ".git");
    write_file(workspace / ".git/config", "host");
    write_file(workspace / "stale.txt", "stale");
    auto packed = sd::tar(source, {"-cf", "-", "./.git", "./new.txt"}, {});
    REQUIRE(packed);
    REQUIRE(sd::unpack(workspace, *packed));
    CHECK(read_file(workspace / ".git/config") == "host");
    CHECK(read_file(workspace / "new.txt") == "new");
    CHECK_FALSE(fs::exists(workspace / "stale.txt"));
    CHECK(std::distance(fs::directory_iterator(workspace), fs::directory_iterator()) == 2);

    auto escape = sd::tar(source, {"-cf", "-", "-P", "--transform=s,^,../,", "./new.txt"}, {});
    REQUIRE(escape);
    CHECK_FALSE(sd::unpack(workspace, *escape));
    CHECK_FALSE(fs::exists(root.path() / "new.txt"));
    CHECK(fs::exists(workspace / "new.txt"));
}

namespace {
class Peers final : public sb::PeerLink {
  public:
    std::vector<std::string> peers() const override { return {"key1", "key2"}; }
    std::optional<std::string> name(std::string_view peer) const override {
        if (peer == "key1") return "laptop";
        return std::nullopt;
    }
    loa::contracts::Awaitable<sb::Result<sb::Channel>> open(std::string) const override {
        co_return std::unexpected(loa::Error(loa::ErrorCode::unsupported, "no network in tests"));
    }
};
} // namespace

TEST_CASE("the registry lists factories in kind order and probes this machine") {
    const auto factories = sb::ProviderRegistry::builtin().factories();
    std::vector<sb::SandboxKind> kinds;
    for (const auto& factory : factories) kinds.push_back(factory->kind());
#ifdef LOA_SANDBOX_VM
    CHECK(kinds == std::vector<sb::SandboxKind>{sb::SandboxKind::Local, sb::SandboxKind::Docker, sb::SandboxKind::Firecracker,
                                                 sb::SandboxKind::Vm, sb::SandboxKind::Ssh, sb::SandboxKind::Peer});
#else
    CHECK(kinds.size() == 4);
#endif

    sb::Machine machine{std::nullopt, nullptr};
    sb::SandboxConfig config;
    const auto local = block_on(sb::LocalFactory().probe(machine, config));
    CHECK(local == sb::ProviderStatus{sb::SandboxKind::Local, sb::Availability::Unavailable, std::string(sb::NO_STUB)});
    CHECK_FALSE(sb::LocalFactory().build(machine, config));
    CHECK(block_on(sb::PeerFactory().probe(machine, config)).detail == "Peer networking is not running");

    machine.stub = LOA_SANDBOX_STUB_PATH;
    machine.peers = std::make_shared<Peers>();
    CHECK(block_on(sb::LocalFactory().probe(machine, config)).availability == sb::Availability::Ready);
    const auto peers = block_on(sb::PeerFactory().probe(machine, config));
    CHECK(peers == sb::ProviderStatus{sb::SandboxKind::Peer, sb::Availability::Ready, "laptop, key2"});
    auto provider = sb::PeerFactory().build(machine, config);
    REQUIRE(provider);
    CHECK(provider->describe() == "a peer node; set options.peer to one of key1 (laptop), key2");
    sb::SandboxSpec spec{"n", "/ws", {}, {}};
    spec.options.peer = "stranger";
    auto refused = block_on(provider->start(spec));
    REQUIRE_FALSE(refused);
    CHECK(refused.error().message() == "peer: no sandbox grant from peer stranger");

    sb::SandboxConfig ssh_config;
    ssh_config.ssh_hosts = {"build-box"};
    CHECK(sb::SshFactory::hosts(ssh_config) == std::vector<std::string>{"build-box"});
    CHECK(sb::SshProvider("/usr/bin/ssh", {"one"}, "/stub").describe() == "the SSH host one (options.host may name another)");

    sb::SandboxService service(config, nullptr);
    service.with_stub(std::nullopt);
    const auto statuses = block_on(service.probe());
    CHECK(statuses.size() == factories.size());
    CHECK(statuses.front().kind == sb::SandboxKind::Local);
}

TEST_CASE("docker and qemu command lines match the Rust providers") {
    sb::SandboxSpec spec{"loa-sbx-1", "/ws", {{"K", "V"}}, {}};
    spec.options.memory_mib = 256;
    spec.options.cpus = 2;
    const auto docker = sb::DockerProvider("/usr/bin/docker", "/stub", "debian:stable-slim", false).run_command(spec);
    CHECK(docker.clear_env);
    CHECK(docker.args == std::vector<std::string>{"run", "--rm", "-i", "--name", "loa-sbx-1", "--network", "none", "--mount",
                                                  "type=bind,src=/ws,dst=/workspace", "--workdir", "/workspace", "--mount",
                                                  "type=bind,src=/stub,dst=/opt/loa/loa-sandbox-stub,readonly", "--memory", "256m",
                                                  "--cpus", "2", "--env", "K=V", "debian:stable-slim", "/opt/loa/loa-sandbox-stub",
                                                  "/workspace"});
#ifdef LOA_SANDBOX_VM
    spec.options.network = true;
    const auto qemu = sb::QemuProvider("/usr/bin/qemu", "/disk.img", fs::path("/vmlinuz"), true).run_command(spec);
    CHECK(qemu.args == std::vector<std::string>{"-display", "none", "-monitor", "none", "-serial", "stdio", "-snapshot", "-m", "256",
                                                "-smp", "2", "-drive", "file=/disk.img,format=raw,if=virtio", "-nic",
                                                "user,model=virtio-net-pci", "-accel", "kvm", "-kernel", "/vmlinuz", "-append",
                                                "console=ttyS0 root=/dev/vda rw"});
    sb::Machine machine{std::nullopt, nullptr};
    sb::SandboxConfig config;
    const auto fc = block_on(sb::FirecrackerFactory().probe(machine, config));
    CHECK(fc.kind == sb::SandboxKind::Firecracker);
    CHECK(fc.availability != sb::Availability::Ready);
    CHECK_FALSE(sb::QemuFactory().build(machine, config));
#endif
}
