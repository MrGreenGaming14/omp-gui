#pragma once

// `RemoteService`: host lookup, limits, approval, the SSH work, and one audit row per call
// whatever the outcome.
//
// Calls block the calling thread (libssh is blocking); `tools.hpp` runs them off the executor.

#include <cstdint>
#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <tuple>
#include <variant>
#include <vector>

#include "loa/remote/audit.hpp"
#include "loa/remote/cancel.hpp"
#include "loa/remote/connection.hpp"
#include "loa/remote/error.hpp"
#include "loa/remote/exec.hpp"
#include "loa/remote/hosts.hpp"
#include "loa/remote/ports.hpp"
#include "loa/remote/pty.hpp"
#include "loa/runtime/json.hpp"

namespace loa::remote {

// Session id audit rows of user opened terminals carry.
inline constexpr std::string_view TERMINAL_SESSION = "terminal";

// A `remote_bash` call; `timeout_secs` is capped by the host's `max_timeout_secs`.
struct BashRequest {
    std::string host;
    std::string command;
    std::optional<std::string> cwd;
    std::optional<std::uint64_t> timeout_secs;
    friend bool operator==(const BashRequest&, const BashRequest&) = default;
};

// What `remote_file` should do.
struct ReadOp {
    friend bool operator==(const ReadOp&, const ReadOp&) = default;
};
struct ListOp {
    friend bool operator==(const ListOp&, const ListOp&) = default;
};
struct WriteOp {
    std::string content;
    friend bool operator==(const WriteOp&, const WriteOp&) = default;
};
// Replace the one occurrence of `old_text` with `new_text`.
struct EditOp {
    std::string old_text;
    std::string new_text;
    friend bool operator==(const EditOp&, const EditOp&) = default;
};
using FileOp = std::variant<ReadOp, ListOp, WriteOp, EditOp>;

// A `remote_file` call.
struct FileRequest {
    std::string host;
    std::string path;
    FileOp op;
    friend bool operator==(const FileRequest&, const FileRequest&) = default;
};

// Kind of a directory entry.
enum class EntryKind { file, dir, symlink, other };
[[nodiscard]] std::string_view to_string(EntryKind kind) noexcept;

// One entry of a listing.
struct DirEntry {
    std::string name;
    EntryKind kind = EntryKind::other;
    std::optional<std::uint64_t> size;
    std::optional<std::uint32_t> mtime;
    std::optional<std::uint32_t> permissions;
    friend bool operator==(const DirEntry&, const DirEntry&) = default;
};

// How a write reached the file.
enum class WriteMode {
    // Written to a temp file in the same directory, then renamed over the target.
    atomic,
    // The server refused the temp file or the rename; the target was rewritten.
    in_place,
};
[[nodiscard]] std::string_view to_string(WriteMode mode) noexcept;

// Result of a `remote_file` call; `path` is the real path acted on.
struct ReadOutput {
    std::string path;
    // `utf8` or `base64`.
    std::string encoding;
    std::string content;
    std::uint64_t size = 0;
    friend bool operator==(const ReadOutput&, const ReadOutput&) = default;
};
struct ListOutput {
    std::string path;
    std::vector<DirEntry> entries;
    friend bool operator==(const ListOutput&, const ListOutput&) = default;
};
struct WriteOutput {
    std::string path;
    std::uint64_t bytes = 0;
    WriteMode mode = WriteMode::atomic;
    friend bool operator==(const WriteOutput&, const WriteOutput&) = default;
};
struct EditOutput {
    std::string path;
    std::uint64_t bytes = 0;
    WriteMode mode = WriteMode::atomic;
    friend bool operator==(const EditOutput&, const EditOutput&) = default;
};
using FileOutput = std::variant<ReadOutput, ListOutput, WriteOutput, EditOutput>;
// The serde form, tagged by `op`.
[[nodiscard]] loa::runtime::json::Value to_json(const FileOutput& output);

// Replaces the one occurrence of `old_text` in `text` with `new_text`.
[[nodiscard]] std::expected<std::string, std::string> apply_edit(std::string_view text, std::string_view old_text, std::string_view new_text);

// Paths and settings the app passes in.
struct RemoteServiceConfig {
    std::vector<std::filesystem::path> known_hosts;
    // File accepted host keys are appended to.
    std::filesystem::path known_hosts_write;
    ConnectorConfig connector;

    // `~/.ssh/known_hosts`, `~/.ssh/known_hosts2`, the system `ssh_known_hosts` and the SSH agent.
    [[nodiscard]] static RemoteServiceConfig from_home(const std::filesystem::path& home);
};

// The OpenSSH system `ssh_known_hosts`: under `%ProgramData%\ssh` on Windows, else `/etc/ssh`.
[[nodiscard]] std::filesystem::path system_known_hosts(std::optional<std::string> program_data);

// The `remote` server's logic, shared by every session. Thread-safe.
class RemoteService {
  public:
    RemoteService(std::shared_ptr<HostRegistry> hosts, std::shared_ptr<AuditSink> audit, std::shared_ptr<HostKeyDecider> decider,
                  std::shared_ptr<Approver> approver, RemoteServiceConfig config);

    [[nodiscard]] const std::shared_ptr<HostRegistry>& hosts() const noexcept { return hosts_; }
    [[nodiscard]] const std::shared_ptr<AuditSink>& audit() const noexcept { return audit_; }

    // Closes the pooled connection to a host.
    void disconnect(const std::string& host);
    // Forgets "allow for session" answers given in `session_id`.
    void end_session(std::string_view session_id);

    // Runs a command on a host (`remote_bash`).
    [[nodiscard]] Result<ExecOutput> bash(std::string_view session_id, const BashRequest& request, const CancelToken& cancel);
    // Opens an interactive login shell for the terminal pane; host keys are checked, limits and
    // approvals don't apply (the user opened it). Audited under `TERMINAL_SESSION`.
    [[nodiscard]] Result<Shell> open_shell(const std::string& host, std::uint16_t cols, std::uint16_t rows);
    // Reads, lists, writes or edits a file on a host (`remote_file`).
    [[nodiscard]] Result<FileOutput> file(std::string_view session_id, const FileRequest& request, const CancelToken& cancel);

  private:
    // Records one audit row for `result` (its outcome and detail) and passes it through.
    template <typename T>
    Result<T> audited(std::string_view session, std::string_view host, Action action, std::string_view target, Result<T> result);
    Result<Host> host(const std::string& name) const;
    Result<void> approve(std::string_view session_id, const Host& host, Action action, std::string_view target, std::string detail,
                         const CancelToken& cancel);
    Result<ExecOutput> bash_inner(std::string_view session_id, const BashRequest& request, const std::string& command,
                                  const CancelToken& cancel);
    Result<FileOutput> file_inner(std::string_view session_id, const FileRequest& request, Action action, std::string& target,
                                  const CancelToken& cancel);

    std::shared_ptr<HostRegistry> hosts_;
    std::shared_ptr<AuditSink> audit_;
    std::shared_ptr<Approver> approver_;
    std::unique_ptr<Connector> connector_;
    // "Allow for session" answers: (session, host, commands?).
    std::mutex grants_mu_;
    std::set<std::tuple<std::string, std::string, bool>> grants_;
};

} // namespace loa::remote
