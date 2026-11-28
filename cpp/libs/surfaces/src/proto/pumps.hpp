#pragma once

// Pumps that move one source into a session's mailbox, so a session waits on a
// single queue instead of racing (and possibly dropping) reads of several sources.

#include <optional>
#include <utility>

#include "loa/surfaces/proto/transport.hpp"
#include "mailbox.hpp"

namespace loa::surfaces::proto::detail {

using StreamEvent = Result<std::optional<StreamMessage>>;

// Pushes every message as alternative 0 of `Event` until the stream ends or fails.
template <typename Event>
asio::awaitable<void> read_pump(std::shared_ptr<MessageReader> reader, std::shared_ptr<Mailbox<Event>> box) {
    for (;;) {
        StreamEvent next = co_await reader->next();
        const bool last = !next || !*next;
        if (!box->push(Event(std::in_place_index<0>, std::move(next))) || last) co_return;
    }
}

} // namespace loa::surfaces::proto::detail
