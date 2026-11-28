// omp one-shot calls: `omp -p` on `@smol` without session or memory.
#include <doctest/doctest.h>

#include <algorithm>
#include <string>
#include <vector>

#include "../kit/support.hpp"
#include "loa/harness/omp/oneshot.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

using loa::harness::omp::MEMORY_OFF;
using loa::harness::omp::OneShot;
using loa::harness::test::block_on;
using loa::harness::test::read_file;

TEST_CASE("omp adapter: one shot runs smol without session or memory and reports failures") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    const auto exe = dir.path() / "omp";
    const auto d = dir.path().string();
    const std::string script =
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '" + d + "/args'\ncat > '" + d + "/stdin'\n"
        "while [ $# -gt 0 ]; do if [ \"$1\" = --config ]; then cp \"$2\" '" + d + "/overlay'; fi; shift; done\n"
        "if grep -q fail '" + d + "/stdin'; then echo 'no smol model configured' >&2; exit 2; fi\nprintf answer\n";
    REQUIRE(loa::runtime::exec::write_executable(exe, script));
    const OneShot omp(loa::runtime::exec::resolve(exe), dir.path());

    auto answer = block_on(exec, omp.summarize("Summarize: ", "the input"));
    REQUIRE_MESSAGE(answer, (answer ? std::string() : answer.error()));
    CHECK(*answer == "answer");
    std::vector<std::string> args;
    {
        const auto text = read_file(dir.path() / "args");
        std::size_t start = 0;
        for (auto end = text.find('\n'); end != std::string::npos; start = end + 1, end = text.find('\n', start))
            args.push_back(text.substr(start, end - start));
    }
    for (const char* flag : {"-p", "--no-session", "--no-tools", "--no-title"})
        CHECK_MESSAGE(std::ranges::find(args, flag) != args.end(), flag);
    const auto model = std::ranges::find(args, "--model");
    REQUIRE(model != args.end());
    CHECK(*std::next(model) == "@smol");
    CHECK(read_file(dir.path() / "overlay") == MEMORY_OFF);
    CHECK(read_file(dir.path() / "stdin") == "Summarize: the input");
    CHECK(omp.pick(" a/b ") == "a/b");
    CHECK(omp.pick(std::nullopt) == "@smol");
    CHECK(omp.pick("  ") == "@smol");

    auto failed = block_on(exec, omp.ask("@smol", "fail"));
    REQUIRE_FALSE(failed);
    CHECK_MESSAGE(failed.error().find("no smol model configured") != std::string::npos, failed.error());

    const OneShot missing(dir.path() / "absent", dir.path());
    auto absent = block_on(exec, missing.ask("@smol", "hi"));
    REQUIRE_FALSE(absent);
    CHECK_MESSAGE(absent.error().find("can't run") != std::string::npos, absent.error());
}

TEST_CASE("omp adapter: one shot gives up after its timeout") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    const auto exe = dir.path() / "omp";
    REQUIRE(loa::runtime::exec::write_executable(exe, "#!/bin/sh\ncat >/dev/null\nsleep 5\n"));
    const auto omp = OneShot(exe, dir.path()).with_timeout(std::chrono::milliseconds(200));
    auto answer = block_on(exec, omp.ask("@smol", "hi"));
    REQUIRE_FALSE(answer);
    CHECK_MESSAGE(answer.error().find("no answer within") != std::string::npos, answer.error());
}
