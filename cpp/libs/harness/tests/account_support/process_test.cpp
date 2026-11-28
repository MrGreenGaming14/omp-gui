#include <doctest/doctest.h>

#include "../kit/support.hpp"
#include "loa/contracts/login_channels.hpp"
#include "loa/harness/account_support/cli.hpp"
#include "loa/harness/account_support/process.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::harness::account_support;
using loa::harness::test::block_on;
using Kind = loa::settings::models::Error::Kind;
namespace login = loa::contracts::login;

namespace {

// An executable `#!/bin/sh` script named `name` in `dir` with `body` after the shebang.
std::filesystem::path fake_program(const std::filesystem::path& dir, std::string_view name, std::string_view body) {
    const auto path = dir / name;
    REQUIRE(loa::runtime::exec::write_executable(path, "#!/bin/sh\n" + std::string(body) + "\n"));
    return loa::runtime::exec::resolve(path);
}

} // namespace

TEST_CASE("account_support: output captures streams, exit code and env") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto prog = fake_program(dir.path(), "tool", "printf 'out:%s:%s' \"$1\" \"$LOA_TEST_VAR\"\nprintf 'err' >&2\nexit 3");
    auto spec = CommandSpec::make(prog, {"first arg"});
    spec.env.emplace_back("LOA_TEST_VAR", "v1");
    auto out = block_on(exec, AsioProcessRunner{}.output(spec)).value();
    CHECK(out.code == 3);
    CHECK(out.out == "out:first arg:v1");
    CHECK(out.err == "err");
}

TEST_CASE("account_support: output runs in current_dir") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    const auto work = dir.path() / "work";
    std::filesystem::create_directory(work);
    auto prog = fake_program(dir.path(), "tool", "pwd");
    auto spec = CommandSpec::make(prog, {});
    spec.current_dir = work;
    auto out = block_on(exec, AsioProcessRunner{}.output(spec)).value();
    std::string printed = out.out;
    while (!printed.empty() && printed.back() == '\n') printed.pop_back();
    CHECK(std::filesystem::canonical(printed) == std::filesystem::canonical(work));
}

TEST_CASE("account_support: a missing program is a process error") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto spec = CommandSpec::make(dir.path() / "absent", {"x"});
    auto out = block_on(exec, AsioProcessRunner{}.output(spec));
    REQUIRE_FALSE(out);
    CHECK(out.error().kind == Kind::Process);
    auto spawned = block_on(exec, AsioProcessRunner{}.spawn(spec));
    REQUIRE_FALSE(spawned);
    CHECK(spawned.error().kind == Kind::Process);
}

TEST_CASE("account_support: a spawned process reads stdin and writes stdout") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto prog = fake_program(dir.path(), "tool", "printf 'Paste code: '\nread code\nprintf 'got %s' \"$code\"\nexit 5");
    auto proc = block_on(exec, AsioProcessRunner{}.spawn(CommandSpec::make(prog, {}))).value();
    auto in = proc->take_stdin();
    REQUIRE(in);
    REQUIRE(block_on(exec, in->write_all("abc123\n")));
    in.reset();
    auto out = proc->take_stdout();
    CHECK(block_on(exec, out->read_to_end()).value() == "Paste code: got abc123");
    CHECK(block_on(exec, proc->wait()).value() == 5);
    CHECK(proc->take_stdout() == nullptr);
}

TEST_CASE("account_support: kill ends a process waiting for input") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto prog = fake_program(dir.path(), "tool", "read line\nexit 0");
    auto proc = block_on(exec, AsioProcessRunner{}.spawn(CommandSpec::make(prog, {}))).value();
    auto keep = proc->take_stdin();
    REQUIRE(block_on(exec, proc->kill()));
    CHECK(block_on(exec, proc->wait()).value() == std::nullopt);
}

TEST_CASE("account_support: the line splitter strips escapes and spots prompts") {
    cli::LineSplitter lines;
    CHECK(lines.push("\x1b[1mOpening\x1b[0m https://auth.example/x?y=1.\r\nPaste code") ==
          std::vector<cli::Line>{cli::Text{"Opening https://auth.example/x?y=1."}});
    CHECK(lines.push(" here: ") == std::vector<cli::Line>{cli::Prompt{"Paste code here:"}});
    CHECK(cli::first_url("Opening https://auth.example/x?y=1.") == "https://auth.example/x?y=1");
    CHECK(cli::first_url("no url") == std::nullopt);
    CHECK(cli::first_url("see (http://a.b/c) now") == "http://a.b/c");
    CHECK(cli::clean("\x1b]0;title\x07 hi \x1b]8;;x\x1b\\there\t") == "hi there");
    CHECK(lines.finish().empty());
    (void)lines.push("tail");
    CHECK(lines.finish() == std::vector<cli::Line>{cli::Text{"tail"}});
}

