// Ports of the loa-util tests (exec, fs, ssh, text, time, async_util, registry) plus the
// util golden fixtures and the TempFile/TempDir lifetimes.

#include <doctest/doctest.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include <filesystem>
#include <fstream>
#include <functional>
#include <iterator>
#include <sstream>

#include "loa/runtime/async_util.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/registry.hpp"
#include "loa/runtime/ssh.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/runtime/text.hpp"
#include "loa/runtime/time.hpp"

namespace stdfs = std::filesystem;
using namespace loa::runtime;
using namespace std::chrono_literals;

namespace {

std::string slurp(const stdfs::path& p) {
    std::ifstream in(p, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), {}};
}

void spit(const stdfs::path& p, std::string_view s) { std::ofstream(p, std::ios::binary) << s; }

stdfs::path make_exe(const stdfs::path& dir, const std::string& name, unsigned mode = 0755) {
    auto p = dir / name;
    spit(p, "x");
    stdfs::permissions(p, static_cast<stdfs::perms>(mode));
    return p;
}

} // namespace

TEST_CASE("exec: which_in searches PATH entries in order, only executables, never for paths") {
    auto a = TempDir::create().value(), b = TempDir::create().value();
    make_exe(a.path(), "tool", 0644);
    const auto want = make_exe(b.path(), "tool");
    make_exe(b.path(), "dup");
    const auto first = make_exe(a.path(), "dup");
    const auto var = a.path().string() + ":" + b.path().string();
    CHECK(exec::which_in("tool", var) == want);
    CHECK(exec::which_in("dup", var) == first);
    CHECK_FALSE(exec::which_in("tool", std::nullopt));
    CHECK_FALSE(exec::which_in("tool.exe", var, ".EXE"));
    stdfs::create_directory(a.path() / "sub");
    make_exe(a.path() / "sub", "t");
    CHECK_FALSE(exec::which_in("sub/t", a.path().string()));
    CHECK(exec::resolve("loa-surely-missing-program") == "loa-surely-missing-program");
}

TEST_CASE("exec: versions, quoting and running programs") {
    const std::pair<const char*, std::optional<std::string>> versions[] = {
        {"omp/19.0.0\n", "19.0.0"}, {"codex-cli 0.46.0", "0.46.0"}, {"2.0.14 (Claude Code)", "2.0.14"},
        {"opencode 1.2.3-beta.1+build", "1.2.3-beta.1+build"}, {"v1.2.3, built", "1.2.3"}, {"gemini 0.9.", "0.9"}, {"x86 build 7", std::nullopt}};
    for (const auto& [text, want] : versions) CHECK_MESSAGE(exec::parse_version(text) == want, text);
    CHECK(exec::shell_quote("it's") == R"('it'\''s')");

    auto dir = TempDir::create().value();
    const auto ok = dir.path() / "ok tool", bad = dir.path() / "bad";
    REQUIRE(exec::write_executable(ok, "#!/bin/sh\necho \"v$1\"\n"));
    REQUIRE(exec::write_executable(bad, "#!/bin/sh\necho 'bad flag' >&2\nexit 3\n"));
    CHECK(exec::which(ok) == ok);
    CHECK(exec::which("sh"));
    CHECK(exec::probe_cli(ok, {"1.2"}, 5s).value() == "v1.2\n");
    const auto err = exec::probe_cli(bad, {}, 5s).error().to_string();
    CHECK(err == "`" + bad.string() + " ` failed (exit status: 3): bad flag");
    CHECK(exec::run_checked(exec::Command{bad}).error() == "exit status: 3: bad flag");
    CHECK(exec::probe_cli(dir.path() / "missing", {}, 5s).error().code() == loa::ErrorCode::not_found);

    // A child that outlives the timeout is reported and killed.
    REQUIRE(exec::write_executable(dir.path() / "slow", "#!/bin/sh\nexec sleep 5\n"));
    CHECK(exec::probe_cli(dir.path() / "slow", {"x"}, 100ms).error().to_string() == "`" + (dir.path() / "slow").string() + "` didn't answer x");

    int fds[2];
    REQUIRE(::pipe(fds) == 0);
    const auto tail = exec::StderrTail::spawn(fds[0], 2);
    const std::string_view bytes = "a\nb\xff\nc";
    REQUIRE(::write(fds[1], bytes.data(), bytes.size()) == static_cast<ssize_t>(bytes.size()));
    ::close(fds[1]);
    REQUIRE(tail.wait_closed(5s));
    CHECK(tail.joined() == "b\xEF\xBF\xBD\nc");
}

