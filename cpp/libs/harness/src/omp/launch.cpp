#include "loa/harness/omp/launch.hpp"

#include <algorithm>
#include <cctype>
#include <filesystem>
#include <string>
#include <string_view>
#include <system_error>
#include <vector>

#include "loa/harness/omp/oneshot.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::harness::omp {

namespace {

using loa::contracts::AdapterError;
namespace fs = std::filesystem;

[[nodiscard]] loa::contracts::Result<fs::path> write_state_file(const fs::path& dir, std::string_view name,
                                                                  std::string_view content) {
    std::error_code ec;
    fs::create_directories(dir, ec);
    if (ec) return std::unexpected(AdapterError::Io{loa::Error(loa::ErrorCode::io, ec.message())});
    const auto path = dir / std::string(name);
    // Do not replace a file another omp process may be reading when its bytes already match.
    auto existing = loa::runtime::fs::read_optional(path);
    if (existing && existing->has_value() && **existing == content) return path;
    auto written = loa::runtime::fs::write_atomic(path, content);
    if (!written) return std::unexpected(AdapterError::Io{written.error()});
    return path;
}

[[nodiscard]] std::string file_safe(std::string_view id) {
    std::string safe;
    safe.reserve(id.size());
    for (unsigned char c : id) {
        if (c >= 0x80) {
            // UTF-8 continuation bytes belong to the same non-ASCII character.
            if (c < 0xC0) continue;
            safe += '_';
        } else {
            safe += (std::isalnum(c) || c == '-') ? static_cast<char>(c) : '_';
        }
    }
    return safe.empty() ? "session" : safe;
}

[[nodiscard]] bool blank(std::string_view text) {
    return std::all_of(text.begin(), text.end(), [](unsigned char c) { return std::isspace(c); });
}

[[nodiscard]] std::string tools_arg(const std::vector<std::string_view>& tools) {
    if (tools.empty()) return "--no-tools";
    std::string arg = "--tools=";
    for (auto tool : tools) {
        if (arg.size() > std::string_view("--tools=").size()) arg += ',';
        arg += tool;
    }
    return arg;
}

[[nodiscard]] std::string_view trim(std::string_view text) {
    while (!text.empty() && std::isspace(static_cast<unsigned char>(text.front()))) text.remove_prefix(1);
    while (!text.empty() && std::isspace(static_cast<unsigned char>(text.back()))) text.remove_suffix(1);
    return text;
}

} // namespace

loa::contracts::Result<rpc::SpawnOptions> spawn_options(const OmpAdapterConfig& adapter,
                                                         const loa::contracts::SessionConfig& session,
                                                         const Target& target) {
    rpc::SpawnOptions opts(session.executable.value_or(adapter.executable));
    opts.mode = rpc::Mode::RpcUi;
    opts.ready_timeout = adapter.ready_timeout;
    opts.cwd = session.cwd;
    opts.env.assign(session.env.begin(), session.env.end());

    if (adapter.memory == MemoryMode::AppDigests) {
        auto overlay = write_state_file(adapter.state_dir, "memory-off.yml", MEMORY_OFF);
        if (!overlay) return std::unexpected(overlay.error());
        opts.arg("--config").arg(overlay->string());
    }

    const std::string* context = adapter.memory == MemoryMode::AppDigests && session.context && !blank(*session.context)
                                     ? &*session.context : nullptr;
    const std::string* instructions = session.instructions && !blank(*session.instructions)
                                          ? &*session.instructions : nullptr;
    if (context || instructions) {
        std::string prompt;
        if (context) prompt = *context;
        if (context && instructions) prompt += "\n\n";
        if (instructions) prompt += *instructions;
        auto file = write_state_file(adapter.state_dir, "context-" + file_safe(session.session_id) + ".md", prompt);
        if (!file) return std::unexpected(file.error());
        opts.arg("--append-system-prompt").arg(file->string());
    }
    if (adapter.session_dir) opts.arg("--session-dir").arg(adapter.session_dir->string());
    if (const auto& mode = session.approval_mode ? session.approval_mode : adapter.approval_mode; mode)
        opts.arg("--approval-mode").arg(*mode);
    if (session.model) opts.arg("--model").arg(*session.model);
    if (target.kind == Target::Kind::Resume) opts.arg("--resume").arg(target.id);
    if (!session.disabled_tools.empty()) {
        std::vector<std::string_view> tools;
        tools.reserve(std::size(DEFAULT_TOOLS));
        for (auto tool : DEFAULT_TOOLS) {
            if (std::ranges::find(session.disabled_tools, tool) == session.disabled_tools.end()) tools.push_back(tool);
        }
        opts.arg(tools_arg(tools));
    }
    return opts;
}

bool drop_unavailable_tools(rpc::SpawnOptions& opts, std::string_view stderr) {
    constexpr std::string_view marker = " unavailable in this session: ";
    std::string_view list;
    while (!stderr.empty()) {
        const auto end = stderr.find('\n');
        const auto line = stderr.substr(0, end);
        if (line.find("Built-in tool") != std::string_view::npos) {
            const auto at = line.find(marker);
            if (at != std::string_view::npos) {
                list = line.substr(at + marker.size());
                break;
            }
        }
        if (end == std::string_view::npos) break;
        stderr.remove_prefix(end + 1);
    }
    if (list.empty()) return false;
    while (!list.empty() && list.back() == '.') list.remove_suffix(1);
    std::vector<std::string_view> unavailable;
    while (!list.empty()) {
        const auto comma = list.find(',');
        auto tool = trim(list.substr(0, comma));
        if (!tool.empty()) unavailable.push_back(tool);
        if (comma == std::string_view::npos) break;
        list.remove_prefix(comma + 1);
    }
    for (auto& arg : opts.args) {
        constexpr std::string_view prefix = "--tools=";
        if (!arg.starts_with(prefix)) continue;
        std::vector<std::string_view> tools;
        std::string_view current(arg);
        current.remove_prefix(prefix.size());
        while (true) {
            const auto comma = current.find(',');
            const auto tool = current.substr(0, comma);
            if (std::ranges::find(unavailable, tool) == unavailable.end()) tools.push_back(tool);
            if (comma == std::string_view::npos) break;
            current.remove_prefix(comma + 1);
        }
        auto next = tools_arg(tools);
        if (next == arg) return false;
        arg = std::move(next);
        return true;
    }
    return false;
}

loa::contracts::Result<std::pair<std::string_view, std::string_view>> split_selector(std::string_view selector) {
    const auto slash = selector.find('/');
    if (slash == std::string_view::npos || slash == 0 || slash == selector.size() - 1)
        return std::unexpected(AdapterError::Protocol{"model `" + std::string(selector) + "` isn't a provider/model selector"});
    return std::pair{selector.substr(0, slash), selector.substr(slash + 1)};
}

} // namespace loa::harness::omp
