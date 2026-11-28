#include <doctest/doctest.h>

#include <unistd.h>

#include "loa/harness/kit/proc.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::harness::kit;
namespace exec = loa::runtime::exec;

TEST_CASE("kit: TailBuffer only cuts at UTF-8 boundaries") {
    TailBuffer tail(4);
    tail.push("ab");
    CHECK_FALSE(tail.truncated());
    // "abcé" is 5 bytes: cutting 1 lands on "b", a character start.
    tail.push("cé");
    CHECK(tail.truncated());
    CHECK(tail.bytes() == "bcé");
    // "bcé€" is 7 bytes: cutting 3 lands inside "é", so the cut moves past it.
    tail.push("\xe2\x82\xac");
    CHECK(tail.bytes() == "€");
    CHECK(tail.text() == "€");

    TailBuffer unlimited;
    unlimited.push(std::string(10000, 'x'));
    CHECK_FALSE(unlimited.truncated());
    CHECK(unlimited.bytes().size() == 10000);

    TailBuffer lossy;
    lossy.push("a\xff" "b");
    CHECK(lossy.text() == "a\xEF\xBF\xBD" "b");
}

TEST_CASE("kit: spawn_piped maps a missing executable to NotFound") {
    auto dir = loa::runtime::TempDir::create().value();
    exec::Command cmd{dir.path() / "no-harness", {}, {}, {}};
    auto spawned = spawn_piped(cmd);
    REQUIRE_FALSE(spawned);
    REQUIRE(spawned.error().get<RpcError::NotFound>());
    CHECK(spawned.error().get<RpcError::NotFound>()->program == (dir.path() / "no-harness").string());
    CHECK(spawned.error().to_adapter_error().get<loa::contracts::AdapterError::NotInstalled>());

    auto io = spawn_error(std::make_error_code(std::errc::permission_denied), cmd);
    REQUIRE(io.get<RpcError::Io>());
    CHECK(io.message() == "harness i/o failed: Permission denied (os error 13)");
}

TEST_CASE("kit: spawn_piped pipes stdio, applies env and cwd, and keeps a stderr tail") {
    auto dir = loa::runtime::TempDir::create().value();
    exec::Command cmd{"/bin/sh",
                      {"-c", "read line; echo \"$line:$GREETING:$(pwd)\"; echo oops >&2; exit 4"},
                      {{"GREETING", "hello"}},
                      dir.path()};
    auto piped = spawn_piped(cmd);
    REQUIRE(piped);
    const std::string input = "ping\n";
    REQUIRE(::write(piped->stdin_fd.get(), input.data(), input.size()) == static_cast<ssize_t>(input.size()));
    piped->stdin_fd.reset();
    std::string out;
    char buf[256];
    for (ssize_t n; (n = ::read(piped->stdout_fd.get(), buf, sizeof buf)) > 0;) out.append(buf, static_cast<std::size_t>(n));
    auto status = piped->child.wait();
    REQUIRE(status);
    CHECK(status->code() == 4);
    CHECK(out == "ping:hello:" + std::filesystem::canonical(dir.path()).string() + "\n");
    CHECK(piped->stderr.wait_closed(std::chrono::seconds(5)));
    CHECK(piped->stderr.joined() == "oops");
}
