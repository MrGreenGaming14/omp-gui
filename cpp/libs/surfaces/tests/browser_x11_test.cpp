// The X11 capture against an in-process X server that answers connection
// setup and `GetImage` from a shared framebuffer, speaking the real wire
// protocol. Port of `loa-surfaces/tests/x11.rs`.

#include <doctest/doctest.h>

#if defined(__linux__)

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <cstring>
#include <mutex>
#include <thread>
#include <tuple>
#include <vector>

#include "loa/runtime/tempfile.hpp"
#include "loa/surfaces/browser/x11.hpp"

using namespace loa::surfaces::browser;
namespace cs = loa::contracts::surface;

namespace {

constexpr std::size_t WIDTH = 8;
constexpr std::size_t HEIGHT = 6;

// BGRX framebuffer as an LSB-first 24 bit TrueColor server stores it.
struct Framebuffer {
    std::mutex mutex;
    std::vector<std::uint8_t> pixels = std::vector<std::uint8_t>(WIDTH * HEIGHT * 4);

    void set(std::size_t x, std::size_t y, std::array<std::uint8_t, 3> rgb) {
        std::lock_guard lock(mutex);
        const auto i = (y * WIDTH + x) * 4;
        pixels[i] = rgb[2];
        pixels[i + 1] = rgb[1];
        pixels[i + 2] = rgb[0];
        pixels[i + 3] = 0;
    }
};

struct Writer {
    std::vector<std::uint8_t> out;
    void u8(std::uint32_t v) { out.push_back(std::uint8_t(v)); }
    void u16(std::uint32_t v) {
        u8(v & 0xff);
        u8(v >> 8);
    }
    void u32(std::uint32_t v) {
        u16(v & 0xffff);
        u16(v >> 16);
    }
    void pad(std::size_t n) { out.insert(out.end(), n, 0); }
};

std::vector<std::uint8_t> setup() {
    Writer w;
    const std::string vendor = "fake";
    w.u8(1);
    w.pad(1);
    w.u16(11);
    w.u16(0);
    w.u16(0); // length, patched below
    w.u32(1);           // release
    w.u32(0x0020'0000); // resource id base
    w.u32(0x001f'ffff); // resource id mask
    w.u32(256);         // motion buffer
    w.u16(std::uint32_t(vendor.size()));
    w.u16(0xffff); // maximum request length
    w.u8(1);       // screens
    w.u8(1);       // formats
    w.u8(0);       // image byte order: LSB first
    w.u8(0);       // bitmap bit order
    w.u8(32);
    w.u8(32);
    w.u8(8);
    w.u8(255);
    w.pad(4);
    w.out.insert(w.out.end(), vendor.begin(), vendor.end());
    w.pad((4 - vendor.size() % 4) % 4);
    // Pixmap format: depth 24, 32 bits per pixel.
    w.u8(24);
    w.u8(32);
    w.u8(32);
    w.pad(5);
    // Screen.
    w.u32(0x100);
    w.u32(0x20);
    w.u32(0xffffff);
    w.u32(0);
    w.u32(0);
    w.u16(WIDTH);
    w.u16(HEIGHT);
    w.u16(300);
    w.u16(200);
    w.u16(1);
    w.u16(1);
    w.u32(0x21); // root visual
    w.u8(0);
    w.u8(0);
    w.u8(24); // root depth
    w.u8(1);  // allowed depths
    // Depth 24 with one visual.
    w.u8(24);
    w.pad(1);
    w.u16(1);
    w.pad(4);
    w.u32(0x21);
    w.u8(4); // TrueColor
    w.u8(8);
    w.u16(256);
    w.u32(0xff0000);
    w.u32(0xff00);
    w.u32(0xff);
    w.pad(4);
    const auto length = (w.out.size() - 8) / 4;
    w.out[6] = std::uint8_t(length & 0xff);
    w.out[7] = std::uint8_t(length >> 8);
    return w.out;
}

bool read_exact(int fd, std::uint8_t* buf, std::size_t n) {
    while (n > 0) {
        const auto r = ::read(fd, buf, n);
        if (r <= 0) return false;
        buf += r;
        n -= std::size_t(r);
    }
    return true;
}

void write_all(int fd, const std::vector<std::uint8_t>& data) {
    std::size_t off = 0;
    while (off < data.size()) {
        const auto r = ::write(fd, data.data() + off, data.size() - off);
        REQUIRE(r > 0);
        off += std::size_t(r);
    }
}

// Serves `fb` on the listening Unix socket until the client hangs up.
std::thread serve(int listener, Framebuffer& fb, std::vector<std::string>& failures) {
    return std::thread([listener, &fb, &failures] {
        const int stream = ::accept(listener, nullptr, nullptr);
        ::close(listener);
        if (stream < 0) return;
        auto le = [](const std::uint8_t* b, std::size_t i) { return std::size_t(b[i]) | std::size_t(b[i + 1]) << 8; };
        std::uint8_t head[12];
        if (!read_exact(stream, head, sizeof head)) return ::close(stream), void();
        if (head[0] != 'l') failures.push_back("client byte order");
        std::vector<std::uint8_t> auth((le(head, 6) + 3) / 4 * 4 + (le(head, 8) + 3) / 4 * 4);
        read_exact(stream, auth.data(), auth.size());
        write_all(stream, setup());
        for (std::uint16_t sequence = 1;; ++sequence) {
            std::uint8_t header[4];
            if (!read_exact(stream, header, 4)) break;
            std::vector<std::uint8_t> body(le(header, 2) * 4 - 4);
            read_exact(stream, body.data(), body.size());
            if (header[0] != 73) {
                failures.push_back("only GetImage is served");
                break;
            }
            const auto x = le(body.data(), 4), y = le(body.data(), 6), w = le(body.data(), 8), h = le(body.data(), 10);
            Writer reply;
            reply.u8(1);
            reply.u8(24);
            reply.u16(sequence);
            reply.u32(std::uint32_t(w * h)); // w * h * 4 bytes in 4 byte units
            reply.u32(0x21);
            reply.pad(20);
            {
                std::lock_guard lock(fb.mutex);
                for (std::size_t row = y; row < y + h; ++row) {
                    const auto* start = fb.pixels.data() + (row * WIDTH + x) * 4;
                    reply.out.insert(reply.out.end(), start, start + w * 4);
                }
            }
            write_all(stream, reply.out);
        }
        ::close(stream);
    });
}

} // namespace

TEST_CASE("x11 captures RGBA, then only the changed rows, then nothing") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto socket_path = dir->path() / "X7";
    Framebuffer fb;
    fb.set(0, 0, {10, 20, 30});
    const int listener = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    std::strncpy(addr.sun_path, socket_path.c_str(), sizeof addr.sun_path - 1);
    REQUIRE(::bind(listener, reinterpret_cast<const sockaddr*>(&addr), sizeof addr) == 0);
    REQUIRE(::listen(listener, 1) == 0);
    std::vector<std::string> failures;
    auto server = serve(listener, fb, failures);

    {
        auto capture = X11Capture::connect(socket_path);
        REQUIRE_MESSAGE(capture, (capture ? "" : capture.error().message()));
        CHECK(capture->screen_size() == std::pair<std::uint32_t, std::uint32_t>(8, 6));
        auto first = capture->grab(4, 3, false);
        REQUIRE(first);
        REQUIRE(*first);
        CHECK(std::tuple((*first)->keyframe, (*first)->region, (*first)->data.size()) ==
              std::tuple(true, cs::Rect{0, 0, 4, 3}, std::size_t(4 * 3 * 4)));
        CHECK(std::vector<std::uint8_t>((*first)->data.begin(), (*first)->data.begin() + 4) ==
              std::vector<std::uint8_t>{10, 20, 30, 255});
        auto same = capture->grab(4, 3, false);
        REQUIRE(same);
        CHECK_MESSAGE(!*same, "unchanged picture sends nothing");

        fb.set(2, 1, {255, 0, 0});
        auto band = capture->grab(4, 3, false);
        REQUIRE(band);
        REQUIRE(*band);
        CHECK(std::tuple((*band)->keyframe, (*band)->region) == std::tuple(false, cs::Rect{0, 1, 4, 1}));
        CHECK(std::vector<std::uint8_t>((*band)->data.begin() + 8, (*band)->data.begin() + 12) ==
              std::vector<std::uint8_t>{255, 0, 0, 255});

        auto key = capture->grab(4, 3, true);
        REQUIRE(key);
        REQUIRE(*key);
        CHECK(std::tuple((*key)->keyframe, (*key)->data.size()) == std::tuple(true, std::size_t(4 * 3 * 4)));
        auto clipped = capture->grab(100, 100, false);
        REQUIRE(clipped);
        REQUIRE(*clipped);
        CHECK(std::tuple((*clipped)->width, (*clipped)->height, (*clipped)->keyframe) == std::tuple(8U, 6U, true));
    }
    server.join();
    CHECK(failures.empty());
}

#endif
