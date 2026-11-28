#pragma once

// Internal: an SFTP v3 client over a libssh session channel, speaking the raw protocol so
// `posix-rename@openssh.com` replaces are available.

#include <libssh/libssh.h>

#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "loa/remote/error.hpp"
#include "loa/remote/service.hpp"

namespace loa::remote::detail {

struct Link;

// SFTP file attributes; absent fields are `nullopt`.
struct FileAttributes {
    std::optional<std::uint64_t> size;
    std::optional<std::uint32_t> uid;
    std::optional<std::uint32_t> gid;
    std::optional<std::uint32_t> permissions;
    std::optional<std::uint32_t> atime;
    std::optional<std::uint32_t> mtime;

    [[nodiscard]] bool is_symlink() const noexcept;
    [[nodiscard]] bool is_dir() const noexcept;
    [[nodiscard]] bool is_regular() const noexcept;
};

// Whether the error is the server answering (the channel still works).
[[nodiscard]] bool is_server_status(const Error& e) noexcept;

class Sftp {
  public:
    [[nodiscard]] static Result<std::shared_ptr<Sftp>> open(std::shared_ptr<Link> link, const std::string& host);
    ~Sftp();
    Sftp(const Sftp&) = delete;
    Sftp& operator=(const Sftp&) = delete;

    // The server's canonical absolute path, symlinks resolved.
    [[nodiscard]] Result<std::string> realpath(const std::string& path);
    // `stat` (follow links) or `lstat`, with "no such file" as `nullopt`.
    [[nodiscard]] Result<std::optional<FileAttributes>> stat(const std::string& path, bool follow);
    // Reads a whole file, refusing files over `max` bytes.
    [[nodiscard]] Result<std::string> read(const std::string& path, std::uint64_t max);
    // Lists a directory, sorted, without `.` and `..`.
    [[nodiscard]] Result<std::vector<std::pair<std::string, FileAttributes>>> list(const std::string& path);
    // Replaces `path` with `data`, atomically when the server allows, keeping permission bits.
    [[nodiscard]] Result<WriteMode> write(const std::string& path, std::string_view data);

    struct Reply;

  private:
    Sftp(std::shared_ptr<Link> link, ssh_channel channel, std::string host) : link_(std::move(link)), channel_(channel), host_(std::move(host)) {}

    Result<void> send(const std::string& packet);
    Result<std::string> receive();
    // One request (type, body after the id) and its reply.
    Result<Reply> request(std::uint8_t type, const std::string& body);

    std::shared_ptr<Link> link_;
    ssh_channel channel_;
    std::string host_;
    bool posix_rename_ = false;
    std::mutex mu_;
    std::uint32_t next_id_ = 0;
    std::string inbox_;
};

} // namespace loa::remote::detail