namespace {

// A login run on one strand with its channels, driven from the test thread.
struct LoginRun {
    explicit LoginRun(loa::runtime::Executor& exec)
        : strand(asio::make_strand(exec.context())), channels(std::make_shared<loa::contracts::LoginChannels>(strand, 16)) {}

    std::future<void> start(std::shared_ptr<loa::contracts::LoginStrategy> strategy) {
        auto run = [](std::shared_ptr<loa::contracts::LoginStrategy> s,
                      std::shared_ptr<loa::contracts::LoginChannels> ch) -> asio::awaitable<void> { co_await s->run(std::move(ch)); };
        return asio::co_spawn(strand, run(std::move(strategy), channels), asio::use_future);
    }
    loa::contracts::LoginUpdate next() {
        auto ch = channels;
        auto receive = [](std::shared_ptr<loa::contracts::LoginChannels> c) -> asio::awaitable<loa::contracts::LoginUpdate> {
            co_return co_await c->updates.async_receive(asio::use_awaitable);
        };
        return asio::co_spawn(strand, receive(ch), asio::use_future).get();
    }
    void send(loa::contracts::LoginControl control) {
        auto deliver = [](std::shared_ptr<loa::contracts::LoginChannels> c, loa::contracts::LoginControl ctl) -> asio::awaitable<void> {
            co_await c->control.async_send(asio::error_code{}, std::move(ctl), asio::use_awaitable);
        };
        asio::co_spawn(strand, deliver(channels, std::move(control)), asio::use_future).get();
    }

    asio::strand<asio::io_context::executor_type> strand;
    std::shared_ptr<loa::contracts::LoginChannels> channels;
};

} // namespace

TEST_CASE("account_support: CliLogin relays the URL, the prompt and input") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto program = fake_program(dir.path(), "claude",
                                "echo 'Visit https://claude.example/oauth?code=abc to sign in'\nprintf 'Paste code here: '\nread code\n"
                                "echo \"Logged in with $code\"\nexit 0");
    auto proc = block_on(exec, AsioProcessRunner{}.spawn(CommandSpec::make(program, {"auth", "login"}))).value();
    LoginRun run(exec);
    auto done = run.start(std::make_shared<CliLogin>(std::move(proc)));
    using U = loa::contracts::LoginUpdate;
    CHECK(run.next() == U{login::Url{"https://claude.example/oauth?code=abc", std::nullopt, std::nullopt}});
    CHECK(run.next() == U{login::Prompt{"Paste code here:", std::nullopt}});
    run.send(loa::contracts::SubmitInput{"XYZ"});
    CHECK(run.next() == U{login::Progress{"Logged in with XYZ"}});
    CHECK(run.next() == U{login::Succeeded{std::nullopt}});
    done.get();
}

TEST_CASE("account_support: cancel kills the login command") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto program = fake_program(dir.path(), "codex", "printf 'Code: '\nread code\nexit 0");
    LoginRun run(exec);
    auto done = run.start(std::make_shared<CliLoginCommand>(std::make_shared<AsioProcessRunner>(), CommandSpec::make(program, {"login"})));
    CHECK(std::holds_alternative<login::Prompt>(run.next()));
    run.send(loa::contracts::CancelLogin{});
    CHECK(run.next() == loa::contracts::LoginUpdate{login::Cancelled{}});
    done.get();
}

TEST_CASE("account_support: a failed login reports its exit code and last line") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto program = fake_program(dir.path(), "tool", "echo 'bad token' >&2\nexit 2");
    LoginRun run(exec);
    auto done = run.start(std::make_shared<CliLoginCommand>(std::make_shared<AsioProcessRunner>(), CommandSpec::make(program, {})));
    CHECK(run.next() == loa::contracts::LoginUpdate{login::Progress{"bad token"}});
    CHECK(run.next() == loa::contracts::LoginUpdate{login::Failed{"login exited with code 2: bad token"}});
    done.get();

    auto done2 = run.start(std::make_shared<CliLoginCommand>(std::make_shared<AsioProcessRunner>(), CommandSpec::make(dir.path() / "absent", {})));
    auto failed = run.next();
    REQUIRE(std::holds_alternative<login::Failed>(failed));
    CHECK(std::get<login::Failed>(failed).reason.starts_with("cannot run "));
    done2.get();
}
