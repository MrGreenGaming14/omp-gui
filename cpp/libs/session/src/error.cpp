#include "loa/session/error.hpp"

namespace loa::session {

Error Error::adapter(contracts::AdapterError error) {
    Error out(Kind::Adapter, error.message());
    out.adapter_.emplace(std::move(error));
    return out;
}

Error Error::commit(const core::CommitError& error) { return Error(Kind::Commit, core::to_string(error)); }

Error Error::journal(const bus::JournalError& error) { return Error(Kind::Journal, bus::to_string(error)); }

Error Error::bus(const bus::BusError& error) { return Error(Kind::Bus, bus::to_string(error)); }

std::string Error::to_string() const {
    switch (kind_) {
    case Kind::NoSession: return "session DOM has no root";
    case Kind::Busy: return "session is not idle";
    case Kind::InvalidSequence: return "requested patch sequence is beyond the current session";
    case Kind::ModelFixed: return "the harness does not allow model changes at this point";
    case Kind::Port: return "feature port failed: " + detail_;
    case Kind::Adapter:
    case Kind::Commit:
    case Kind::Journal:
    case Kind::Bus: return detail_;
    }
    return detail_;
}

} // namespace loa::session
