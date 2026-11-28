#pragma once

// The `Source` port: a feed read item by item until it ends (port of
// `loa_runtime::feed::Source`). Adapters over channels live in `feed.hpp`.

#include <optional>

#include "loa/contracts/async.hpp"

namespace loa::app_runtime {

// A feed read item by item until it ends.
template <typename T>
class Source {
  public:
    using Item = T;
    virtual ~Source() = default;

    // The next item; `nullopt` once the feed ended.
    [[nodiscard]] virtual contracts::Awaitable<std::optional<T>> next() = 0;
};

} // namespace loa::app_runtime
