#pragma once

// Launcher for an Xvfb virtual display. Port of
// `loa-surfaces/src/browser/xvfb.rs`. Linux only.

#if defined(__linux__)

#include <cstdint>
#include <filesystem>
#include <memory>
#include <string>

#include "loa/contracts/async.hpp"
#include "loa/surfaces/browser/error.hpp"

namespace loa::surfaces::browser {

namespace detail {
class Process;
}

// A running Xvfb server. Destroying it kills the server.
class VirtualDisplay {
  public:
    // Starts `xvfb` with one `width` x `height` 24 bit screen. Xvfb picks a
    // free display number and reports it on stdout (`-displayfd 1`) once it
    // accepts connections; its socket is `socket_dir/X<n>`.
    [[nodiscard]] static loa::contracts::Awaitable<Result<VirtualDisplay>>
    start(std::filesystem::path xvfb, std::filesystem::path socket_dir, std::uint32_t width, std::uint32_t height);

    VirtualDisplay(VirtualDisplay&&) noexcept;
    VirtualDisplay& operator=(VirtualDisplay&&) noexcept;
    ~VirtualDisplay();

    // Value for `DISPLAY`, e.g. `:99`.
    [[nodiscard]] std::string display() const { return ":" + std::to_string(number_); }
    [[nodiscard]] const std::filesystem::path& socket() const noexcept { return socket_; }

    // Stops the server and waits for it to exit.
    [[nodiscard]] loa::contracts::Awaitable<Result<void>> stop();

  private:
    VirtualDisplay(std::unique_ptr<detail::Process> child, std::uint32_t number, std::filesystem::path socket);

    std::unique_ptr<detail::Process> child_;
    std::uint32_t number_ = 0;
    std::filesystem::path socket_;
};

} // namespace loa::surfaces::browser

#endif
