#pragma once

// `NativeCodec`: a harness's own session files, read into and written from the DOM, and
// found on disk.

#include <cstdint>
#include <filesystem>
#include <functional>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/sink.hpp"

namespace loa::harness::kit {

// One native session file.
struct NativeSession {
    std::string harness;
    std::filesystem::path path;
    // The harness's project folder name, when it groups sessions by one.
    std::optional<std::string> project;
    std::optional<std::string> title;
    // Unix milliseconds of the last change.
    std::int64_t modified = 0;
    std::uint64_t size = 0;

    // The session file at `path`, sized and dated from its metadata.
    [[nodiscard]] static std::optional<NativeSession> at(std::string_view harness, std::filesystem::path path,
                                                         std::optional<std::string> project,
                                                         std::optional<std::string> title);

    friend bool operator==(const NativeSession&, const NativeSession&) = default;
};

class NativeCodec {
  public:
    virtual ~NativeCodec() = default;
    // Reads the native session at `path` into the empty `sink`, keeping original timestamps.
    [[nodiscard]] virtual loa::contracts::Result<> import(const std::filesystem::path& path,
                                                          loa::core::PatchSink& sink) const = 0;
    // Writes `dom` as a native session that imports back to an equal DOM.
    [[nodiscard]] virtual loa::contracts::Result<> export_(const loa::core::Dom& dom,
                                                           const std::filesystem::path& out) const = 0;
    // The harness's sessions under the home directory `home`.
    [[nodiscard]] virtual std::vector<NativeSession> discover(const std::filesystem::path& home) const = 0;
};

// Files exactly `depth` folders below `dir` whose name passes `keep`.
[[nodiscard]] std::vector<std::filesystem::path> walk(const std::filesystem::path& dir, std::size_t depth,
                                                      const std::function<bool(std::string_view)>& keep);

// The name of the folder holding `path`.
[[nodiscard]] std::optional<std::string> parent_name(const std::filesystem::path& path);

} // namespace loa::harness::kit
