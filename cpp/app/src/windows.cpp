#include "loa/app/windows.hpp"

#include <algorithm>
#include <fstream>
#include <sstream>
#include <tuple>

#include "loa/ui/kit/theme.hpp"

namespace loa::app {

namespace fs = std::filesystem;

std::string_view blurb(ConfigsTab tab) {
    switch (tab) {
    case ConfigsTab::Models: return "Which model and effort each role runs on, and where a role falls back when its model fails.";
    case ConfigsTab::Tools: return "Built-in tools, MCP servers and remote hosts the harness may call.";
    case ConfigsTab::Agents: return "Agent definitions omp can delegate to. Each is a Markdown file with front matter.";
    case ConfigsTab::Skills: return "Skills load when a task matches their description.";
    case ConfigsTab::Rules: return "Instruction files loaded into every session, in this order.";
    }
    return {};
}

std::string_view places(ConfigsTab tab) {
    switch (tab) {
    case ConfigsTab::Agents: return ".omp/agents/ or ~/.omp/agent/agents/";
    case ConfigsTab::Skills: return ".omp/skills/<name>/SKILL.md or ~/.omp/agent/skills/";
    default: return "RULES.md, AGENTS.md or ~/.omp/agent/RULES.md";
    }
}

std::string_view label(ConfigsTab tab) {
    switch (tab) {
    case ConfigsTab::Models: return "Model roles";
    case ConfigsTab::Agents: return "Agents";
    case ConfigsTab::Skills: return "Skills";
    case ConfigsTab::Tools: return "Tools & MCP";
    case ConfigsTab::Rules: return "Rules";
    }
    return {};
}

std::string_view group(ConfigsTab tab) {
    switch (tab) {
    case ConfigsTab::Models: return "Models";
    case ConfigsTab::Agents:
    case ConfigsTab::Skills: return "Delegation";
    case ConfigsTab::Tools:
    case ConfigsTab::Rules: return "Behavior";
    }
    return {};
}

std::string_view label(SettingsTab tab) {
    switch (tab) {
    case SettingsTab::General: return "General";
    case SettingsTab::Peers: return "Peers & sharing";
    case SettingsTab::Harnesses: return "Harnesses";
    case SettingsTab::Updates: return "Updates";
    }
    return {};
}

namespace {

std::optional<std::string> read_file(const fs::path& path) {
    std::error_code ec;
    if (!fs::is_regular_file(path, ec)) return std::nullopt;
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::nullopt;
    std::stringstream text;
    text << in.rdbuf();
    return text.str();
}

// The files in `dir` (or `<dir>/*/SKILL.md` with `skill`), named by stem or folder.
std::vector<std::pair<fs::path, std::string>> list(const fs::path& dir, bool skill) {
    std::vector<fs::path> files;
    std::error_code ec;
    for (fs::directory_iterator it(dir, ec), end; !ec && it != end; it.increment(ec)) {
        const auto path = it->path();
        if (skill) {
            auto file = path / "SKILL.md";
            if (fs::is_regular_file(file, ec)) files.push_back(std::move(file));
        } else if (path.extension() == ".md") {
            files.push_back(path);
        }
    }
    std::ranges::sort(files);
    std::vector<std::pair<fs::path, std::string>> out;
    for (auto& path : files) {
        auto name = skill ? path.parent_path().filename().string() : path.stem().string();
        out.emplace_back(std::move(path), std::move(name));
    }
    return out;
}

} // namespace

std::vector<DocFile> scan(ConfigsTab tab, const fs::path& project, const std::optional<fs::path>& home) {
    std::optional<fs::path> global;
    if (home) global = *home / ".omp/agent";
    std::vector<std::tuple<fs::path, std::string, std::string_view>> found;
    auto add = [&](std::vector<std::pair<fs::path, std::string>> files, std::string_view scope) {
        for (auto& [path, name] : files) found.emplace_back(std::move(path), std::move(name), scope);
    };
    switch (tab) {
    case ConfigsTab::Agents:
    case ConfigsTab::Skills: {
        const bool agents = tab == ConfigsTab::Agents;
        const auto local = agents ? ".omp/agents" : ".omp/skills";
        const auto shared = agents ? "agents" : "skills";
        add(list(project / local, !agents), "This project");
        if (global) add(list(*global / shared, !agents), "All projects");
        break;
    }
    case ConfigsTab::Rules: {
        if (global) add({{*global / "RULES.md", "RULES.md"}}, "All projects");
        std::vector<std::pair<fs::path, std::string>> files;
        for (const char* name : {"RULES.md", "AGENTS.md", "CLAUDE.md"}) files.emplace_back(project / name, name);
        add(std::move(files), "This project");
        break;
    }
    case ConfigsTab::Models:
    case ConfigsTab::Tools: break;
    }
    std::vector<DocFile> docs;
    for (auto& [path, name, scope] : found) {
        auto text = read_file(path);
        if (!text) continue;
        docs.push_back(DocFile{std::move(name), std::move(path), scope, std::move(*text)});
    }
    return docs;
}

std::pair<float, float> detached_size(const Detached& what) {
    using ui::kit::theme::LayoutRole;
    using ui::kit::theme::width;
    if (const auto* tab = std::get_if<detached::Tab>(&what); tab && !tab->id.starts_with("surface"))
        return {width(LayoutRole::DetachWidth), width(LayoutRole::DetachHeight)};
    return {width(LayoutRole::PopupWidth), width(LayoutRole::TallHeight)};
}

} // namespace loa::app
