#pragma once

// The client methods ACP agents call: `fs/*` and `terminal/*`, confined to the session cwd
// (port of `acp/host.rs`). Terminals run under a pseudo-terminal (forkpty on POSIX, ConPTY
// in a job object on Windows); their output is kept in a `TailBuffer`.

#include <atomic>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>

#include "loa/harness/kit/rpc.hpp"
#include "loa/runtime/error.hpp"

namespace loa::harness::acp {

// One session's files and terminals.
class Host {
  public:
    struct Terminal;

    // A host for `cwd`; fails when it can't be canonicalized.
    [[nodiscard]] static std::expected<std::shared_ptr<Host>, loa::Error> create(const std::filesystem::path& cwd);
    // Kills every command.
    ~Host();
    Host(const Host&) = delete;
    Host& operator=(const Host&) = delete;

    // The session cwd, canonical.
    [[nodiscard]] const std::filesystem::path& root() const noexcept { return root_; }

    // `path` (absolute, or relative to the root) inside the root; `..`, and symlinks that lead
    // out, are refused (`ErrorCode::permission_denied`).
    [[nodiscard]] std::expected<std::filesystem::path, loa::Error> resolve(std::string_view path) const;

    // Serves one request of `method`.
    [[nodiscard]] kit::Awaitable<kit::RpcResult<kit::Json>> serve(std::string method, kit::Json request) const;

    // Kills every command; the session is gone.
    void release_all() const;

  private:
    explicit Host(std::filesystem::path root) : root_(std::move(root)) {}
    [[nodiscard]] kit::RpcResult<kit::Json> create_terminal(const kit::Json& request) const;
    [[nodiscard]] kit::RpcResult<std::shared_ptr<Terminal>> terminal(const kit::Json& request) const;

    std::filesystem::path root_;
    mutable std::mutex mutex_;
    mutable std::map<std::string, std::shared_ptr<Terminal>> terminals_;
    mutable std::atomic<std::uint64_t> next_{1};
};

} // namespace loa::harness::acp
