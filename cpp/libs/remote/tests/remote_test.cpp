// End to end (port of crates/loa-remote/tests/remote.rs): the service against loopback OpenSSH
// servers, an ssh-agent and real files.

#include <doctest/doctest.h>

#include <signal.h>
#include <sys/stat.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <thread>

#include "loa/remote/config.hpp"
#include "loa/remote/resolve.hpp"
#include "loa/remote/service.hpp"
#include "loa/runtime/exec.hpp"
#include "sshd.hpp"

using namespace loa::remote;
using namespace loa::remote::test;
namespace fs = std::filesystem;

#define REQUIRE_SSHD()                                                \
    if (!have_sshd()) {                                               \
        MESSAGE("no sshd found (set LOA_SSHD); skipped");             \
        return;                                                       \
    }

namespace {

BashRequest bash(std::string command) { return BashRequest{"box", std::move(command), std::nullopt, std::nullopt}; }
FileRequest file(const fs::path& path, FileOp op) { return FileRequest{"box", path.string(), std::move(op)}; }
FileOp write_op(std::string content) { return WriteOp{std::move(content)}; }
FileOp edit_op(std::string o, std::string n) { return EditOp{std::move(o), std::move(n)}; }

// A directory for remote files: the rig's, canonical (the server reports real paths).
fs::path root(const Rig& rig) {
    const auto r = fs::canonical(rig.dir.path()) / "fs";
    fs::create_directories(r);
    return r;
}

void put(const fs::path& path, const std::string& bytes, fs::perms mode = fs::perms::owner_read | fs::perms::owner_write) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << bytes;
    fs::permissions(path, mode);
}

template <typename F>
bool eventually(F ready) {
    for (int i = 0; i < 300; ++i) {
        if (ready()) return true;
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    return false;
}

} // namespace

TEST_CASE("bash captures, caps, quotes, reuses and audits") {
    REQUIRE_SSHD();
    auto limits = open_limits();
    limits.max_output_bytes = 1000;
    const auto rig = setup(limits).build();
    auto out = rig->service->bash("s1", bash("echo hello"), CancelToken());
    REQUIRE(out);
    CHECK(out->stdout_text == "hello\n");
    CHECK(out->exit_status == 0u);
    out = rig->service->bash("s1", bash("echo boom >&2; exit 3"), CancelToken());
    REQUIRE(out);
    CHECK(out->stderr_text == "boom\n");
    CHECK(out->exit_status == 3u);
    const auto row = rig->last();
    CHECK(row.action == Action::command);
    CHECK(row.outcome == Outcome::ok);
    CHECK(row.target == "echo boom >&2; exit 3");
    CHECK(row.session_id == "s1");
    CHECK(row.detail == "exit 3; stdout 0 bytes, stderr 5 bytes");
    out = rig->service->bash("s1", bash("head -c 5000 /dev/zero | tr '\\0' x"), CancelToken());
    REQUIRE(out);
    CHECK(out->stdout_text.size() == 1000);
    CHECK(out->stdout_dropped == 4000);
    const auto quoted_dir = root(*rig) / "it's here";
    fs::create_directories(quoted_dir);
    auto in_dir = bash("pwd");
    in_dir.cwd = quoted_dir.string();
    out = rig->service->bash("s1", in_dir, CancelToken());
    REQUIRE(out);
    CHECK(out->stdout_text == quoted_dir.string() + "\n");
    CHECK(rig->last().target == "cd " + loa::runtime::exec::shell_quote(quoted_dir.string()) + " && pwd");
    CHECK(rig->last().target.find("it'\\''s here") != std::string::npos);
    CHECK(rig->server->commands() == 4);
    CHECK_MESSAGE(rig->server->connections() == 1, "commands reuse the connection");
}

