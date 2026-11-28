#include "loa/surfaces/browser/xvfb.hpp"

#if defined(__linux__)

#include <asio/awaitable.hpp>

#include <unistd.h>

#include <charconv>

#include "browser/process.hpp"

namespace loa::surfaces::browser {

namespace {

// How long Xvfb may take to report its display number.
constexpr auto START_TIMEOUT = std::chrono::seconds(15);

std::string trim(std::string s) {
    const auto b = s.find_first_not_of(" \t\r\n");
    if (b == std::string::npos) return {};
    return s.substr(b, s.find_last_not_of(" \t\r\n") - b + 1);
}

} // namespace

VirtualDisplay::VirtualDisplay(std::unique_ptr<detail::Process> child, std::uint32_t number, std::filesystem::path socket)
    : child_(std::move(child)), number_(number), socket_(std::move(socket)) {}
VirtualDisplay::VirtualDisplay(VirtualDisplay&&) noexcept = default;
VirtualDisplay& VirtualDisplay::operator=(VirtualDisplay&&) noexcept = default;
VirtualDisplay::~VirtualDisplay() = default;

loa::contracts::Awaitable<Result<VirtualDisplay>> VirtualDisplay::start(std::filesystem::path xvfb,
                                                                       std::filesystem::path socket_dir,
                                                                       std::uint32_t width, std::uint32_t height) {
    auto spawned = detail::Process::spawn(
        xvfb,
        {"-displayfd", "1", "-screen", "0", std::to_string(width) + "x" + std::to_string(height) + "x24", "-nolisten",
         "tcp", "-nocursor", "-noreset"},
        {}, true);
    if (!spawned) co_return std::unexpected(BrowserError::display("cannot run " + xvfb.string() + ": " + spawned.error().detail()));
    auto child = std::make_unique<detail::Process>(std::move(*spawned));
    const int out = child->take_stdout();
    int err = child->take_stderr();
    auto line = co_await detail::read_line(out, std::chrono::steady_clock::now() + START_TIMEOUT);
    ::close(out);
    std::optional<std::uint32_t> number;
    if (line) {
        const auto text = trim(*line);
        std::uint32_t n = 0;
        const auto [end, ec] = std::from_chars(text.data(), text.data() + text.size(), n);
        if (!text.empty() && ec == std::errc{} && end == text.data() + text.size()) number = n;
    }
    if (!number) {
        child->kill();
        (void)co_await child->wait(std::chrono::seconds(5));
        std::string log;
        char buf[4096];
        ssize_t n = 0;
        while ((n = ::read(err, buf, sizeof buf)) > 0) log.append(buf, std::size_t(n));
        ::close(err);
        co_return std::unexpected(BrowserError::display("Xvfb did not report a display: " + trim(log)));
    }
    // Keep draining stderr so a chatty server never blocks on a full pipe.
    detail::drain_lines(err, "xvfb");
    co_return VirtualDisplay(std::move(child), *number, socket_dir / ("X" + std::to_string(*number)));
}

loa::contracts::Awaitable<Result<void>> VirtualDisplay::stop() {
    if (child_) {
        child_->kill();
        (void)co_await child_->wait(std::chrono::seconds(10));
    }
    co_return Result<void>{};
}

} // namespace loa::surfaces::browser

#endif
