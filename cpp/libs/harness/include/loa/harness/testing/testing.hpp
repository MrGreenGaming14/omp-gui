#pragma once

// Test doubles for harness tests: a scripted stand-in process, a sink that can be awaited,
// the DOM convention checks, the transcript-replay and native round-trip runners, and a
// scripted harness for app tests (port of `testing/mod.rs`).

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <fstream>
#include <functional>
#include <iterator>
#include <mutex>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/sink.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/kit/native.hpp"
#include "loa/harness/testing/dom.hpp"
#include "loa/harness/testing/scripted.hpp"
#include "loa/runtime/error.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::testing {

// How long `WatchSink::until` waits.
inline constexpr std::chrono::seconds UNTIL_TIMEOUT{20};

// A `MemorySink` that announces every commit's seq.
class WatchSink final : public loa::core::PatchSink {
  public:
    loa::core::MemorySink inner;

    [[nodiscard]] std::expected<loa::core::Patch, loa::core::CommitError> commit_at(loa::core::Draft draft, std::int64_t ts) override;

    // The seq of the last commit (0 before any).
    [[nodiscard]] std::uint64_t seq() const;

    // Waits until `done` holds for the DOM; after `timeout` the error names `what` and holds
    // the DOM's XML (the Rust helper panics with that text).
    [[nodiscard]] std::expected<void, std::string> until(std::string_view what, const std::function<bool(const loa::core::Dom&)>& done,
                                                         std::chrono::milliseconds timeout = UNTIL_TIMEOUT) const;

  private:
    mutable std::mutex mutex_;
    mutable std::condition_variable changed_;
    std::uint64_t seq_ = 0;
};

// Feeds every line of `fixture` (one JSON event each, `#` comments and blank lines skipped)
// through `decode` to `mapper`, writing to `sink` at timestamp 0. Returns the replies in
// order, or the first failure prefixed by its line.
template <typename M, typename Decode>
[[nodiscard]] std::expected<std::vector<typename M::Reply>, std::string> replay(M& mapper, loa::core::PatchSink& sink,
                                                                               const std::filesystem::path& fixture, Decode&& decode) {
    std::ifstream in(fixture, std::ios::binary);
    if (!in) return std::unexpected(fixture.string() + ": cannot read the fixture");
    const loa::core::DomWriter w = loa::core::DomWriter(sink).at(0);
    std::vector<typename M::Reply> out;
    std::string raw;
    while (std::getline(in, raw)) {
        std::string_view line = raw;
        while (!line.empty() && std::string_view(" \t\r\n").contains(line.front())) line.remove_prefix(1);
        while (!line.empty() && std::string_view(" \t\r\n").contains(line.back())) line.remove_suffix(1);
        if (line.empty() || line.starts_with('#')) continue;
        auto event = decode(line);
        if (!event) return std::unexpected(std::string(line) + ": " + event.error());
        auto replies = mapper.apply(std::move(*event), w);
        if (!replies) return std::unexpected(std::string(line) + ": " + replies.error().message());
        for (auto& reply : *replies) out.push_back(std::move(reply));
    }
    return out;
}

// Compares `actual` with the golden file at `path`; with `LOA_BLESS` set it rewrites the file
// first. The error says which file differs.
[[nodiscard]] std::expected<void, std::string> golden(const std::filesystem::path& path, std::string_view actual);

// The DOM as golden text: its XML without the seq header, timestamps masked.
[[nodiscard]] std::string render(const loa::core::Dom& dom);

// Imports `native`, exports it and imports the export again; both DOMs must render the same.
[[nodiscard]] std::expected<void, std::string> round_trip(const loa::harness::kit::NativeCodec& codec,
                                                          const std::filesystem::path& native);

// A scripted stand-in for a harness executable speaking line-delimited JSON on stdio, run by
// bash. Script lines:
// - `> {json}`: write the frame to stdout; `{{name}}` is replaced by an id captured earlier.
// - `< kind [name]`: read one stdin frame, fail (exit 3) unless its `type` or `method` is
//   `kind`, and capture its `id` as `name`.
// - `stderr text`, `exit code`; `# …` and blank lines are ignored.
//
// After the last line it reads stdin until EOF, then exits 0. It records its arguments and
// every stdin frame for assertions.
class FakeProcess {
  public:
    // Writes the fake into `dir` (which must exist) with `script` as its transcript.
    [[nodiscard]] static std::expected<FakeProcess, loa::Error> install(const std::filesystem::path& dir, std::string_view script);

    // Sets what `--version` prints.
    [[nodiscard]] std::expected<void, loa::Error> set_version(std::string_view version) const;

    [[nodiscard]] const std::filesystem::path& executable() const noexcept { return executable_; }
    // The arguments of the last run.
    [[nodiscard]] std::vector<std::string> args() const;
    // Every frame the last run read from stdin, parsed.
    [[nodiscard]] std::vector<loa::runtime::json::Value> received() const;

  private:
    FakeProcess(std::filesystem::path dir, std::filesystem::path executable)
        : dir_(std::move(dir)), executable_(std::move(executable)) {}
    std::filesystem::path dir_;
    std::filesystem::path executable_;
};

// The ready frame of omp 19 (protocol v1 and v2).
inline constexpr std::string_view READY_V2 =
    R"({"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1,2],"maxFrameBytes":1048576,"maxReassembledFrameBytes":67108864,"supportsBashExcludeFromContext":true})";

// Script lines for omp's startup with v2 negotiation, as the client performs it.
[[nodiscard]] std::string handshake_v2();

} // namespace loa::harness::testing