TEST_CASE("exec: retry_busy passes other errors through") {
    int attempts = 0;
    auto r = exec::retry_busy([&]() -> std::expected<int, std::error_code> {
        ++attempts;
        return std::unexpected(std::make_error_code(std::errc::no_such_file_or_directory));
    });
    CHECK(attempts == 1);
    CHECK(r.error() == std::errc::no_such_file_or_directory);
}

TEST_CASE("fs: writes, snapshots and confinement") {
    auto dir = TempDir::create().value();
    const auto real = dir.path() / "real", link = dir.path() / "link";
    REQUIRE(fs::write_atomic(real, "a", 0600));
    struct stat st {};
    REQUIRE(::stat(real.c_str(), &st) == 0);
    CHECK((st.st_mode & 0777) == 0600);
    stdfs::create_symlink(real, link);
    auto snap = fs::Snapshot::load(link).value();
    CHECK(snap.save("b").value());
    CHECK(stdfs::is_symlink(link));
    CHECK(slurp(real) == "b");
    spit(real, "other");
    CHECK_FALSE(snap.save("c").value());
    CHECK(fs::read_optional(real).value() == "other");
    CHECK(fs::read_optional(dir.path() / "none").value() == std::nullopt);

    stdfs::create_symlink("/", dir.path() / "out");
    const auto root = stdfs::canonical(dir.path());
    const std::pair<const char*, const char*> cases[] = {{"a/b", "a/b"}, {"./real", "real"}, {"../x", nullptr},
                                                         {"/etc", nullptr}, {"out/etc", nullptr}, {"link", "real"}};
    for (const auto& [rel, ok] : cases) {
        const auto got = fs::confine(dir.path(), rel);
        if (ok) CHECK_MESSAGE(got.value() == root / ok, rel);
        else CHECK_MESSAGE(got.error().code() == loa::ErrorCode::permission_denied, rel);
    }

    auto v = json::parse(R"({"a":[1,2],"b":{}})").value();
    REQUIRE(fs::write_json_atomic(dir.path() / "j.json", v));
    CHECK(slurp(dir.path() / "j.json") == "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}");
}

TEST_CASE("fs: home and data_dir follow the environment") {
    const std::string saved_home = std::getenv("HOME") ? std::getenv("HOME") : "";
    const char* saved_xdg = std::getenv("XDG_DATA_HOME");
    const std::string saved_xdg_s = saved_xdg ? saved_xdg : "";
    ::setenv("HOME", "/somewhere/else", 1);
    ::setenv("XDG_DATA_HOME", "relative", 1);
    CHECK(fs::home() == stdfs::path("/somewhere/else"));
    CHECK(fs::data_dir("loa") == stdfs::path("/somewhere/else/.local/share/loa"));
    ::setenv("XDG_DATA_HOME", "/xdg", 1);
    CHECK(fs::data_dir("loa") == stdfs::path("/xdg/loa"));
    ::setenv("HOME", "", 1);
    CHECK(fs::home().has_value()); // the passwd entry
    ::setenv("HOME", saved_home.c_str(), 1);
    if (saved_xdg) ::setenv("XDG_DATA_HOME", saved_xdg_s.c_str(), 1);
    else ::unsetenv("XDG_DATA_HOME");
}

