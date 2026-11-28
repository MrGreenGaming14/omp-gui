// Port of the tests in crates/loa-models/src/testing.rs.

#include <doctest/doctest.h>

#include <asio/as_tuple.hpp>
#include <asio/co_spawn.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/io_context.hpp>
#include <asio/use_awaitable.hpp>
#include <asio/use_future.hpp>

#include "loa/models/testing.hpp"
#include "loa/settings/models/rpc_channel.hpp"

using namespace loa::models::testing;
namespace rpc = loa::settings::models::rpc;

TEST_CASE("login_records_and_checks_input") {
    FakeOmp fake;
    fake.script = script::AwaitInput{"code"};
    fake.login_frames.push_back(rpc::Input{"prompt", "Code", std::nullopt, std::nullopt});
    asio::io_context io;
    auto result = asio::co_spawn(
        io,
        [&]() -> loa::contracts::Awaitable<rpc::RpcResult<std::string>> {
            using namespace asio::experimental::awaitable_operators;
            auto ui = std::make_shared<rpc::LoginUiChannel>(io.get_executor());
            auto answer = [&]() -> loa::contracts::Awaitable<void> {
                auto [ec, frame] = co_await ui->channel.async_receive(asio::as_tuple(asio::use_awaitable));
                CHECK_FALSE(ec);
                CHECK(std::holds_alternative<rpc::Input>(frame));
                CHECK(co_await fake.respond_ui(rpc::UiValue{"prompt", "code"}));
            };
            co_return co_await (fake.login("provider", ui) && answer());
        },
        asio::use_future);
    io.run();
    CHECK(result.get().value() == "provider");
    CHECK(fake.responses().size() == 1);
}

TEST_CASE("completed_login_updates_provider_state") {
    FakeOmp fake;
    fake.providers.push_back({"provider", "Provider", true, false});
    fake.script = script::CompleteAfter{std::chrono::steady_clock::duration::zero()};
    asio::io_context io;
    auto result = asio::co_spawn(
        io,
        [&]() -> loa::contracts::Awaitable<std::pair<std::string, bool>> {
            auto ui = std::make_shared<rpc::LoginUiChannel>(io.get_executor());
            auto login = co_await fake.login("provider", ui);
            auto providers = co_await fake.get_login_providers();
            co_return std::pair{login.value(), providers.value()[0].authenticated};
        },
        asio::use_future);
    io.run();
    CHECK(result.get() == std::pair<std::string, bool>{"provider", true});
}
