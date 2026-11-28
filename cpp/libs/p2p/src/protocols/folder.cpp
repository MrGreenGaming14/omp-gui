#include "loa/p2p/protocols/folder.hpp"

#include <algorithm>
#include <cctype>
#include <filesystem>
#include <format>
#include <fstream>
#include <functional>
#include <iterator>
#include <system_error>

#include "loa/p2p/auth.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/time.hpp"

namespace loa::p2p::folder {

namespace {

namespace stdfs = std::filesystem;
using trust::Action;

enum class Op { list, read, write };

// Rust `std::io::Error` display.
std::string io_text(const std::error_code& ec) { return std::format("{} (os error {})", ec.message(), ec.value()); }

std::string too_big(std::string_view verb) {
    return std::format("files over {} MiB can't be {} over the peer link", MAX_FILE >> 20, verb);
}

wire::FolderReply reply(std::string error) { return wire::make_ack(std::type_identity<wire::FolderReply>{}, std::move(error)); }

wire::FolderReply done(std::vector<wire::WireEntry> entries, Bytes data) {
    wire::FolderReply out;
    out.entries = std::move(entries);
    out.data = std::move(data);
    out.done = true;
    return out;
}

template <class T>
using Checked = std::expected<T, std::string>;

// The real path an op acts on: the folder must be covered as asked and once
// links are resolved, and the path must stay inside it.
Checked<stdfs::path> resolve(const std::function<bool(std::string_view)>& covered, bool write, const std::string& folder,
                             const std::string& path) {
    const std::string_view verb = write ? "write to" : "read";
    if (!covered(folder)) return std::unexpected(std::format("no grant lets this node {} {}", verb, folder));
    auto root = runtime::fs::canonicalize(folder);
    if (!root) return std::unexpected(std::format("{} can't be opened: {}", folder, root.error().message()));
    if (!covered(root->string())) return std::unexpected(std::format("{} leads outside the shared folder", folder));
    if (write) {
        std::error_code ec;
        auto status = stdfs::symlink_status(*root / path, ec);
        if (!ec && stdfs::is_symlink(status)) return std::unexpected(std::format("{} is a symlink", path));
        if (ec && ec != std::errc::no_such_file_or_directory) return std::unexpected(std::format("{}: {}", path, io_text(ec)));
    }
    auto target = runtime::fs::confine(*root, path);
    if (!target) {
        if (target.error().code() == ErrorCode::permission_denied)
            return std::unexpected(std::format("{} is outside the shared folder", path));
        return std::unexpected(std::format("{}: {}", path, target.error().message()));
    }
    std::error_code ec;
    auto status = stdfs::status(*target, ec);
    if (write && *target == *root) return std::unexpected("name a file to write");
    if (write && !ec && stdfs::is_directory(status)) return std::unexpected(std::format("{} is a folder", path));
    if (!write && (ec || !stdfs::exists(status)))
        return std::unexpected(std::format("{}: {}", path, io_text(ec ? ec : std::make_error_code(std::errc::no_such_file_or_directory))));
    return *target;
}

std::string lower(std::string s) {
    std::ranges::transform(s, s.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return s;
}

Checked<std::vector<wire::WireEntry>> list(const stdfs::path& dir) {
    std::error_code ec;
    stdfs::directory_iterator it(dir, ec);
    if (ec) return std::unexpected(io_text(ec));
    std::vector<wire::WireEntry> entries;
    for (const auto& entry : it) {
        // Names that aren't valid text couldn't be asked for again.
        auto name = entry.path().filename().string();
        std::error_code mec;
        auto status = stdfs::status(entry.path(), mec);
        if (mec) status = stdfs::symlink_status(entry.path(), mec);
        if (mec) return std::unexpected(io_text(mec));
        const bool is_dir = stdfs::is_directory(status);
        std::uint64_t size = 0;
        if (!is_dir) {
            auto s = stdfs::is_regular_file(status) ? stdfs::file_size(entry.path(), mec) : 0;
            size = mec ? 0 : s;
        }
        entries.push_back({std::move(name), is_dir, size});
    }
    std::ranges::sort(entries, [](const wire::WireEntry& a, const wire::WireEntry& b) {
        if (a.dir != b.dir) return a.dir;
        return lower(a.name) < lower(b.name);
    });
    return entries;
}

Checked<Bytes> read(const stdfs::path& path) {
    std::error_code ec;
    auto status = stdfs::status(path, ec);
    if (ec) return std::unexpected(io_text(ec));
    if (!stdfs::is_regular_file(status)) return std::unexpected("not a file");
    auto size = stdfs::file_size(path, ec);
    if (ec) return std::unexpected(io_text(ec));
    if (size > MAX_FILE) return std::unexpected(too_big("opened"));
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(io_text(std::error_code(errno, std::generic_category())));
    return Bytes(std::istreambuf_iterator<char>(in), {});
}

// The requester side: one op on a fresh stream; `on_reply` takes each reply until it says done.
Result<> request(NodeHost& host, const NodeId& peer, std::string_view op, std::string_view folder, std::string_view path,
                 std::span<const std::uint8_t> upload, const std::function<Result<bool>(wire::FolderReply)>& on_reply) {
    auto conn = host.dial(peer, wire::FOLDER);
    if (!conn) return std::unexpected(conn.error());
    auto result = [&]() -> Result<> {
        auto stream = (*conn)->open_stream();
        if (!stream) return std::unexpected(stream.error());
        wire::FolderRequest opening{std::string(op), std::string(folder), std::string(path), upload.size(), {}};
        if (auto r = wire::send(*stream->send, opening); !r) return r;
        if (auto r = host.grants().present(peer, *stream); !r) return r;
        auto ready = wire::expect<wire::FolderReply>(*stream->recv);
        if (!ready) return std::unexpected(ready.error());
        if (!ready->ready) return fail(NodeError::Kind::protocol, "expected the owner's go-ahead");
        for (std::size_t at = 0; at < upload.size(); at += CHUNK) {
            wire::FolderRequest chunk;
            auto part = upload.subspan(at, std::min(CHUNK, upload.size() - at));
            chunk.data.assign(part.begin(), part.end());
            if (auto r = wire::send(*stream->send, chunk); !r) return r;
        }
        while (true) {
            auto answer = wire::expect<wire::FolderReply>(*stream->recv);
            if (!answer) return std::unexpected(answer.error());
            auto finished = on_reply(std::move(*answer));
            if (!finished) return std::unexpected(finished.error());
            if (*finished) return {};
        }
    }();
    (*conn)->close(0, "done");
    return result;
}

} // namespace

Result<> serve(NodeHost& host, const Connection& conn, Stream stream) {
    auto received = wire::recv<wire::FolderRequest>(*stream.recv);
    if (!received) return std::unexpected(received.error());
    if (!*received) return fail(NodeError::Kind::protocol, "no request");
    auto request = std::move(**received);
    auto& grants = host.grants();
    if (auto r = grants.challenge(conn->remote(), stream); !r) return r;
    const auto peer = trust::to_bus(conn->remote());
    auto& tx = *stream.send;

    std::optional<Op> op;
    if (request.op == "list") op = Op::list;
    else if (request.op == "read") op = Op::read;
    else if (request.op == "write") op = Op::write;
    Checked<stdfs::path> target = std::unexpected(std::string("unknown folder request"));
    if (op == Op::write && request.size > MAX_FILE) target = std::unexpected(too_big("written"));
    else if (op) {
        const Action action = *op == Op::write ? Action::write : Action::read;
        auto access = grants.access;
        auto covered = [&](std::string_view path) {
            return access->GrantGate::allows(peer, trust::Request::folder(action, path), runtime::time::now_ms());
        };
        target = resolve(covered, *op == Op::write, request.folder, request.path);
    }
    if (!target) return wire::send(tx, reply(std::move(target.error())));
    if (auto r = wire::send(tx, wire::make_ack(std::type_identity<wire::FolderReply>{}, std::nullopt)); !r) return r;

    switch (*op) {
    case Op::list: {
        auto entries = list(*target);
        return wire::send(tx, entries ? done(std::move(*entries), {}) : reply(std::move(entries.error())));
    }
    case Op::read: {
        auto bytes = read(*target);
        if (!bytes) return wire::send(tx, reply(std::move(bytes.error())));
        if (bytes->empty()) return wire::send(tx, done({}, {}));
        for (std::size_t at = 0; at < bytes->size(); at += CHUNK) {
            wire::FolderReply chunk;
            const auto end = std::min(bytes->size(), at + CHUNK);
            chunk.data.assign(bytes->begin() + static_cast<std::ptrdiff_t>(at), bytes->begin() + static_cast<std::ptrdiff_t>(end));
            chunk.done = end == bytes->size();
            if (auto r = wire::send(tx, chunk); !r) return r;
        }
        return {};
    }
    case Op::write: {
        const auto size = static_cast<std::size_t>(request.size);
        std::string bytes;
        bytes.reserve(size);
        while (bytes.size() < size) {
            auto chunk = wire::recv<wire::FolderRequest>(*stream.recv);
            if (!chunk) return std::unexpected(chunk.error());
            const auto data = *chunk ? std::move((*chunk)->data) : Bytes{};
            if (data.empty() || bytes.size() + data.size() > size)
                return wire::send(tx, reply("the file's bytes don't match its size"));
            bytes.append(data.begin(), data.end());
        }
        auto written = runtime::fs::write_atomic(*target, bytes, std::nullopt);
        return wire::send(tx, written ? done({}, {}) : reply(written.error().message()));
    }
    }
    return {};
}

Result<std::vector<FolderEntry>> list_remote(NodeHost& host, const NodeId& peer, std::string_view folder,
                                             std::string_view path) {
    std::vector<FolderEntry> out;
    auto r = request(host, peer, "list", folder, path, {}, [&](wire::FolderReply reply) -> Result<bool> {
        for (auto& e : reply.entries) out.push_back({std::move(e.name), e.dir, e.size});
        return true;
    });
    if (!r) return std::unexpected(r.error());
    return out;
}

Result<Bytes> read_remote(NodeHost& host, const NodeId& peer, std::string_view folder, std::string_view path) {
    Bytes bytes;
    auto r = request(host, peer, "read", folder, path, {}, [&](wire::FolderReply reply) -> Result<bool> {
        bytes.insert(bytes.end(), reply.data.begin(), reply.data.end());
        if (bytes.size() > MAX_FILE) return fail(NodeError::Kind::protocol, "the file is larger than the owner said");
        return reply.done;
    });
    if (!r) return std::unexpected(r.error());
    return bytes;
}

Result<> write_remote(NodeHost& host, const NodeId& peer, std::string_view folder, std::string_view path,
                      std::span<const std::uint8_t> bytes) {
    if (bytes.size() > MAX_FILE) return fail(NodeError::Kind::refused, too_big("written"));
    return request(host, peer, "write", folder, path, bytes, [](wire::FolderReply) -> Result<bool> { return true; });
}

} // namespace loa::p2p::folder
