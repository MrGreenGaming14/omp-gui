#include "loa/bus/bus.hpp"

#include <format>
#include <type_traits>

#include "loa/bus/error.hpp"

namespace loa::bus {

std::string to_string(const BusError& e) {
    return std::format("session {} is already open", std::get<bus_error::AlreadyOpen>(e).session.as_str());
}

loa::Error to_error(const BusError& e) { return loa::Error(ErrorCode::already_exists, to_string(e)); }

std::string to_string(const JournalError& e) {
    namespace je = journal_error;
    return std::visit(
        [](const auto& v) -> std::string {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, je::Io>) return std::format("{}: {}", v.path.string(), v.source);
            else if constexpr (std::is_same_v<T, je::NotAJournal>) return v.path.string() + " is not a journal";
            else if constexpr (std::is_same_v<T, je::UnsupportedVersion>)
                return std::format("{} uses journal format {}, this build reads {}", v.path.string(), v.found,
                                   v.supported);
            else if constexpr (std::is_same_v<T, je::CorruptHeader>) return v.path.string() + ": header is corrupt";
            else if constexpr (std::is_same_v<T, je::Corrupt>)
                return std::format("{}: corrupt record at byte {}: {}", v.path.string(), v.offset, v.reason);
            else if constexpr (std::is_same_v<T, je::SequenceGap>)
                return std::format("patch seq {} doesn't follow seq {}", v.got, v.last);
            else if constexpr (std::is_same_v<T, je::OutOfRange>)
                return std::format("seq {} is past the last patch {}", v.seq, v.last);
            else if constexpr (std::is_same_v<T, je::TooLarge>)
                return std::format("encoded patch takes {} bytes, more than a record holds", v.bytes);
            else if constexpr (std::is_same_v<T, je::DomMismatch>)
                return std::format("the DOM is at seq {}, the journal at seq {}", v.dom, v.journal);
            else if constexpr (std::is_same_v<T, je::Apply>)
                return std::format("replaying patch {}: {}", v.seq, core::to_string(v.source));
            else if constexpr (std::is_same_v<T, je::InvalidSessionId>)
                return std::format("session id \"{}\" must be 1 to 128 ASCII letters, digits, '-', '_' or '.', and "
                                   "not start with '.'",
                                   v.id);
            else if constexpr (std::is_same_v<T, je::Exists>) return std::format("session {} already exists", v.id);
            else if constexpr (std::is_same_v<T, je::NotFound>) return std::format("session {} doesn't exist", v.id);
            else if constexpr (std::is_same_v<T, je::Locked>)
                return std::format("session at {} is open in another journal", v.path.string());
            else if constexpr (std::is_same_v<T, je::Closed>) return "the session's journal was closed";
            else return v.error.to_string();
        },
        e);
}

loa::Error to_error(const JournalError& e) {
    namespace je = journal_error;
    auto code = std::visit(
        [](const auto& v) {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, je::Io>) return ErrorCode::io;
            else if constexpr (std::is_same_v<T, je::InvalidSessionId>) return ErrorCode::invalid_argument;
            else if constexpr (std::is_same_v<T, je::Exists>) return ErrorCode::already_exists;
            else if constexpr (std::is_same_v<T, je::NotFound>) return ErrorCode::not_found;
            else if constexpr (std::is_same_v<T, je::Locked>) return ErrorCode::permission_denied;
            else if constexpr (std::is_same_v<T, je::Closed>) return ErrorCode::cancelled;
            else if constexpr (std::is_same_v<T, je::UnsupportedVersion>) return ErrorCode::unsupported;
            else if constexpr (std::is_same_v<T, je::Proto>) return v.error.code();
            else return ErrorCode::parse;
        },
        e);
    return loa::Error(code, to_string(e));
}

std::expected<std::shared_ptr<SessionWriter>, BusError> Bus::open(WriterConfig config) {
    std::scoped_lock lock(mutex_);
    if (sessions_.contains(config.session))
        return std::unexpected(BusError(bus_error::AlreadyOpen{std::move(config.session)}));
    SessionId id = config.session;
    auto writer = std::make_shared<SessionWriter>(std::move(config));
    sessions_.emplace(std::move(id), writer);
    return writer;
}

std::shared_ptr<SessionWriter> Bus::get(std::string_view session) const {
    std::scoped_lock lock(mutex_);
    auto it = sessions_.find(session);
    return it == sessions_.end() ? nullptr : it->second;
}

std::vector<SessionId> Bus::ids() const {
    std::scoped_lock lock(mutex_);
    std::vector<SessionId> out;
    out.reserve(sessions_.size());
    for (const auto& [id, _] : sessions_) out.push_back(id);
    return out;
}

std::shared_ptr<SessionWriter> Bus::close(std::string_view session) {
    std::shared_ptr<SessionWriter> writer;
    {
        std::scoped_lock lock(mutex_);
        auto it = sessions_.find(session);
        if (it == sessions_.end()) return nullptr;
        writer = std::move(it->second);
        sessions_.erase(it);
    }
    writer->close();
    return writer;
}

} // namespace loa::bus
