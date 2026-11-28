#include "loa/projects/error.hpp"

namespace loa::projects {

std::string Error::to_string() const {
    switch (kind) {
        case ErrorKind::Port:
        case ErrorKind::Commit:
        case ErrorKind::Io: return detail;
        case ErrorKind::SessionNotFound: return "session " + detail + " not found";
        case ErrorKind::NodeNotFound: return "node " + std::to_string(node) + " not found in session " + detail;
        case ErrorKind::Summarizer: return "summarizer failed: " + detail;
        case ErrorKind::InvalidAnswer: return "invalid digest answer: " + detail;
        case ErrorKind::NothingToDigest: return "the session has no turns to digest";
        case ErrorKind::Arguments: return "invalid tool arguments: " + detail;
        case ErrorKind::EmptyName: return "the project name is empty";
        case ErrorKind::ProjectExists: return "a project named " + detail + " already exists";
        case ErrorKind::ChangedOnDisk: return path.string() + " was changed outside Loa since it was loaded";
    }
    return detail;
}

loa::Error Error::to_error() const {
    ErrorCode code = ErrorCode::internal;
    switch (kind) {
        case ErrorKind::SessionNotFound:
        case ErrorKind::NodeNotFound: code = ErrorCode::not_found; break;
        case ErrorKind::Arguments:
        case ErrorKind::EmptyName:
        case ErrorKind::InvalidAnswer:
        case ErrorKind::NothingToDigest: code = ErrorCode::invalid_argument; break;
        case ErrorKind::ProjectExists: code = ErrorCode::already_exists; break;
        case ErrorKind::Port:
        case ErrorKind::Io:
        case ErrorKind::ChangedOnDisk: code = ErrorCode::io; break;
        case ErrorKind::Summarizer:
        case ErrorKind::Commit: code = ErrorCode::internal; break;
    }
    return loa::Error(code, to_string());
}

} // namespace loa::projects