TEST_CASE("TempFile and TempDir are removed on destruction") {
    stdfs::path file, dir;
    {
        auto f = TempFile::create().value();
        auto d = TempDir::create().value();
        file = f.path();
        dir = d.path();
        spit(dir / "inner", "x");
        CHECK(stdfs::exists(file));
        CHECK(stdfs::exists(dir / "inner"));
    }
    CHECK_FALSE(stdfs::exists(file));
    CHECK_FALSE(stdfs::exists(dir));
}

TEST_CASE("ssh: aliases follow includes and ssh -G output parses") {
    auto dir = TempDir::create().value();
    stdfs::create_directory(dir.path() / "conf.d");
    spit(dir.path() / "conf.d/a.conf", "Host inc-b inc-a\n");
    spit(dir.path() / "config", "Host Db web*  !bad\n  HostName x\nInclude conf.d/*.conf\nMatch host y\nHost=db last\n");
    CHECK(ssh::aliases(dir.path() / "config") == std::vector<std::string>{"db", "inc-b", "inc-a", "last"});
    CHECK(ssh::aliases(dir.path() / "missing").empty());
    CHECK(ssh::wildcard_match("*.lan", "db.lan"));
    CHECK(ssh::wildcard_match("d?", "db"));
    CHECK_FALSE(ssh::wildcard_match("*.lan", "db.lan.x"));
    CHECK(ssh::wildcard_match("a*b*c", "aXbYc"));
    const auto got = ssh::parse("user me\nhostname 10.0.0.1\nport 2222\nproxyjump j1,j2\nidentityagent none\nforwardagent no\n");
    CHECK(got == ssh::ResolvedHost{"10.0.0.1", "me", 2222, "j1,j2", std::nullopt});
    const auto missing = ssh::resolve_with(dir.path() / "no-ssh", {}, "h");
    CHECK(missing.error().message().find("no OpenSSH client") != std::string::npos);
}

TEST_CASE("text: cuts, escapes and looks up") {
    const std::string_view s = "a\xC3\xA9\xE2\x82\xAC" "b"; // a é € b: a=0, é=1..3, €=3..6, b=6
    const std::size_t cases[][3] = {{0, 0, 0}, {2, 1, 3}, {4, 3, 6}, {9, 7, 7}};
    for (const auto& c : cases) {
        CHECK(text::floor_boundary(s, c[0]) == c[1]);
        CHECK(text::ceil_boundary(s, c[0]) == c[2]);
    }
    CHECK(text::clip(s, 5) == "a\xC3\xA9");
    CHECK(text::clip(s, 99) == s);
    CHECK(text::first_line("\n  hello world \nx", 5) == "hello…");
    CHECK(text::first_line("hi", 5) == "hi");
    CHECK(text::clip_marked("abcdef", 2) == "ab… [4 more bytes]");
    CHECK(text::clip_marked("ab", 2) == "ab");
    CHECK(text::percent_encode("a b/\xC3\xA9~") == "a%20b%2F%C3%A9~");
    CHECK(text::random_hex(8).size() == 16);
    CHECK(text::random_hex(8) != text::random_hex(8));
    CHECK(text::mime_of("x.PNG") == "image/png");
    CHECK(text::mime_of("doc.pdf") == "application/pdf");
    CHECK_FALSE(text::mime_of("a.rs"));
    CHECK_FALSE(text::mime_of("noext"));
}

TEST_CASE("text: byte pages cover the text exactly once") {
    const text::Paginator pages{8, 16};
    CHECK(pages.size(0) == 8);
    CHECK(pages.size(5) == 5);
    CHECK(pages.size(99) == 16);
    std::string repeated;
    for (int i = 0; i < 40; ++i) repeated += "\xC3\xBC\xE2\x82\xACx\n";
    for (const std::string_view t : {std::string_view(), std::string_view("short"), std::string_view("line one\nline two\nline three\n"),
                                     std::string_view(repeated)}) {
        std::string joined;
        std::size_t offset = 0;
        for (;;) {
            const auto page = pages.bytes(t, offset, 7);
            CHECK(page.range.start == offset);
            CHECK(page.range.size() <= 7);
            CHECK((!page.range.empty() || t.empty()));
            joined += t.substr(page.range.start, page.range.size());
            if (!page.next) break;
            offset = *page.next;
        }
        CHECK(joined == t);
    }
}

