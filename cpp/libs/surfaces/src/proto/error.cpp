#include "loa/surfaces/proto/error.hpp"

#include <format>

#include "loa/surfaces/proto/fragment.hpp"

namespace loa::surfaces::proto {

namespace {

std::string_view reject_name(RejectCode code) {
    switch (code) {
    case RejectCode::Version: return "Version";
    case RejectCode::NotFound: return "NotFound";
    case RejectCode::Unavailable: return "Unavailable";
    case RejectCode::Unspecified: break;
    }
    return "Unspecified";
}

} // namespace

std::string DatagramError::message() const {
    switch (kind_) {
    case Kind::TooLarge: return std::format("datagram of {} bytes exceeds the {} byte limit", len_, max_);
    case Kind::Unsupported: return "the connection does not carry datagrams";
    case Kind::Closed: return "the connection is closed";
    }
    return {};
}

ProtoError::ProtoError(contracts::surface::SurfaceError e) : kind_(Kind::Surface), surface_(std::move(e)) {}
ProtoError::ProtoError(DatagramError e) : kind_(Kind::Datagram), datagram_(e) {}

ProtoError ProtoError::io(std::string message) {
    ProtoError e(Kind::Io);
    e.detail_ = std::move(message);
    return e;
}

ProtoError ProtoError::decode(const loa::Error& error) {
    ProtoError e(Kind::Decode);
    e.detail_ = error.to_string();
    return e;
}

ProtoError ProtoError::protocol(std::string what) {
    ProtoError e(Kind::Protocol);
    e.detail_ = std::move(what);
    return e;
}

ProtoError ProtoError::too_large(std::uint64_t len, std::uint64_t max) {
    ProtoError e(Kind::TooLarge);
    e.len_ = len;
    e.max_ = max;
    return e;
}

ProtoError ProtoError::datagram_too_small(std::size_t max_datagram) {
    ProtoError e(Kind::DatagramTooSmall);
    e.max_ = max_datagram;
    return e;
}

ProtoError ProtoError::frame_too_large(std::size_t len, std::size_t max_datagram) {
    ProtoError e(Kind::FrameTooLarge);
    e.len_ = len;
    e.max_ = max_datagram;
    return e;
}

ProtoError ProtoError::rejected(RejectCode code, std::string reason) {
    ProtoError e(Kind::Rejected);
    e.code_ = code;
    e.detail_ = std::move(reason);
    return e;
}

ProtoError ProtoError::closed(std::string reason) {
    ProtoError e(Kind::Closed);
    e.detail_ = std::move(reason);
    return e;
}

ProtoError ProtoError::checksum() { return ProtoError(Kind::Checksum); }

ProtoError ProtoError::video(std::string message) {
    ProtoError e(Kind::Video);
    e.detail_ = std::move(message);
    return e;
}

std::string ProtoError::message() const {
    switch (kind_) {
    case Kind::Surface: return surface_->message();
    case Kind::Io: return "stream: " + detail_;
    case Kind::Decode: return "malformed message: " + detail_;
    case Kind::Datagram: return "datagram: " + datagram_->message();
    case Kind::Protocol: return "protocol violation: " + detail_;
    case Kind::TooLarge: return std::format("{} bytes exceed the {} byte limit", len_, max_);
    case Kind::DatagramTooSmall: return std::format("a {} byte datagram limit leaves no room for frame data", max_);
    case Kind::FrameTooLarge:
        return std::format("a {} byte frame needs more than {} fragments of {} bytes", len_, MAX_FRAGMENTS, max_);
    case Kind::Rejected: return std::format("the server rejected the surface ({}): {}", reject_name(code_), detail_);
    case Kind::Closed: return "session closed: " + detail_;
    case Kind::Checksum: return "document checksum mismatch";
    case Kind::Video: return "video: " + detail_;
    }
    return {};
}

loa::Error ProtoError::to_error() const {
    auto code = ErrorCode::unknown;
    switch (kind_) {
    case Kind::Surface: return surface_->to_error();
    case Kind::Io: code = ErrorCode::io; break;
    case Kind::Decode: code = ErrorCode::parse; break;
    case Kind::Closed:
    case Kind::Datagram: code = ErrorCode::io; break;
    case Kind::Rejected: code = code_ == RejectCode::NotFound ? ErrorCode::not_found : ErrorCode::unsupported; break;
    case Kind::TooLarge:
    case Kind::DatagramTooSmall:
    case Kind::FrameTooLarge: code = ErrorCode::invalid_argument; break;
    case Kind::Protocol:
    case Kind::Checksum: code = ErrorCode::parse; break;
    case Kind::Video: code = ErrorCode::internal; break;
    }
    return {code, message()};
}

} // namespace loa::surfaces::proto
