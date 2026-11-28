#include "loa/app/watch.hpp"

#include <asio.hpp>

#include "loa/bus/queue.hpp"

namespace loa::app {

loa::runtime::Task detail::run_watch(std::function<std::unique_ptr<app_runtime::Source<app_runtime::Feed>>()> open,
                                     std::function<std::optional<Message>(app_runtime::Feed)> map, Task::Emit send,
                                     std::stop_token stop) {
    auto source = open();
    if (!source) co_return;
    while (!stop.stop_requested()) {
        auto item = co_await source->next();
        if (!item || stop.stop_requested()) co_return;
        if (auto message = map(std::move(*item))) send(std::move(*message));
    }
}

Subscription watch_feed(std::string key, std::function<std::unique_ptr<app_runtime::Source<app_runtime::Feed>>()> open,
                        std::function<std::optional<Message>(app_runtime::Feed)> map) {
    return subscription::Stream{
        std::move(key), [open = std::move(open), map = std::move(map)](Task::Emit send, std::stop_token stop,
                                                                       loa::runtime::Executor& executor) {
            executor.spawn(detail::run_watch(open, map, std::move(send), std::move(stop)));
        }};
}

Subscriptions sessions(const app_runtime::Sessions& sessions) {
    Subscriptions out;
    for (const auto& [id, session] : sessions.iter()) {
        auto writer = session.writer();
        auto open = [writer]() -> std::unique_ptr<app_runtime::Source<app_runtime::Feed>> {
            return std::make_unique<app_runtime::WriterFeed>(writer, bus::Policy::coalescing(FEED_CAPACITY));
        };
        auto map = [id = id](app_runtime::Feed feed) -> std::optional<Message> {
            if (std::holds_alternative<app_runtime::feed::Ended>(feed)) return std::nullopt;
            return msg::Session{lifecycle::msg::Feed{id, std::move(feed)}};
        };
        out.push_back(watch_feed(ptr_key("session", writer), std::move(open), std::move(map)));
    }
    return out;
}

} // namespace loa::app
