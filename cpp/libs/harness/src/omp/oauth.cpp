#include "loa/harness/omp/oauth.hpp"

#include <asio.hpp>
#include <asio/experimental/channel.hpp>
#include <optional>
#include <variant>

namespace loa::harness::omp {
namespace {
using namespace settings::models::rpc;
using Event = std::variant<LoginUiRequest, contracts::LoginControl, RpcResult<std::string>>;
using EventChannel = asio::experimental::channel<void(asio::error_code, Event)>;

contracts::Awaitable<bool> send(std::shared_ptr<contracts::LoginChannels> channels, contracts::LoginUpdate update) {
    auto [ec] = co_await channels->updates.async_send(asio::error_code{}, std::move(update), asio::as_tuple(asio::use_awaitable));
    co_return !ec;
}
} // namespace

// Asio channels are not thread-safe, so the whole flow runs on the executor of the login's
// channels (a strand when the caller runs on a multi-threaded context).
contracts::Awaitable<void> OmpOAuthLogin::run(std::shared_ptr<contracts::LoginChannels> channels) {
    auto executor = channels->updates.get_executor();
    co_await asio::co_spawn(executor, flow(std::move(channels)), asio::use_awaitable);
}

contracts::Awaitable<void> OmpOAuthLogin::flow(std::shared_ptr<contracts::LoginChannels> channels) {
    if (!provider.available) {
        (void)co_await send(channels, contracts::login::Failed{"omp offers no login for " + provider.name + " now"});
        co_return;
    }
    auto executor = co_await asio::this_coro::executor;
    auto ui = std::make_shared<LoginUiChannel>(executor);
    auto events = std::make_shared<EventChannel>(executor, 128);
    auto rpc_ref = rpc;
    auto id = provider.id;
    asio::co_spawn(executor, [rpc_ref, id = std::move(id), ui, events]() -> contracts::Awaitable<void> {
        auto result = co_await rpc_ref->login(id, ui);
        (void)events->try_send(asio::error_code{}, Event{std::move(result)});
    }, asio::detached);
    asio::co_spawn(executor, [ui, events]() -> contracts::Awaitable<void> {
        for (;;) {
            auto [ec, frame] = co_await ui->channel.async_receive(asio::as_tuple(asio::use_awaitable));
            if (ec || !events->try_send(asio::error_code{}, Event{std::move(frame)})) co_return;
        }
    }, asio::detached);
    asio::co_spawn(executor, [channels, events]() -> contracts::Awaitable<void> {
        for (;;) {
            auto [ec, control] = co_await channels->control.async_receive(asio::as_tuple(asio::use_awaitable));
            if (ec || !events->try_send(asio::error_code{}, Event{std::move(control)})) co_return;
        }
    }, asio::detached);

    std::optional<std::string> pending;
    auto handle_ui = [&](LoginUiRequest request) -> contracts::Awaitable<bool> {
        auto* frame = &request;
        bool delivered = true;
        if (auto* url = std::get_if<OpenUrl>(frame)) {
            auto opened = opener->open_url(url->url);
            delivered = co_await send(channels, contracts::login::Url{url->url, url->launch_url, url->instructions});
            if (delivered && !opened) delivered = co_await send(channels, contracts::login::Progress{"Could not open browser: " + opened.error()});
        } else if (auto* notify = std::get_if<Notify>(frame)) {
            delivered = co_await send(channels, contracts::login::Progress{notify->message});
        } else if (auto* input = std::get_if<Input>(frame)) {
            pending = input->id;
            delivered = co_await send(channels, contracts::login::Prompt{input->title, input->placeholder});
        } else if (auto* cancel = std::get_if<Cancel>(frame)) {
            if (pending == cancel->target_id) pending.reset();
        }
        co_return delivered;
    };
    for (;;) {
        auto [ec, event] = co_await events->async_receive(asio::as_tuple(asio::use_awaitable));
        if (ec) co_return;
        if (auto* frame = std::get_if<LoginUiRequest>(&event)) {
            if (!co_await handle_ui(std::move(*frame))) break;
        } else if (auto* control = std::get_if<contracts::LoginControl>(&event)) {
            if (auto* input = std::get_if<contracts::SubmitInput>(control)) {
                if (pending) {
                    auto response = co_await rpc->respond_ui(UiValue{std::move(*pending), input->text});
                    pending.reset();
                    if (!response) {
                        (void)co_await send(channels, contracts::login::Failed{response.error().to_string()});
                        break;
                    }
                }
            } else {
                if (pending) (void)co_await rpc->respond_ui(UiCancelled{std::move(*pending), true});
                (void)co_await send(channels, contracts::login::Cancelled{});
                break;
            }
        } else if (auto* result = std::get_if<RpcResult<std::string>>(&event)) {
            // The RPC response follows all preceding UI frames on the same connection: relay
            // the frames still queued before the outcome.
            bool delivered = true;
            while (delivered) {
                std::optional<LoginUiRequest> queued;
                if (!ui->channel.try_receive([&](asio::error_code, LoginUiRequest request) { queued = std::move(request); }) || !queued) break;
                delivered = co_await handle_ui(std::move(*queued));
            }
            if (!delivered) break;
            if (*result) (void)co_await send(channels, contracts::login::Succeeded{});
            else (void)co_await send(channels, contracts::login::Failed{result->error().to_string()});
            break;
        }
    }
    events->close();
    co_return;
}

} // namespace loa::harness::omp
