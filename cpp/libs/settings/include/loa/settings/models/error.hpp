#pragma once

// Errors of omp model settings, the `SettingsValidator` port and snapshot file helpers
// (port of `loa-settings` `models/error.rs`).

#include <cstdint>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/runtime/error.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/json.hpp"

namespace loa::settings::models {

// A failed RPC call: omp's `success: false` response or a transport failure.
struct RpcError {
    enum class Kind : std::uint8_t {
        Command,   // command, message, code
        Transport, // message
    };
    Kind kind = Kind::Transport;
    std::string command;
    std::string message;
    std::optional<std::string> code;

    [[nodiscard]] static RpcError command_failed(std::string command, std::string message, std::optional<std::string> code = {});
    [[nodiscard]] static RpcError transport(std::string message);

    // The Rust `Display` text.
    [[nodiscard]] std::string to_string() const;
    friend bool operator==(const RpcError&, const RpcError&) = default;
};

// Errors from reading, editing and validating model settings.
struct Error {
    enum class Kind : std::uint8_t {
        Io,            // path, source
        Parse,         // what, message: a document did not parse
        Unsupported,   // what, reason: a layout the in place editor does not edit
        EditCheck,     // path: a text edit did not produce the intended document
        ChangedOnDisk, // path
        Invalid,       // field, reason
        Rejected,      // what, message: omp itself rejected the edited file
        AlreadyExists, // path
        Process,       // program, message
        Rpc,           // rpc
    };

    Kind kind = Kind::Invalid;
    std::filesystem::path path;
    std::string what;
    std::string message;
    std::string reason;
    std::string field;
    std::string program;
    loa::Error source;
    RpcError rpc;

    [[nodiscard]] static Error io(std::filesystem::path path, loa::Error source);
    [[nodiscard]] static Error invalid(std::string field, std::string reason);
    [[nodiscard]] static Error parse(std::string what, std::string message);
    [[nodiscard]] static Error unsupported(std::string what, std::string reason);
    [[nodiscard]] static Error edit_check(std::string path);
    [[nodiscard]] static Error changed_on_disk(std::filesystem::path path);
    [[nodiscard]] static Error rejected(std::string what, std::string message);
    [[nodiscard]] static Error already_exists(std::filesystem::path path);
    [[nodiscard]] static Error process(std::string program, std::string message);
    [[nodiscard]] static Error from_rpc(RpcError rpc);

    // The Rust `Display` text.
    [[nodiscard]] std::string to_string() const;
    // As a `loa::Error` for callers that don't care about the kind.
    [[nodiscard]] loa::Error to_error() const;
};

template <typename T = void>
using Result = std::expected<T, Error>;

// Validates candidate omp files before atomic saves.
class SettingsValidator {
  public:
    virtual ~SettingsValidator() = default;
    // `expected`: each edited setting's dotted path and the JSON value omp must read back.
    [[nodiscard]] virtual contracts::Awaitable<Result<>> check_config(
        std::string text, std::vector<std::pair<std::string, runtime::json::Value>> expected) const = 0;
    [[nodiscard]] virtual contracts::Awaitable<Result<>> check_models(std::string text) const = 0;
};

// Reads `path` as UTF-8 text, none when it does not exist.
[[nodiscard]] Result<std::optional<std::string>> read_optional(const std::filesystem::path& path);

// Loads `path` and its text, remembering what was seen for `save_snapshot`.
[[nodiscard]] Result<std::pair<runtime::fs::Snapshot, std::optional<std::string>>> load_snapshot(const std::filesystem::path& path);

// Writes `text` atomically unless the file changed since it was loaded.
[[nodiscard]] Result<> save_snapshot(runtime::fs::Snapshot& snapshot, std::string_view text);

} // namespace loa::settings::models