TEST_CASE("cancel and timeout kill a hanging command") {
    REQUIRE_SSHD();
    const auto rig = setup(open_limits()).build();
    const auto pid_file = rig->dir.path() / "hang.pid";
    CancelToken cancel;
    Result<ExecOutput> cancelled = std::unexpected(Error::invalid_request("not run"));
    std::thread task([&] { cancelled = rig->service->bash("s", bash("echo $$ > '" + pid_file.string() + "'; exec sleep 1000"), cancel); });
    REQUIRE(eventually([&] { return fs::exists(pid_file) && fs::file_size(pid_file) > 0; }));
    cancel.cancel();
    task.join();
    REQUIRE(cancelled);
    CHECK(cancelled->end == ExecEnd::cancelled);
    CHECK(rig->last().outcome == Outcome::cancelled);
    const auto pid = std::stoi(slurp(pid_file));
    CHECK_MESSAGE(eventually([&] { return ::kill(pid, 0) != 0; }), "the cancelled command was killed");

    auto timed = bash("sleep 1000");
    timed.timeout_secs = 1;
    const auto out = rig->service->bash("s", timed, CancelToken());
    REQUIRE(out);
    CHECK(out->end == ExecEnd::timed_out);
    CHECK(rig->last().outcome == Outcome::timed_out);
    CHECK(eventually([&] { return rig->server->signalled("KILL"); }));
}

TEST_CASE("proxyjump chain resolves hop by hop and tunnels through each") {
    REQUIRE_SSHD();
    auto s = setup(open_limits());
    s.accept_keys = true;
    const auto rig = s.build();
    TestServer inner(rig->agent->public_line);
    TestServer target(rig->agent->public_line);
    Static ssh(user());
    ssh.with("outer", "127.0.0.1", rig->server->port).with("inner", "127.0.0.1", inner.port, "outer").with("target", "127.0.0.1", target.port, "inner");
    const std::map<std::string, RemoteHost> entries{{"target", entry_for_alias("target")}};
    auto [hosts, failed] = load(entries, ssh);
    REQUIRE(failed.empty());
    auto host = hosts.at(0);
    host.limits = open_limits();
    rig->hosts->insert(host);
    auto request = bash("echo through");
    request.host = "target";
    const auto out = rig->service->bash("s", request, CancelToken());
    REQUIRE_MESSAGE(out, (out ? "" : out.error().message()));
    CHECK(out->stdout_text == "through\n");
    CHECK(rig->server->forwarded_to(inner.port));
    CHECK(rig->server->commands() == 0);
    CHECK(inner.forwarded_to(target.port));
    CHECK(target.commands() == 1);
    CHECK(inner.commands() == 0);
    std::vector<std::uint16_t> prompted;
    for (const auto& p : rig->decider->prompts()) prompted.push_back(p.port);
    const std::vector<std::uint16_t> unknown_hops{inner.port, target.port};
    CHECK_MESSAGE(prompted == unknown_hops, "only the unknown hops are asked about");
}

TEST_CASE("new host keys are asked once and changed ones refused") {
    REQUIRE_SSHD();
    {
        auto s = setup(open_limits());
        s.known = "";
        s.accept_keys = true;
        const auto rig = s.build();
        REQUIRE(rig->service->bash("s", bash("echo 1"), CancelToken()));
        CHECK(slurp(rig->known_hosts) == rig->server->known_hosts_line());
        rig->service->disconnect("box");
        REQUIRE(rig->service->bash("s", bash("echo 2"), CancelToken()));
        const auto prompts = rig->decider->prompts();
        CHECK(rig->server->connections() == 2);
        REQUIRE(prompts.size() == 1);
        CHECK(prompts[0].change.kind == HostKeyChange::Kind::new_key);
        CHECK(prompts[0].fingerprint == fingerprint(rig->server->host_key));
        CHECK(prompts[0].key_type == "ssh-ed25519");
    }
    const auto rig = setup(open_limits()).build();
    keygen(rig->dir.path() / "old");
    const auto old = public_key(rig->dir.path() / "old.pub");
    const auto known = "[127.0.0.1]:" + std::to_string(rig->server->port) + " " + old.to_openssh() + "\n";
    std::ofstream(rig->known_hosts, std::ios::binary | std::ios::trunc) << known;
    const auto err = rig->service->bash("s", bash("echo pwned"), CancelToken());
    REQUIRE_FALSE(err);
    CHECK(err.error().kind() == ErrorKind::host_key_rejected);
    CHECK(err.error().reason().find("CHANGED") != std::string::npos);
    const auto prompts = rig->decider->prompts();
    REQUIRE(prompts.size() == 1);
    REQUIRE(prompts[0].change.kind == HostKeyChange::Kind::changed);
    CHECK(prompts[0].change.previous.at(0).fingerprint == fingerprint(old));
    CHECK(rig->server->commands() == 0);
    CHECK(slurp(rig->known_hosts) == known);
    CHECK(rig->last().outcome == Outcome::error);
}

