#pragma once

// Interactive login shells for the terminal pane: a PTY channel on the host's pooled connection,
// split so output is read while input is written. The user opens these, so limits don't apply.

#include <chrono>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <string_view>

#include "loa/remote/error.hpp"

namespace loa::remote {

// Terminal type announced to the remote side.
inline constexpr std::string_view TERM = "xterm-256color";

// One read from a shell channel.
struct ShellRead {
    enum class Kind {
        // Output (stdout and stderr, as a PTY merges them).
        data,
        // The shell ended; every later read returns this again.
        closed,
    };
    Kind kind = Kind::data;
    std::string data;
    std::optional<std::uint32_t> exit_status;

    [[nodiscard]] static ShellRead output(std::string bytes) { return {Kind::data, std::move(bytes), std::nullopt}; }
    [[nodiscard]] static ShellRead closed_with(std::optional<std::uint32_t> status) { return {Kind::closed, {}, status}; }
    friend bool operator==(const ShellRead&, const ShellRead&) = default;
};

namespace detail {
struct ShellChannel;
}

// Output side of an interactive shell.
class ShellReader {
  public:
    explicit ShellReader(std::shared_ptr<detail::ShellChannel> channel) : channel_(std::move(channel)) {}
    [[nodiscard]] const std::string& host() const noexcept;
    // The next chunk of output, or `closed`; blocks.
    [[nodiscard]] ShellRead read();
    // As `read`, giving up (none) after `timeout`.
    [[nodiscard]] std::optional<ShellRead> read_for(std::chrono::milliseconds timeout);

  private:
    std::shared_ptr<detail::ShellChannel> channel_;
};

// Input and control side of an interactive shell; destroying it hangs up.
class ShellWriter {
  public:
    explicit ShellWriter(std::shared_ptr<detail::ShellChannel> channel) : channel_(std::move(channel)) {}
    ShellWriter(ShellWriter&&) noexcept = default;
    ShellWriter& operator=(ShellWriter&&) noexcept = default;
    ~ShellWriter();
    [[nodiscard]] const std::string& host() const noexcept;
    // Sends keyboard input.
    [[nodiscard]] Result<void> write(std::string_view bytes);
    // Tells the remote PTY its new size.
    [[nodiscard]] Result<void> resize(std::uint16_t cols, std::uint16_t rows);
    // Hangs up: sends EOF and closes the channel.
    [[nodiscard]] Result<void> close();

  private:
    std::shared_ptr<detail::ShellChannel> channel_;
};

// An open interactive shell.
struct Shell {
    ShellReader reader;
    ShellWriter writer;
};

} // namespace loa::remote
