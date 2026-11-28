#pragma once

// Wire protocol between the host and the exec stub inside a sandbox (Rust `loa_sandbox::proto`).
//
// The stub first writes `MAGIC`, which lets the host find the protocol in a serial console that
// also carries boot messages. After it, both directions carry frames: a little endian u32 body
// length, then a postcard encoded `Request` or `Response`.

#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/runtime/error.hpp"

namespace loa::sandbox {
class ByteReader;
class ByteWriter;
} // namespace loa::sandbox

namespace loa::sandbox::proto {

// Raw bytes (Rust `Vec<u8>`).
using Bytes = std::string;

// Protocol version the stub announces in `Response::Ready`.
inline constexpr std::uint32_t VERSION = 2;
// Written by the stub before its first frame.
inline constexpr std::string_view MAGIC{"\0LOA-STUB/2\n", 12};
// Largest frame body either side accepts.
inline constexpr std::uint32_t MAX_FRAME = 64u << 20;

// A command for the stub: `argv[0]` with the rest as its arguments.
struct ExecRequest {
    std::vector<std::string> argv;
    // Working directory, relative to the stub root. None is the root.
    std::optional<std::string> cwd;
    // Added to the stub's own environment.
    std::vector<std::pair<std::string, std::string>> env;
    // Written to the command's stdin, which is then closed; none is an empty stdin.
    std::optional<Bytes> stdin_data;
    // Kills the command and its process group after this many milliseconds.
    std::optional<std::uint64_t> timeout_ms;

    // `/bin/sh -c script sh args…`: the script sees `args` as `$1`, `$2`, ….
    [[nodiscard]] static ExecRequest shell(std::string_view script, const std::vector<std::string>& args = {});

    friend bool operator==(const ExecRequest&, const ExecRequest&) = default;
};

namespace request {
// Answered by `Stdout`/`Stderr` chunks, then `Exit` (or `Error` if it can't start).
struct Exec {
    std::uint32_t id = 0;
    ExecRequest exec;
    friend bool operator==(const Exec&, const Exec&) = default;
};
// Kills every running exec and ends the stub.
struct Shutdown {
    friend bool operator==(const Shutdown&, const Shutdown&) = default;
};
} // namespace request
using Request = std::variant<request::Exec, request::Shutdown>;

struct ExitStatus {
    std::optional<std::int32_t> code;
    // Signal that ended the command, when it didn't exit.
    std::optional<std::int32_t> signal;
    // The command ran past its timeout and was killed.
    bool timed_out = false;

    [[nodiscard]] bool success() const noexcept { return code == 0; }
    friend bool operator==(const ExitStatus&, const ExitStatus&) = default;
};

namespace response {
// First frame after `MAGIC`.
struct Ready {
    std::uint32_t version = 0;
    friend bool operator==(const Ready&, const Ready&) = default;
};
struct Stdout {
    std::uint32_t id = 0;
    Bytes data;
    friend bool operator==(const Stdout&, const Stdout&) = default;
};
struct Stderr {
    std::uint32_t id = 0;
    Bytes data;
    friend bool operator==(const Stderr&, const Stderr&) = default;
};
struct Exit {
    std::uint32_t id = 0;
    ExitStatus status;
    friend bool operator==(const Exit&, const Exit&) = default;
};
struct Error {
    std::uint32_t id = 0;
    std::string message;
    friend bool operator==(const Error&, const Error&) = default;
};
} // namespace response
using Response = std::variant<response::Ready, response::Stdout, response::Stderr, response::Exit, response::Error>;

// The request a response answers; none for `Ready`.
[[nodiscard]] std::optional<std::uint32_t> response_id(const Response& response) noexcept;
// Rust `{:?}` of a response, for error messages.
[[nodiscard]] std::string debug(const Response& response);

// The postcard body alone.
[[nodiscard]] Bytes encode(const Request& request);
[[nodiscard]] Bytes encode(const Response& response);
// One frame: the body length (u32 LE), then the postcard body.
[[nodiscard]] Bytes frame(const Request& request);
[[nodiscard]] Bytes frame(const Response& response);

// Decode one frame body; errors read "sandbox protocol: …" (`ErrorCode::parse`).
[[nodiscard]] std::expected<Request, loa::Error> decode_request(std::string_view body);
[[nodiscard]] std::expected<Response, loa::Error> decode_response(std::string_view body);

// Reads one frame body into `buf`. False on a clean end of stream before the frame starts.
[[nodiscard]] std::expected<bool, loa::Error> read_frame(ByteReader& reader, Bytes& buf);
// Consumes bytes until just after `MAGIC`, discarding boot messages before it. False if the
// stream ends first.
[[nodiscard]] std::expected<bool, loa::Error> skip_to_magic(ByteReader& reader);
// Writes `MAGIC` and the `Ready` frame: the stub's first output.
[[nodiscard]] std::expected<void, loa::Error> write_hello(ByteWriter& writer);

} // namespace loa::sandbox::proto