TEST_CASE("only agent identities authenticate") {
    REQUIRE_SSHD();
    {
        auto s = setup(open_limits());
        s.authorized = false;
        const auto rig = s.build();
        const auto err = rig->service->bash("s", bash("echo"), CancelToken());
        REQUIRE_FALSE(err);
        CHECK(err.error().kind() == ErrorKind::auth_failed);
        CHECK(err.error().tried() == 1);
    }
    auto s = setup(open_limits());
    s.agent = false;
    const auto rig = s.build();
    const auto err = rig->service->bash("s", bash("echo"), CancelToken());
    REQUIRE_FALSE(err);
    CHECK(err.error().kind() == ErrorKind::no_agent);
    // IdentityAgent wins over the (missing) default socket.
    auto host = *rig->hosts->get("box");
    host.target.agent = AgentSocket::at(rig->agent->socket);
    rig->hosts->insert(host);
    const auto ok = rig->service->bash("s", bash("echo ok"), CancelToken());
    REQUIRE(ok);
    CHECK(ok->stdout_text == "ok\n");
}

TEST_CASE("files write atomically, then edit, read and list") {
    REQUIRE_SSHD();
    const auto rig = setup(open_limits()).build();
    const auto r = root(*rig);
    const auto conf = r / "srv/app.conf";
    put(conf, "port = 1\nname = a\n", fs::perms::owner_read | fs::perms::owner_write | fs::perms::group_read);
    auto out = rig->service->file("s", file(conf, write_op("port = 2\nname = a\n")), CancelToken());
    REQUIRE(out);
    const auto* written = std::get_if<WriteOutput>(&*out);
    REQUIRE(written);
    CHECK(written->mode == WriteMode::atomic);
    CHECK(written->bytes == 18);
    CHECK(slurp(conf) == "port = 2\nname = a\n");
    CHECK((fs::status(conf).permissions() & fs::perms::mask) == (fs::perms::owner_read | fs::perms::owner_write | fs::perms::group_read));
    CHECK(std::distance(fs::directory_iterator(r / "srv"), fs::directory_iterator()) == 1);
    REQUIRE(rig->service->file("s", file(conf, edit_op("port = 2", "port = 3")), CancelToken()));
    for (const auto& [old_text, error] : {std::pair{" = ", "occurs 2 times"}, std::pair{"missing", "not found"}}) {
        const auto err = rig->service->file("s", file(conf, edit_op(old_text, "x")), CancelToken());
        REQUIRE_FALSE(err);
        CHECK(err.error().message().find(error) != std::string::npos);
    }
    put(r / "srv/blob", std::string("\xff\x00\xfe", 3));
    fs::create_directories(r / "srv/sub");
    for (const auto& [path, content, encoding] :
         {std::tuple{conf, "port = 3\nname = a\n", "utf8"}, std::tuple{r / "srv/blob", "/wD+", "base64"}}) {
        const auto read = rig->service->file("s", file(path, ReadOp{}), CancelToken());
        REQUIRE(read);
        const auto* got = std::get_if<ReadOutput>(&*read);
        REQUIRE(got);
        CHECK(got->content == content);
        CHECK(got->encoding == encoding);
    }
    const auto listed = rig->service->file("s", file(r / "srv", ListOp{}), CancelToken());
    REQUIRE(listed);
    const auto& entries = std::get<ListOutput>(*listed).entries;
    std::vector<std::pair<std::string, EntryKind>> got;
    for (const auto& e : entries) got.emplace_back(e.name, e.kind);
    CHECK(got == std::vector<std::pair<std::string, EntryKind>>{{"app.conf", EntryKind::file}, {"blob", EntryKind::file}, {"sub", EntryKind::dir}});
    CHECK(rig->server->connections() == 1);

    // A new file is written to a temp file and renamed into place too.
    const auto fresh = rig->service->file("s", file(r / "srv/new", write_op("a")), CancelToken());
    REQUIRE(fresh);
    CHECK(std::get<WriteOutput>(*fresh).mode == WriteMode::atomic);
    CHECK(slurp(r / "srv/new") == "a");
}

