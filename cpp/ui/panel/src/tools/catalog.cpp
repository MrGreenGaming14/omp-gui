// What the tools page shows after grouping, filtering and toggles. Port of
// `crates/loa-ui-panel/src/tools/catalog.rs`.

#include <algorithm>
#include <array>

#include "loa/ui/panel/tools.hpp"

namespace loa::ui::panel::tools {

namespace {

std::string lower(std::string_view s) {
    std::string out(s);
    for (auto& c : out)
        if (c >= 'A' && c <= 'Z') c = static_cast<char>(c - 'A' + 'a');
    return out;
}

std::string_view trim(std::string_view s) {
    constexpr std::string_view WS = " \t\r\n\f\v";
    const auto b = s.find_first_not_of(WS);
    if (b == std::string_view::npos) return {};
    return s.substr(b, s.find_last_not_of(WS) - b + 1);
}

std::vector<Search::Haystack> haystacks(const std::vector<ToolInfo>& tools) {
    std::vector<Search::Haystack> out;
    out.reserve(tools.size());
    for (const auto& t : tools) out.push_back({lower(t.name), lower(t.description)});
    return out;
}

std::vector<ToolRow> tool_rows(const std::vector<Rows::Row>& rows, const std::vector<ToolInfo>& infos) {
    std::vector<ToolRow> out;
    for (const auto& r : rows)
        if (r.index < infos.size()) out.push_back({r.id, &infos[r.index], r.enabled});
    return out;
}

} // namespace

std::string_view label(Toggle t) noexcept {
    static constexpr std::array<std::string_view, 3> L{"on", "off", "mixed"};
    return L[static_cast<std::size_t>(t)];
}

std::pair<Groups, Groups> names(const Catalog& c) {
    auto n = [](const std::vector<ToolRow>& rows) {
        std::vector<std::string_view> out;
        for (const auto& r : rows) out.push_back(r.info->name);
        return out;
    };
    Groups b, s;
    for (const auto& g : c.builtin) b.push_back(n(g.tools));
    for (const auto& v : c.servers) s.push_back(n(v.tools));
    return {std::move(b), std::move(s)};
}

Filter::Filter(std::string_view text) : text_(lower(trim(text))) {}
bool Filter::matches(std::string_view text) const { return empty() || matches_lower(lower(text)); }
bool Filter::matches_lower(std::string_view lower) const { return lower.find(text_) != std::string_view::npos; }
bool Filter::tool(const ToolInfo& t) const { return matches(t.name) || matches(t.description); }

Search::Search(const BuiltinTools& builtin, std::span<const McpServer> servers)
    : harness_(haystacks(builtin.harness_tools)), app_(haystacks(builtin.app_tools)) {
    for (const auto& s : servers) servers_.emplace_back(lower(s.name), haystacks(s.tools));
}

Toggle server_toggle(const McpServer& s, const std::set<ToolId>& disabled) {
    const auto n = static_cast<std::size_t>(
        std::ranges::count_if(s.tools, [&](const ToolInfo& t) { return disabled.contains(ToolId::mcp(s.name, t.name)); }));
    if (n == 0) return Toggle::On;
    return n == s.tools.size() ? Toggle::Off : Toggle::Mixed;
}

Rows::Rows(const BuiltinTools& builtin, std::span<const McpServer> servers, const Search& search, const std::set<ToolId>& disabled,
           const Filter& filter) {
    auto row = [&](ToolId id, std::size_t index) {
        const bool enabled = !disabled.contains(id);
        return Row{std::move(id), index, enabled};
    };
    auto rows = [&](const std::vector<ToolInfo>& tools, const std::vector<Search::Haystack>& found) {
        std::vector<Row> out;
        for (std::size_t i = 0; i < tools.size() && i < found.size(); ++i)
            if (found[i].matches(filter)) out.push_back(row(ToolId::builtin(tools[i].name), i));
        return out;
    };
    builtin_.clear();
    for (auto&& [group, list] : {std::pair{BuiltinGroup::Harness, rows(builtin.harness_tools, search.harness_)},
                                 std::pair{BuiltinGroup::App, rows(builtin.app_tools, search.app_)}})
        if (filter.empty() || !list.empty()) builtin_.emplace_back(group, list);
    for (std::size_t index = 0; index < servers.size() && index < search.servers_.size(); ++index) {
        const auto& s = servers[index];
        const auto& [name, found] = search.servers_[index];
        const bool all = filter.matches_lower(name);
        std::vector<Row> tools;
        for (std::size_t i = 0; i < s.tools.size() && i < found.size(); ++i)
            if (all || found[i].matches(filter)) tools.push_back(row(ToolId::mcp(s.name, s.tools[i].name), i));
        if (all || !tools.empty()) servers_.emplace_back(index, server_toggle(s, disabled), std::move(tools));
    }
}

Catalog Rows::catalog(const BuiltinTools& builtin, std::span<const McpServer> servers) const {
    Catalog c;
    for (const auto& [group, rows] : builtin_) {
        const bool harness = group == BuiltinGroup::Harness;
        c.builtin.push_back({group, harness ? std::string_view(builtin.harness) : APP_GROUP_TITLE,
                             tool_rows(rows, harness ? builtin.harness_tools : builtin.app_tools)});
    }
    for (const auto& [index, toggle, rows] : servers_)
        if (index < servers.size()) c.servers.push_back({&servers[index], toggle, tool_rows(rows, servers[index].tools)});
    return c;
}

PeerMatrix peer_matrix(const BuiltinTools& builtin, std::span<const McpServer> servers, std::span<const PeerTools> peers,
                       std::optional<std::string_view> only_peer, const Filter& filter) {
    PeerMatrix m;
    for (const auto& p : peers)
        if (!only_peer || p.key == *only_peer) m.peers.push_back(&p);
    std::vector<ToolId> ids;
    for (const auto* list : {&builtin.harness_tools, &builtin.app_tools})
        for (const auto& t : *list)
            if (filter.tool(t)) ids.push_back(ToolId::builtin(t.name));
    for (const auto& s : servers) {
        const bool all = filter.matches(s.name);
        for (const auto& t : s.tools)
            if (all || filter.tool(t)) ids.push_back(ToolId::mcp(s.name, t.name));
    }
    for (auto& id : ids) {
        MatrixRow r{std::move(id), {}};
        for (const auto* p : m.peers) r.allowed.push_back(p->allows(r.tool));
        if (!only_peer || std::ranges::any_of(r.allowed, [](bool a) { return a; })) m.rows.push_back(std::move(r));
    }
    return m;
}

} // namespace loa::ui::panel::tools
