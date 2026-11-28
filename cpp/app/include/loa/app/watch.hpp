#pragma once

// Subscriptions over shared feeds: broadcast and watch channels, queues and session writers
// (port of `loa-app/src/watch.rs`). Each runs a `Source` on the `loa::runtime` executor and
// emits its items as messages, which the Qt host (`AppHost`) delivers on the GUI thread.

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <functional>
#include <memory>
#include <optional>
#include <stop_token>
#include <string>

#include "loa/app/area.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/app_runtime/source.hpp"

namespace loa::app {

// Undelivered patches a session feed may hold before it resubscribes.
inline constexpr std::size_t FEED_CAPACITY = 4096;

// A subscription key naming a shared value by its address: copies of one `shared_ptr` are the
// same feed, a new value is a new one.
template <typename T>
[[nodiscard]] std::string ptr_key(std::string_view kind, const std::shared_ptr<T>& value) {
    char buf[2 + 2 * sizeof(void*) + 1];
    std::snprintf(buf, sizeof buf, "%p", static_cast<const void*>(value.get()));
    return std::string(kind) + "@" + buf;
}

namespace detail {
loa::runtime::Task run_watch(std::function<std::unique_ptr<app_runtime::Source<app_runtime::Feed>>()> open,
                             std::function<std::optional<Message>(app_runtime::Feed)> map, Task::Emit send,
                             std::stop_token stop);
} // namespace detail

// The items of the session feed `open` returns, as `map` turns them into messages (`nullopt`
// skips one), under `key`.
[[nodiscard]] Subscription watch_feed(std::string key,
                                      std::function<std::unique_ptr<app_runtime::Source<app_runtime::Feed>>()> open,
                                      std::function<std::optional<Message>(app_runtime::Feed)> map);

// One feed per open session: the writer's DOM, then every patch, as `lifecycle::msg::Feed`.
[[nodiscard]] Subscriptions sessions(const app_runtime::Sessions& sessions);

} // namespace loa::app