TEST_CASE("limits refuse before connecting and every refusal is audited") {
    REQUIRE_SSHD();
    auto read_only = open_limits();
    read_only.read_only = true;
    auto no_commands = open_limits();
    no_commands.commands = false;
    auto small = open_limits();
    small.max_file_bytes = 4;
    auto srv = open_limits();
    srv.allowed_paths = {"/srv"};
    const auto all = open_limits();
    const HostLimits none;
    const auto unknown = [](FileRequest r) {
        r.host = "nope";
        return r;
    };
    auto nope = bash("echo");
    nope.host = "nope";
    auto etc = bash("pwd");
    etc.cwd = "/etc";
    const std::vector<std::tuple<HostLimits, std::variant<BashRequest, FileRequest>, Action>> cases{
        {read_only, file("/srv/x", write_op("x")), Action::write},
        {read_only, bash("echo"), Action::command},
        {no_commands, bash("echo"), Action::command},
        {all, nope, Action::command},
        {all, unknown(file("/srv/x", ReadOp{})), Action::read},
        {small, file("/srv/new", write_op("12345")), Action::write},
        {srv, file("/srv/../etc/shadow", ReadOp{}), Action::read},
        {srv, etc, Action::command},
        {none, file("/srv/x", ListOp{}), Action::list},
    };
    for (std::size_t i = 0; i < cases.size(); ++i) {
        const auto& [limits, request, action] = cases[i];
        CAPTURE(i);
        const auto rig = setup(limits).build();
        std::optional<Error> err;
        if (const auto* b = std::get_if<BashRequest>(&request)) {
            auto r = rig->service->bash("s9", *b, CancelToken());
            if (!r) err = r.error();
        } else {
            auto r = rig->service->file("s9", std::get<FileRequest>(request), CancelToken());
            if (!r) err = r.error();
        }
        REQUIRE(err);
        const auto kind = err->kind();
        CHECK((kind == ErrorKind::refused || kind == ErrorKind::unknown_host || kind == ErrorKind::too_large));
        const auto row = rig->last();
        CHECK(row.session_id == "s9");
        CHECK(row.action == action);
        CHECK_MESSAGE(rig->server->connections() == 0, "nothing may be sent");
    }
}

TEST_CASE("symlinks cannot escape the allowed paths") {
    REQUIRE_SSHD();
    auto s = open_limits();
    const auto rig0 = setup(s).build();
    const auto r = root(*rig0);
    s.allowed_paths = {(r / "srv").string(), (r / "www").string()};
    auto host = *rig0->hosts->get("box");
    host.limits = s;
    rig0->hosts->insert(host);
    const auto& rig = rig0;
    for (const auto* dir : {"etc", "data", "data/www", "srv"}) fs::create_directories(r / dir);
    put(r / "etc/shadow", "secret");
    put(r / "data/ok.txt", "fine");
    put(r / "data/www/real.html", "v1");
    fs::create_directory_symlink(r / "etc", r / "srv/etc");
    fs::create_symlink("../etc/shadow", r / "srv/shadow");
    fs::create_directory_symlink(r / "data", r / "srv/data");
    fs::create_directory_symlink(r / "data/www", r / "www");
    fs::create_symlink("real.html", r / "data/www/index.html");
    const std::vector<std::pair<fs::path, FileOp>> attempts{{r / "srv/shadow", ReadOp{}},
                                                            {r / "srv/etc/passwd", write_op("root::0:0")},
                                                            {r / "srv/etc", ListOp{}},
                                                            {r / "srv/data/ok.txt", ReadOp{}}};
    for (const auto& [path, op] : attempts) {
        CAPTURE(path);
        const auto result = rig->service->file("s", file(path, op), CancelToken());
        REQUIRE_FALSE(result);
        CHECK(result.error().kind() == ErrorKind::refused);
    }
    CHECK(rig->last().target == (r / "srv/data/ok.txt").string() + " -> " + (r / "data/ok.txt").string());
    auto in_link = bash("pwd");
    in_link.cwd = (r / "srv/etc").string();
    const auto refused = rig->service->bash("s", in_link, CancelToken());
    REQUIRE_FALSE(refused);
    CHECK(refused.error().kind() == ErrorKind::refused);
    CHECK(rig->server->commands() == 0);
    CHECK_FALSE(fs::exists(r / "etc/passwd"));
    // An allowed prefix that is itself a symlink, and a link inside it, are followed.
    const auto out = rig->service->file("s", file(r / "www/index.html", write_op("v2")), CancelToken());
    REQUIRE(out);
    CHECK(std::get<WriteOutput>(*out).path == (r / "data/www/real.html").string());
    CHECK(slurp(r / "data/www/real.html") == "v2");
    CHECK(fs::is_symlink(r / "data/www/index.html"));
    CHECK(fs::read_symlink(r / "data/www/index.html") == "real.html");
}

