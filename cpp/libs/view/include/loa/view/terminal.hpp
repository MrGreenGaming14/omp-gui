#pragma once

// The terminal tab's ports: PTY channels the app opens on remote hosts.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"

// Ports the terminal needs and the app implements.
namespace loa::view::terminal::port {

class Error {
  public:
    enum class Kind : std::uint8_t { Io, Spawn, NotADirectory, Remote, NoRemote, Closed };

    [[nodiscard]] static Error io(std::string message);
    [[nodiscard]] static Error spawn(std::string message);
    [[nodiscard]] static Error not_a_directory(const std::filesystem::path& dir);
    [[nodiscard]] static Error remote(std::string host, std::string message);
    [[nodiscard]] static Error no_remote();
    [[nodiscard]] static Error closed();

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The host of `Remote`, empty otherwise.
    [[nodiscard]] const std::string& host() const noexcept { return host_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;

    friend bool operator==(const Error&, const Error&) = default;

  private:
    Error(Kind kind, std::string detail, std::string host = {})
        : kind_(kind), detail_(std::move(detail)), host_(std::move(host)) {}

    Kind kind_;
    std::string detail_;
    std::string host_;
};

template <typename T = void>
using Result = std::expected<T, Error>;

// A terminal grid in cells, plus one cell's pixel size (0 when unknown) for programs that ask.
struct GridSize {
    std::uint16_t cols = 80;
    std::uint16_t rows = 24;
    std::uint16_t cell_width = 0;
    std::uint16_t cell_height = 0;

    friend bool operator==(const GridSize&, const GridSize&) = default;
};

// Output, or the program's end; `exit_code` is `std::nullopt` when it was killed or the transport didn't say.
namespace channel_read {
struct Data {
    std::vector<std::uint8_t> bytes;
    friend bool operator==(const Data&, const Data&) = default;
};
struct Closed {
    std::optional<std::uint32_t> exit_code;
    friend bool operator==(const Closed&, const Closed&) = default;
};
} // namespace channel_read

using ChannelRead = std::variant<channel_read::Data, channel_read::Closed>;

// Read half of a PTY channel; not called again after `Closed` or an error.
class ChannelReader {
  public:
    virtual ~ChannelReader() = default;
    [[nodiscard]] virtual loa::contracts::Awaitable<Result<ChannelRead>> read() = 0;
};

// Write half of a PTY channel. Destroying it must hang up like `close`.
class ChannelWriter {
  public:
    virtual ~ChannelWriter() = default;
    [[nodiscard]] virtual loa::contracts::Awaitable<Result<>> write(std::vector<std::uint8_t> bytes) = 0;
    [[nodiscard]] virtual loa::contracts::Awaitable<Result<>> resize(GridSize size) = 0;
    [[nodiscard]] virtual loa::contracts::Awaitable<Result<>> close() = 0;
};

// An open PTY, split so output is read while input is written.
struct PtyChannel {
    std::unique_ptr<ChannelReader> reader;
    std::unique_ptr<ChannelWriter> writer;
};

// An interactive login shell on `host`, a name from the Remote hosts section.
class RemotePty {
  public:
    virtual ~RemotePty() = default;
    [[nodiscard]] virtual loa::contracts::Awaitable<Result<PtyChannel>> open_pty(std::string host, GridSize size) const = 0;
};

} // namespace loa::view::terminal::port
