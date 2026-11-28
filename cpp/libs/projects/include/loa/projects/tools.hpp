#pragma once

// `project_search` and `project_read` as tools, served to every harness through a `ToolSet` (the
// `HostTools` port). The project is the calling session's.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/tools.hpp"
#include "loa/projects/search.hpp"
#include "loa/projects/source.hpp"

namespace loa::projects {

inline constexpr std::string_view PROJECT_SEARCH = "project_search";
inline constexpr std::string_view PROJECT_READ = "project_read";

// Search arguments.
struct SearchArgs {
    std::optional<std::string> query;
    TextMode mode = TextMode::Phrase;
    std::optional<std::string> element;
    std::map<std::string, std::optional<std::string>> attributes;
    std::optional<std::vector<std::string>> sessions;
    bool include_excluded = false;
    std::optional<std::size_t> limit;
    std::optional<std::string> cursor;

    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<SearchArgs, std::string> from_json(const contracts::Json& json);
};

// Read arguments.
struct ReadArgs {
    std::string session;
    std::optional<std::uint64_t> node;
    std::size_t offset = 0;
    std::optional<std::size_t> max_bytes;

    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<ReadArgs, std::string> from_json(const contracts::Json& json);
};

// Both tools over `source`, showing what `access` allows.
[[nodiscard]] std::shared_ptr<contracts::ToolSet> tool_set(std::shared_ptr<const SessionSource> source,
                                                           std::shared_ptr<const Access> access);

} // namespace loa::projects