TEST_CASE("approvals gate writes and commands per session and class") {
    REQUIRE_SSHD();
    HostLimits limits;
    limits.allowed_paths = {"/"};
    auto s = setup(limits);
    s.approvals = {Approval::deny("not now"), Approval::allow_session(), Approval::allow_once()};
    const auto rig = s.build();
    const auto f = root(*rig) / "srv/f";
    put(f, "orig");
    const std::vector<std::tuple<std::string, std::variant<BashRequest, FileOp>, bool, std::size_t>> steps{
        {"s1", write_op("A"), false, 1},       {"s1", write_op("B"), true, 2}, {"s1", edit_op("B", "C"), true, 2},
        {"s1", bash("echo x"), true, 3},       {"s2", write_op("D"), false, 4}, {"s3", FileOp{ReadOp{}}, true, 4},
    };
    for (std::size_t i = 0; i < steps.size(); ++i) {
        const auto& [session, request, ok, asked] = steps[i];
        CAPTURE(i);
        bool done = false;
        if (const auto* b = std::get_if<BashRequest>(&request)) done = rig->service->bash(session, *b, CancelToken()).has_value();
        else done = rig->service->file(session, file(f, std::get<FileOp>(request)), CancelToken()).has_value();
        CHECK(done == ok);
        CHECK(rig->approver->requests().size() == asked);
    }
    CHECK(slurp(f) == "C");
    AuditQuery q;
    q.outcome = Outcome::denied;
    const auto denied = rig->audit->query(q);
    REQUIRE(denied);
    std::vector<std::pair<std::string, std::string>> got;
    for (const auto& row : *denied) got.emplace_back(row.session_id, row.detail);
    CHECK(got == std::vector<std::pair<std::string, std::string>>{{"s2", "denied by the user"}, {"s1", "denied by the user: not now"}});
    const auto request = rig->approver->requests().at(2);
    CHECK(request.action == Action::command);
    CHECK(request.target == "echo x");
}

TEST_CASE("shells open a pty, take input, resize and report their exit") {
    REQUIRE_SSHD();
    const auto rig = setup(HostLimits{}).build();
    auto shell = rig->service->open_shell("box", 120, 40);
    REQUIRE_MESSAGE(shell, (shell ? "" : shell.error().message()));
    auto& reader = shell->reader;
    auto& writer = shell->writer;
    std::string seen;
    const auto until = [&](const std::string& needle) -> std::optional<std::optional<std::uint32_t>> {
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
        while (seen.find(needle) == std::string::npos && std::chrono::steady_clock::now() < deadline) {
            const auto r = reader.read_for(std::chrono::milliseconds(200));
            if (!r) continue;
            if (r->kind == ShellRead::Kind::closed) return r->exit_status;
            seen += r->data;
        }
        return std::nullopt;
    };
    CHECK(writer.write("echo \"term=$TERM size=$(stty size)\"\r"));
    until("size=40 120");
    CHECK(seen.find("term=xterm-256color size=40 120") != std::string::npos);
    CHECK(writer.resize(80, 24));
    CHECK(writer.write("echo \"now=$(stty size)\"\r"));
    until("now=24 80");
    CHECK(seen.find("now=24 80") != std::string::npos);
    CHECK(writer.write("exit 7\r"));
    const auto exited = until("never printed");
    REQUIRE(exited);
    CHECK(*exited == 7u);
    CHECK_MESSAGE(reader.read() == ShellRead::closed_with(7), "stays closed");
    const auto row = rig->last();
    CHECK(row.action == Action::terminal);
    CHECK(row.outcome == Outcome::ok);
    CHECK(row.session_id == TERMINAL_SESSION);
    CHECK(row.detail == "120x40");
    const auto nowhere = rig->service->open_shell("nowhere", 80, 24);
    REQUIRE_FALSE(nowhere);
    CHECK(nowhere.error().kind() == ErrorKind::unknown_host);
}
