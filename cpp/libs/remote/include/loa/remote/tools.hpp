#pragma once

// `remote_bash` and `remote_file` as typed `Tool`s; `tool_set` serves them as host tools and,
// through `ToolSetMcp`, over MCP.

#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <string_view>

#include "loa/contracts/tools.hpp"
#include "loa/remote/service.hpp"

namespace loa::remote {

// MCP server name.
inline constexpr std::string_view SERVER_NAME = "remote";
inline constexpr std::string_view REMOTE_BASH = "remote_bash";
inline constexpr std::string_view REMOTE_FILE = "remote_file";

// Arguments of `remote_bash`.
struct BashArgs {
    // Host name from the Remote hosts list.
    std::string host;
    // Shell command, run by the remote user's login shell.
    std::string command;
    // Absolute directory to run the command in.
    std::optional<std::string> cwd;
    // Seconds before the command is stopped.
    std::optional<std::uint64_t> timeout_secs;

    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<BashArgs, std::string> from_json(const loa::contracts::Json& json);
};

// What `remote_file` does.
enum class FileOpName { read, write, list, edit };
// How `content` is encoded.
enum class Encoding { utf8, base64 };

// Arguments of `remote_file`.
struct FileArgs {
    // Host name from the Remote hosts list.
    std::string host;
    FileOpName op = FileOpName::read;
    // Absolute path on the host.
    std::string path;
    // `write`: the new file content.
    std::optional<std::string> content;
    // `write`: how `content` is encoded. Defaults to `utf8`.
    std::optional<Encoding> encoding;
    // `edit`: exact text to replace; must occur exactly once.
    std::optional<std::string> old_text;
    // `edit`: replacement text.
    std::optional<std::string> new_text;

    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<FileArgs, std::string> from_json(const loa::contracts::Json& json);
    // The service request; the `content`/`old`/`new` required errors and the base64 error.
    [[nodiscard]] std::expected<FileRequest, std::string> into_request() &&;
};

// `remote_bash`.
class RemoteBash final : public loa::contracts::Tool<BashArgs> {
  public:
    explicit RemoteBash(std::shared_ptr<RemoteService> service) : service_(std::move(service)) {}
    [[nodiscard]] std::string_view name() const override { return REMOTE_BASH; }
    [[nodiscard]] std::string_view description() const override;
    [[nodiscard]] loa::contracts::ToolAnnotations annotations() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::ToolResult> run(const loa::contracts::CallCtx& ctx, BashArgs args) const override;

  private:
    std::shared_ptr<RemoteService> service_;
};

// `remote_file`.
class RemoteFile final : public loa::contracts::Tool<FileArgs> {
  public:
    explicit RemoteFile(std::shared_ptr<RemoteService> service) : service_(std::move(service)) {}
    [[nodiscard]] std::string_view name() const override { return REMOTE_FILE; }
    [[nodiscard]] std::string_view description() const override;
    [[nodiscard]] loa::contracts::ToolAnnotations annotations() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::ToolResult> run(const loa::contracts::CallCtx& ctx, FileArgs args) const override;

  private:
    std::shared_ptr<RemoteService> service_;
};

// Both tools over `service`.
[[nodiscard]] loa::contracts::ToolSet tool_set(std::shared_ptr<RemoteService> service);

} // namespace loa::remote
