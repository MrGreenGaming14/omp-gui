#include "loa/updater/error.hpp"

#include <cstdio>

namespace loa::updater {
namespace {

std::string display(const Path& p) { return p.string(); }

} // namespace

std::string debug_str(std::string_view s) {
    std::string out = "\"";
    for (const char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            case '\0': out += "\\0"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20 || c == 0x7f) {
                    char buf[12];
                    std::snprintf(buf, sizeof buf, "\\u{%x}", static_cast<unsigned>(static_cast<unsigned char>(c)));
                    out += buf;
                } else {
                    out += c;
                }
        }
    }
    return out + '"';
}

Error Error::io(const Path& path, std::string_view source) { return {Kind::Io, display(path) + ": " + std::string(source)}; }
Error Error::io(const Path& path, std::error_code ec) {
    return io(path, ec.message() + " (os error " + std::to_string(ec.value()) + ")");
}
Error Error::io(const Path& path, const loa::Error& source) { return io(path, source.to_string()); }
Error Error::json(const Path& path, std::string_view source) { return {Kind::Json, display(path) + " is not valid: " + std::string(source)}; }
Error Error::unknown_product(std::string_view id) { return {Kind::UnknownProduct, "unknown product " + std::string(id)}; }
Error Error::not_installed(std::string_view product, std::string_view reason) {
    return {Kind::NotInstalled, std::string(product) + " is not installed: " + std::string(reason)};
}
Error Error::nothing_offered(std::string_view product) {
    return {Kind::NothingOffered, "no update is offered for " + std::string(product) + "; check its feed first"};
}
Error Error::not_staged(std::string_view product, const Version& version) {
    return {Kind::NotStaged, std::string(product) + " " + version.to_string() + " is not staged yet"};
}
Error Error::no_executable(std::string_view product, const Version& version, std::string_view reason) {
    return {Kind::NoExecutable, "cannot find the executable of " + std::string(product) + " " + version.to_string() + ": " + std::string(reason)};
}
Error Error::http(std::string_view url, std::string_view reason) {
    return {Kind::Http, "request to " + std::string(url) + " failed: " + std::string(reason)};
}
Error Error::status(std::string_view url, long status) { return {Kind::Status, std::string(url) + " answered HTTP " + std::to_string(status)}; }
Error Error::rate_limited(std::uint64_t until) {
    Error e{Kind::RateLimited, "rate limited until unix time " + std::to_string(until)};
    e.retry_at_ = until;
    return e;
}
Error Error::feed(std::string_view message) { return {Kind::Feed, "feed: " + std::string(message)}; }
Error Error::unverifiable(std::string_view product, const Version& version, std::string_view what) {
    return {Kind::Unverifiable, std::string(product) + " " + version.to_string() + " has no " + std::string(what) + "; refusing to install it"};
}
Error Error::bad_name(std::string_view name) { return {Kind::BadName, debug_str(name) + " is not a single file name"}; }
Error Error::too_large(std::string_view url, std::uint64_t limit) {
    return {Kind::TooLarge, std::string(url) + " sent more than " + std::to_string(limit) + " bytes"};
}
Error Error::mismatch(std::string_view what, std::string_view expected, std::string_view actual) {
    return {Kind::Mismatch, "artifact " + std::string(what) + " mismatch: expected " + std::string(expected) + ", got " + std::string(actual)};
}
Error Error::unsafe_entry(std::string_view entry, std::string_view reason) {
    return {Kind::UnsafeEntry, "archive entry " + debug_str(entry) + " rejected: " + std::string(reason)};
}
Error Error::archive(std::string_view message) { return {Kind::Archive, "archive: " + std::string(message)}; }
Error Error::no_previous(std::string_view product) { return {Kind::NoPrevious, std::string(product) + " has no previous version to roll back to"}; }
Error Error::unsigned_artifact(const Path& artifact) {
    return {Kind::Unsigned, "signature check failed: " + display(artifact) + " has no signature"};
}
Error Error::signature(std::string_view message) { return {Kind::Signature, "signature check failed: " + std::string(message)}; }

} // namespace loa::updater
