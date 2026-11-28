#include "loa/settings/models/error.hpp"

#include <utility>

namespace loa::settings::models {

RpcError RpcError::command_failed(std::string command, std::string message, std::optional<std::string> code) {
    RpcError e;
    e.kind = Kind::Command;
    e.command = std::move(command);
    e.message = std::move(message);
    e.code = std::move(code);
    return e;
}

RpcError RpcError::transport(std::string message) {
    RpcError e;
    e.kind = Kind::Transport;
    e.message = std::move(message);
    return e;
}

std::string RpcError::to_string() const {
    switch (kind) {
    case Kind::Command:
        return "omp " + command + " failed: " + message;
    case Kind::Transport:
        return "omp RPC connection failed: " + message;
    }
    return message;
}

namespace {
Error make(Error::Kind kind) {
    Error e;
    e.kind = kind;
    return e;
}
} // namespace

Error Error::io(std::filesystem::path path, loa::Error source) {
    Error e = make(Kind::Io);
    e.path = std::move(path);
    e.source = std::move(source);
    return e;
}

Error Error::invalid(std::string field, std::string reason) {
    Error e = make(Kind::Invalid);
    e.field = std::move(field);
    e.reason = std::move(reason);
    return e;
}

Error Error::parse(std::string what, std::string message) {
    Error e = make(Kind::Parse);
    e.what = std::move(what);
    e.message = std::move(message);
    return e;
}

Error Error::unsupported(std::string what, std::string reason) {
    Error e = make(Kind::Unsupported);
    e.what = std::move(what);
    e.reason = std::move(reason);
    return e;
}

Error Error::edit_check(std::string path) {
    Error e = make(Kind::EditCheck);
    e.path = std::move(path);
    return e;
}

Error Error::changed_on_disk(std::filesystem::path path) {
    Error e = make(Kind::ChangedOnDisk);
    e.path = std::move(path);
    return e;
}

Error Error::rejected(std::string what, std::string message) {
    Error e = make(Kind::Rejected);
    e.what = std::move(what);
    e.message = std::move(message);
    return e;
}

Error Error::already_exists(std::filesystem::path path) {
    Error e = make(Kind::AlreadyExists);
    e.path = std::move(path);
    return e;
}

Error Error::process(std::string program, std::string message) {
    Error e = make(Kind::Process);
    e.program = std::move(program);
    e.message = std::move(message);
    return e;
}

Error Error::from_rpc(RpcError rpc) {
    Error e = make(Kind::Rpc);
    e.rpc = std::move(rpc);
    return e;
}

std::string Error::to_string() const {
    switch (kind) {
    case Kind::Io:
        return "cannot access " + path.string() + ": " + source.to_string();
    case Kind::Parse:
        return what + " does not parse: " + message;
    case Kind::Unsupported:
        return "cannot edit " + what + ": " + reason;
    case Kind::EditCheck:
        return "editing " + path.string() + " would change more than the edited key; nothing was written";
    case Kind::ChangedOnDisk:
        return path.string() + " changed on disk since it was loaded";
    case Kind::Invalid:
        return "invalid value for " + field + ": " + reason;
    case Kind::Rejected:
        return "omp rejected " + what + ": " + message;
    case Kind::AlreadyExists:
        return path.string() + " already exists";
    case Kind::Process:
        return "cannot run " + program + ": " + message;
    case Kind::Rpc:
        return rpc.to_string();
    }
    return {};
}

loa::Error Error::to_error() const {
    ErrorCode code = ErrorCode::unknown;
    switch (kind) {
    case Kind::Io:
        return source.context(to_string());
    case Kind::Parse:
        code = ErrorCode::parse;
        break;
    case Kind::Unsupported:
        code = ErrorCode::unsupported;
        break;
    case Kind::EditCheck:
    case Kind::ChangedOnDisk:
    case Kind::Rejected:
    case Kind::Process:
    case Kind::Rpc:
        code = ErrorCode::internal;
        break;
    case Kind::Invalid:
        code = ErrorCode::invalid_argument;
        break;
    case Kind::AlreadyExists:
        code = ErrorCode::already_exists;
        break;
    }
    return {code, to_string()};
}

namespace {

// UTF-8 validity, as Rust's `String::from_utf8`.
bool valid_utf8(std::string_view s) {
    std::size_t i = 0;
    while (i < s.size()) {
        const auto c = static_cast<unsigned char>(s[i]);
        std::size_t n = 0;
        std::uint32_t cp = 0;
        if (c < 0x80) {
            ++i;
            continue;
        }
        if ((c & 0xE0) == 0xC0) {
            n = 1;
            cp = c & 0x1F;
        } else if ((c & 0xF0) == 0xE0) {
            n = 2;
            cp = c & 0x0F;
        } else if ((c & 0xF8) == 0xF0) {
            n = 3;
            cp = c & 0x07;
        } else {
            return false;
        }
        if (i + n >= s.size()) {
            return false;
        }
        for (std::size_t k = 1; k <= n; ++k) {
            const auto cc = static_cast<unsigned char>(s[i + k]);
            if ((cc & 0xC0) != 0x80) {
                return false;
            }
            cp = (cp << 6) | (cc & 0x3F);
        }
        if ((n == 1 && cp < 0x80) || (n == 2 && cp < 0x800) || (n == 3 && (cp < 0x10000 || cp > 0x10FFFF)) ||
            (cp >= 0xD800 && cp <= 0xDFFF)) {
            return false;
        }
        i += n + 1;
    }
    return true;
}

Result<std::optional<std::string>> text(const std::filesystem::path& path, const std::optional<std::string>& bytes) {
    if (!bytes) {
        return std::optional<std::string>{};
    }
    if (!valid_utf8(*bytes)) {
        return std::unexpected(Error::io(path, loa::Error(ErrorCode::io, "stream did not contain valid UTF-8")));
    }
    return std::optional<std::string>{*bytes};
}

} // namespace

Result<std::optional<std::string>> read_optional(const std::filesystem::path& path) {
    auto bytes = runtime::fs::read_optional(path);
    if (!bytes) {
        return std::unexpected(Error::io(path, bytes.error()));
    }
    return text(path, *bytes);
}

Result<std::pair<runtime::fs::Snapshot, std::optional<std::string>>> load_snapshot(const std::filesystem::path& path) {
    auto snapshot = runtime::fs::Snapshot::load(path);
    if (!snapshot) {
        return std::unexpected(Error::io(path, snapshot.error()));
    }
    auto t = text(path, snapshot->contents());
    if (!t) {
        return std::unexpected(std::move(t.error()));
    }
    return std::pair{std::move(*snapshot), std::move(*t)};
}

Result<> save_snapshot(runtime::fs::Snapshot& snapshot, std::string_view text) {
    auto saved = snapshot.save(text);
    if (!saved) {
        return std::unexpected(Error::io(snapshot.path(), saved.error()));
    }
    if (!*saved) {
        return std::unexpected(Error::changed_on_disk(snapshot.path()));
    }
    return {};
}

} // namespace loa::settings::models