TEST_CASE("time: ages, spans and timestamps") {
    constexpr std::int64_t min = 60'000, day = 86'400'000;
    const std::pair<std::int64_t, const char*> ages[] = {
        {-20'000, "just now"}, {-5 * min, "5m ago"}, {-3 * 60 * min, "3h ago"}, {-2 * day, "2d ago"}, {min, "just now"}};
    for (const auto& [then, want] : ages) CHECK(time::ago(10 * day, 10 * day + then) == want);
    CHECK(time::until(35 * min) == "35m");
    CHECK(time::until(275 * min) == "4h 35m");
    CHECK(time::until(2 * day + 12 * 60 * min) == "2d 12h");
    CHECK(time::parse_iso_ms("2026-01-02T03:04:05.678Z") == 1'767'323'045'678);
    CHECK(time::parse_iso_ms("2026-01-02T05:04:05.678+02:00") == 1'767'323'045'678);
    CHECK(time::parse_iso_ms("1970-01-01T00:00:00Z") == 0);
    CHECK_FALSE(time::parse_iso_ms("2026-13-02T03:04:05Z"));
    CHECK_FALSE(time::parse_iso_ms("2023-02-29T00:00:00Z"));
    CHECK(time::format_iso_ms(1'767'323'045'678) == "2026-01-02T03:04:05.678Z");
    const std::string leap = "2024-02-29T23:59:59.123Z";
    CHECK(time::format_iso_ms(time::parse_iso_ms(leap).value()) == leap);
    CHECK(time::now_ms() > 1'700'000'000'000);
    CHECK(time::now_us() / 1000 >= static_cast<std::uint64_t>(time::now_ms()) - 1000);
}

TEST_CASE("golden: format_iso_ms and ssh aliases match the Rust output") {
    const stdfs::path golden = LOA_GOLDEN_DIR "/util";
    std::istringstream iso(slurp(golden / "iso.txt"));
    std::string rebuilt;
    int rows = 0;
    for (std::int64_t ms = 0; iso >> ms;) {
        std::string stamp;
        iso >> stamp;
        rebuilt += std::to_string(ms) + " " + time::format_iso_ms(ms) + "\n";
        CHECK(time::parse_iso_ms(stamp) == ms);
        ++rows;
    }
    CHECK(rows == 8);
    CHECK(rebuilt == slurp(golden / "iso.txt"));
    std::string aliases;
    for (const auto& a : ssh::aliases(golden / "ssh/config")) aliases += a + "\n";
    CHECK(aliases == slurp(golden / "ssh/aliases.txt"));
}

TEST_CASE("async_util: touches push the deadline out") {
    KeyedDebouncer<std::string> d(2s);
    const auto t0 = std::chrono::steady_clock::time_point{};
    d.touch("a", t0);
    d.touch("a", t0 + 1s);
    d.touch("b", t0 + 1s);
    CHECK(d.next() == t0 + 3s);
    CHECK(d.take_due(t0 + 2500ms, false).empty());
    CHECK(d.take_due(t0 + 3500ms, false) == std::vector<std::string>{"a", "b"});
    d.touch("c", t0);
    CHECK(d.take_due(t0, true) == std::vector<std::string>{"c"});
    CHECK(d.empty());
}

TEST_CASE("registry: registers, looks up and replaces by id") {
    Registry<std::function<int()>> r;
    for (const auto& [id, v] : {std::pair{"b", 2}, std::pair{"a", 1}, std::pair{"b", 3}})
        r.insert(id, std::make_shared<std::function<int()>>([v = v] { return v; }));
    std::vector<std::pair<std::string, int>> seen;
    for (const auto& [id, f] : r) seen.emplace_back(id, (*f)());
    CHECK(seen == std::vector<std::pair<std::string, int>>{{"a", 1}, {"b", 3}});
    CHECK_FALSE(r.get("c"));
    CHECK(r.size() == 2);
}
