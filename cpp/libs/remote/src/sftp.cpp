#include "sftp.hpp"

#include <algorithm>
#include <atomic>
#include <cstdio>

#include "loa/remote/limits.hpp"
#include "loa/runtime/time.hpp"
#include "session.hpp"
#include "wire.hpp"

namespace loa::remote::detail {

namespace {

constexpr std::string_view POSIX_RENAME = "posix-rename@openssh.com";
// Bytes per READ / WRITE request; every SFTP server accepts 32 KiB.
constexpr std::uint32_t CHUNK = 32 * 1024;
constexpr int READ_SLICE_MS = 50;

enum : std::uint8_t {
    FXP_INIT = 1,
    FXP_VERSION = 2,
    FXP_OPEN = 3,
    FXP_CLOSE = 4,
    FXP_READ = 5,
    FXP_WRITE = 6,
    FXP_LSTAT = 7,
    FXP_FSTAT = 8,
    FXP_OPENDIR = 11,
    FXP_READDIR = 12,
    FXP_REMOVE = 13,
    FXP_REALPATH = 16,
    FXP_STAT = 17,
    FXP_RENAME = 18,
    FXP_STATUS = 101,
    FXP_HANDLE = 102,
    FXP_DATA = 103,
    FXP_NAME = 104,
    FXP_ATTRS = 105,
    FXP_EXTENDED = 200,
};

enum : std::uint32_t { FX_OK = 0, FX_EOF = 1, FX_NO_SUCH_FILE = 2, FX_FAILURE = 4, FX_OP_UNSUPPORTED = 8 };

enum : std::uint32_t { READ = 0x01, WRITE = 0x02, CREATE = 0x08, TRUNCATE = 0x10, EXCLUDE = 0x20 };

enum : std::uint32_t { ATTR_SIZE = 0x1, ATTR_UIDGID = 0x2, ATTR_PERMISSIONS = 0x4, ATTR_ACMODTIME = 0x8, ATTR_EXTENDED = 0x80000000 };

// russh-sftp's `StatusCode` `Display` text.
std::string status_name(std::uint32_t code) {
    static constexpr std::string_view NAMES[] = {"Ok",           "Eof",           "No such file",    "Permission denied",    "Failure",
                                                 "Bad message",  "No connection", "Connection lost", "Operation unsupported"};
    return code < std::size(NAMES) ? std::string(NAMES[code]) : "status " + std::to_string(code);
}

std::optional<FileAttributes> read_attrs(wire::Reader& r) {
    const auto flags = r.u32();
    if (!flags) return std::nullopt;
    FileAttributes a;
    if (*flags & ATTR_SIZE) {
        a.size = r.u64();
        if (!a.size) return std::nullopt;
    }
    if (*flags & ATTR_UIDGID) {
        a.uid = r.u32();
        a.gid = r.u32();
        if (!a.uid || !a.gid) return std::nullopt;
    }
    if (*flags & ATTR_PERMISSIONS) {
        a.permissions = r.u32();
        if (!a.permissions) return std::nullopt;
    }
    if (*flags & ATTR_ACMODTIME) {
        a.atime = r.u32();
        a.mtime = r.u32();
        if (!a.atime || !a.mtime) return std::nullopt;
    }
    if (*flags & ATTR_EXTENDED) {
        const auto count = r.u32();
        if (!count) return std::nullopt;
        for (std::uint32_t i = 0; i < *count; ++i) {
            if (!r.string() || !r.string()) return std::nullopt;
        }
    }
    return a;
}

void put_attrs(wire::Bytes& out, const FileAttributes& a) {
    std::uint32_t flags = 0;
    if (a.permissions) flags |= ATTR_PERMISSIONS;
    wire::put_u32(out, flags);
    if (a.permissions) wire::put_u32(out, *a.permissions);
}

std::string temp_name(const std::string& path) {
    static std::atomic<std::uint64_t> counter{0};
    const auto [dir, name] = split(path);
    const auto sep = dir == "/" ? "" : "/";
    char hex[32];
    std::snprintf(hex, sizeof hex, "%llx", static_cast<unsigned long long>(loa::runtime::time::now_us()));
    return std::string(dir) + sep + "." + std::string(name) + ".loa-" + hex + "-" + std::to_string(counter.fetch_add(1)) + ".tmp";
}

} // namespace

bool FileAttributes::is_symlink() const noexcept { return permissions && (*permissions & 0170000) == 0120000; }
bool FileAttributes::is_dir() const noexcept { return permissions && (*permissions & 0170000) == 0040000; }
bool FileAttributes::is_regular() const noexcept { return permissions && (*permissions & 0170000) == 0100000; }

bool is_server_status(const Error& e) noexcept { return e.kind() == ErrorKind::sftp && e.reason().starts_with("status:"); }

// A reply: its type, and either a status code or the body after the request id.
struct Sftp::Reply {
    std::uint8_t type = 0;
    std::uint32_t status = FX_OK;
    std::string body;
};

namespace {

// The error for a failed `what path` request: a server status, or the channel failing.
struct Failure {
    std::optional<std::uint32_t> status;
    std::string message;
};

} // namespace

Result<std::shared_ptr<Sftp>> Sftp::open(std::shared_ptr<Link> link, const std::string& host) {
    auto channel = open_channel(*link, host);
    if (!channel) return std::unexpected(channel.error());
    {
        std::lock_guard lock(link->mu);
        if (ssh_channel_request_subsystem(*channel, "sftp") != SSH_OK) {
            auto e = Error::ssh(host, ssh_get_error(link->ssh));
            ssh_channel_free(*channel);
            return std::unexpected(std::move(e));
        }
    }
    std::shared_ptr<Sftp> sftp(new Sftp(link, *channel, host));
    wire::Bytes init;
    wire::put_u32(init, 5);
    wire::put_u8(init, FXP_INIT);
    wire::put_u32(init, 3);
    std::lock_guard guard(sftp->mu_);
    if (auto sent = sftp->send(init); !sent) return std::unexpected(Error::sftp(host, sent.error().reason()));
    auto packet = sftp->receive();
    if (!packet) return std::unexpected(Error::sftp(host, packet.error().reason()));
    wire::Reader r(*packet);
    if (r.u8() != FXP_VERSION || !r.u32()) return std::unexpected(Error::sftp(host, "Unexpected packet"));
    while (!r.empty()) {
        const auto name = r.string();
        const auto data = r.string();
        if (!name || !data) break;
        if (*name == POSIX_RENAME && *data == "1") sftp->posix_rename_ = true;
    }
    return sftp;
}

Sftp::~Sftp() { close_channel(*link_, channel_); }

Result<void> Sftp::send(const std::string& packet) {
    std::lock_guard lock(link_->mu);
    if (ssh_channel_write(channel_, packet.data(), static_cast<std::uint32_t>(packet.size())) == SSH_ERROR) {
        return std::unexpected(Error::sftp(host_, std::string("I/O: ") + ssh_get_error(link_->ssh)));
    }
    return {};
}

Result<std::string> Sftp::receive() {
    char buf[CHUNK + 1024];
    for (;;) {
        if (inbox_.size() >= 4) {
            const auto len = *wire::Reader(std::string_view(inbox_).substr(0, 4)).u32();
            if (inbox_.size() >= 4 + static_cast<std::size_t>(len)) {
                auto packet = inbox_.substr(4, len);
                inbox_.erase(0, 4 + static_cast<std::size_t>(len));
                return packet;
            }
        }
        std::lock_guard lock(link_->mu);
        const int n = ssh_channel_read_timeout(channel_, buf, sizeof buf, 0, READ_SLICE_MS);
        if (n == SSH_ERROR) return std::unexpected(Error::sftp(host_, std::string("I/O: ") + ssh_get_error(link_->ssh)));
        if (n > 0) {
            inbox_.append(buf, static_cast<std::size_t>(n));
        } else if (ssh_channel_is_eof(channel_) || ssh_channel_is_closed(channel_)) {
            return std::unexpected(Error::sftp(host_, "I/O: the SFTP channel closed"));
        }
    }
}

Result<Sftp::Reply> Sftp::request(std::uint8_t type, const std::string& body) {
    std::lock_guard guard(mu_);
    const auto id = ++next_id_;
    wire::Bytes packet;
    wire::put_u32(packet, static_cast<std::uint32_t>(1 + 4 + body.size()));
    wire::put_u8(packet, type);
    wire::put_u32(packet, id);
    packet += body;
    if (auto sent = send(packet); !sent) return std::unexpected(sent.error());
    auto got = receive();
    if (!got) return std::unexpected(got.error());
    wire::Reader r(*got);
    const auto reply_type = r.u8();
    const auto reply_id = r.u32();
    if (!reply_type || reply_id != id) return std::unexpected(Error::sftp(host_, "Unexpected packet"));
    Reply reply{*reply_type, FX_OK, std::string(r.rest())};
    if (reply.type == FXP_STATUS) {
        wire::Reader s(reply.body);
        reply.status = s.u32().value_or(FX_FAILURE);
    }
    return reply;
}

namespace {

// `status: what path: code` for a server status, else `what path: message`.
Error failed(const std::string& host, std::string_view what, std::string_view path, std::uint32_t status) {
    return Error::sftp(host, "status: " + std::string(what) + " " + std::string(path) + ": " + status_name(status));
}

Error transport(const std::string& host, std::string_view what, std::string_view path, const Error& e) {
    return Error::sftp(host, std::string(what) + " " + std::string(path) + ": " + e.reason());
}

Error unexpected(const std::string& host, std::string_view what, std::string_view path) {
    return Error::sftp(host, std::string(what) + " " + std::string(path) + ": Unexpected packet");
}

std::string path_body(std::string_view path) {
    wire::Bytes b;
    wire::put_string(b, path);
    return b;
}

} // namespace

Result<std::string> Sftp::realpath(const std::string& path) {
    auto reply = request(FXP_REALPATH, path_body(path));
    if (!reply) return std::unexpected(transport(host_, "realpath", path, reply.error()));
    if (reply->type == FXP_STATUS) return std::unexpected(failed(host_, "realpath", path, reply->status));
    if (reply->type != FXP_NAME) return std::unexpected(unexpected(host_, "realpath", path));
    wire::Reader r(reply->body);
    const auto count = r.u32();
    const auto name = r.string();
    if (!count || *count == 0 || !name) return std::unexpected(Error::sftp(host_, "realpath " + path + ": empty reply"));
    return std::string(*name);
}

Result<std::optional<FileAttributes>> Sftp::stat(const std::string& path, bool follow) {
    auto reply = request(follow ? FXP_STAT : FXP_LSTAT, path_body(path));
    if (!reply) return std::unexpected(transport(host_, "stat", path, reply.error()));
    if (reply->type == FXP_STATUS) {
        if (reply->status == FX_NO_SUCH_FILE) return std::optional<FileAttributes>{};
        return std::unexpected(failed(host_, "stat", path, reply->status));
    }
    wire::Reader r(reply->body);
    auto attrs = reply->type == FXP_ATTRS ? read_attrs(r) : std::nullopt;
    if (!attrs) return std::unexpected(unexpected(host_, "stat", path));
    return attrs;
}

Result<std::string> Sftp::read(const std::string& path, std::uint64_t max) {
    wire::Bytes open;
    wire::put_string(open, path);
    wire::put_u32(open, READ);
    put_attrs(open, {});
    auto reply = request(FXP_OPEN, open);
    if (!reply) return std::unexpected(transport(host_, "open", path, reply.error()));
    if (reply->type == FXP_STATUS) return std::unexpected(failed(host_, "open", path, reply->status));
    wire::Reader hr(reply->body);
    const auto handle_view = reply->type == FXP_HANDLE ? hr.string() : std::nullopt;
    if (!handle_view) return std::unexpected(unexpected(host_, "open", path));
    const std::string handle(*handle_view);
    const auto handle_body = path_body(handle);

    auto result = [&]() -> Result<std::string> {
        auto st = request(FXP_FSTAT, handle_body);
        if (!st) return std::unexpected(transport(host_, "fstat", path, st.error()));
        if (st->type == FXP_STATUS) return std::unexpected(failed(host_, "fstat", path, st->status));
        wire::Reader ar(st->body);
        const auto attrs = st->type == FXP_ATTRS ? read_attrs(ar) : std::nullopt;
        if (!attrs) return std::unexpected(unexpected(host_, "fstat", path));
        if (attrs->size && *attrs->size > max) return std::unexpected(Error::too_large(path, *attrs->size, max));
        std::string out;
        if (attrs->size) out.reserve(static_cast<std::size_t>(*attrs->size));
        for (;;) {
            wire::Bytes req = handle_body;
            wire::put_u64(req, out.size());
            wire::put_u32(req, CHUNK);
            auto data = request(FXP_READ, req);
            if (!data) return std::unexpected(transport(host_, "read", path, data.error()));
            if (data->type == FXP_STATUS) {
                if (data->status == FX_EOF) break;
                return std::unexpected(failed(host_, "read", path, data->status));
            }
            wire::Reader dr(data->body);
            const auto bytes = data->type == FXP_DATA ? dr.string() : std::nullopt;
            if (!bytes) return std::unexpected(unexpected(host_, "read", path));
            if (bytes->empty()) break;
            out.append(*bytes);
            if (out.size() > max) return std::unexpected(Error::too_large(path, out.size(), max));
        }
        return out;
    }();
    (void)request(FXP_CLOSE, handle_body);
    return result;
}

Result<std::vector<std::pair<std::string, FileAttributes>>> Sftp::list(const std::string& path) {
    auto reply = request(FXP_OPENDIR, path_body(path));
    if (!reply) return std::unexpected(transport(host_, "opendir", path, reply.error()));
    if (reply->type == FXP_STATUS) return std::unexpected(failed(host_, "opendir", path, reply->status));
    wire::Reader hr(reply->body);
    const auto handle_view = reply->type == FXP_HANDLE ? hr.string() : std::nullopt;
    if (!handle_view) return std::unexpected(unexpected(host_, "opendir", path));
    const auto handle_body = path_body(*handle_view);
    std::vector<std::pair<std::string, FileAttributes>> out;
    auto result = [&]() -> Result<void> {
        for (;;) {
            auto names = request(FXP_READDIR, handle_body);
            if (!names) return std::unexpected(transport(host_, "readdir", path, names.error()));
            if (names->type == FXP_STATUS) {
                if (names->status == FX_EOF) return {};
                return std::unexpected(failed(host_, "readdir", path, names->status));
            }
            if (names->type != FXP_NAME) return std::unexpected(unexpected(host_, "readdir", path));
            wire::Reader r(names->body);
            const auto count = r.u32();
            if (!count) return std::unexpected(unexpected(host_, "readdir", path));
            for (std::uint32_t i = 0; i < *count; ++i) {
                const auto name = r.string();
                const auto longname = r.string();
                auto attrs = name && longname ? read_attrs(r) : std::nullopt;
                if (!attrs) return std::unexpected(unexpected(host_, "readdir", path));
                if (*name != "." && *name != "..") out.emplace_back(std::string(*name), *attrs);
            }
        }
    }();
    (void)request(FXP_CLOSE, handle_body);
    if (!result) return std::unexpected(result.error());
    std::ranges::sort(out, {}, &std::pair<std::string, FileAttributes>::first);
    return out;
}

Result<WriteMode> Sftp::write(const std::string& path, std::string_view data) {
    auto existing = stat(path, false);
    if (!existing) return std::unexpected(existing.error());
    FileAttributes attrs;
    if (*existing && (*existing)->permissions) attrs.permissions = *(*existing)->permissions & 07777;

    // Opens `target` with `flags`: the handle, a server status (`Failure::status`), or a transport error.
    const auto open = [&](const std::string& target, std::uint32_t flags) -> std::expected<std::string, Failure> {
        wire::Bytes body;
        wire::put_string(body, target);
        wire::put_u32(body, flags);
        put_attrs(body, attrs);
        auto reply = request(FXP_OPEN, body);
        if (!reply) return std::unexpected(Failure{std::nullopt, reply.error().reason()});
        if (reply->type == FXP_STATUS) return std::unexpected(Failure{reply->status, {}});
        wire::Reader r(reply->body);
        const auto handle = reply->type == FXP_HANDLE ? r.string() : std::nullopt;
        if (!handle) return std::unexpected(Failure{std::nullopt, "Unexpected packet"});
        return std::string(*handle);
    };
    const auto open_error = [&](const std::string& target, const Failure& f) {
        return f.status ? failed(host_, "open", target, *f.status) : Error::sftp(host_, "open " + target + ": " + f.message);
    };
    const auto write_handle = [&](const std::string& handle, const std::string& target) -> Result<void> {
        for (std::size_t offset = 0; offset < data.size(); offset += CHUNK) {
            wire::Bytes body = path_body(handle);
            wire::put_u64(body, offset);
            wire::put_string(body, data.substr(offset, CHUNK));
            auto reply = request(FXP_WRITE, body);
            if (!reply) return std::unexpected(transport(host_, "write", target, reply.error()));
            if (reply->type != FXP_STATUS) return std::unexpected(unexpected(host_, "write", target));
            if (reply->status != FX_OK) return std::unexpected(failed(host_, "write", target, reply->status));
        }
        return {};
    };
    const auto close = [&](const std::string& handle, const std::string& target) -> Result<void> {
        auto reply = request(FXP_CLOSE, path_body(handle));
        if (!reply) return std::unexpected(transport(host_, "close", target, reply.error()));
        if (reply->type != FXP_STATUS) return std::unexpected(unexpected(host_, "close", target));
        if (reply->status != FX_OK) return std::unexpected(failed(host_, "close", target, reply->status));
        return {};
    };
    const auto remove = [&](const std::string& target) { (void)request(FXP_REMOVE, path_body(target)); };
    // `false` when the server won't replace `to` (plain SFTPv3 rename onto an existing file).
    const auto rename_over = [&](const std::string& from, const std::string& to) -> Result<bool> {
        Result<Reply> reply = std::unexpected(Error::cancelled());
        if (posix_rename_) {
            wire::Bytes body;
            wire::put_string(body, POSIX_RENAME);
            wire::put_string(body, from);
            wire::put_string(body, to);
            reply = request(FXP_EXTENDED, body);
        } else {
            wire::Bytes body;
            wire::put_string(body, from);
            wire::put_string(body, to);
            reply = request(FXP_RENAME, body);
        }
        if (!reply) return std::unexpected(transport(host_, "rename", to, reply.error()));
        if (reply->type != FXP_STATUS) return std::unexpected(unexpected(host_, "rename", to));
        if (reply->status == FX_OK) return true;
        if (reply->status == FX_FAILURE || reply->status == FX_OP_UNSUPPORTED) return false;
        return std::unexpected(failed(host_, "rename", to, reply->status));
    };

    const auto tmp = temp_name(path);
    auto handle = open(tmp, WRITE | CREATE | EXCLUDE);
    if (handle) {
        auto written = write_handle(*handle, tmp);
        auto closed = close(*handle, tmp);
        Result<bool> renamed = !written ? Result<bool>(std::unexpected(written.error()))
                               : !closed ? Result<bool>(std::unexpected(closed.error()))
                                         : rename_over(tmp, path);
        if (!renamed) {
            remove(tmp);
            return std::unexpected(renamed.error());
        }
        if (*renamed) return WriteMode::atomic;
        remove(tmp);
    } else if (!handle.error().status) {
        return std::unexpected(open_error(tmp, handle.error()));
    }
    auto target = open(path, WRITE | CREATE | TRUNCATE);
    if (!target) return std::unexpected(open_error(path, target.error()));
    auto written = write_handle(*target, path);
    auto closed = close(*target, path);
    if (!written) return std::unexpected(written.error());
    if (!closed) return std::unexpected(closed.error());
    return WriteMode::in_place;
}

} // namespace loa::remote::detail
