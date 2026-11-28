#include "loa/projects/read.hpp"

#include "loa/core/schema.hpp"
#include "loa/projects/search.hpp"
#include "loa/projects/text.hpp"

namespace loa::projects {

Result<ReadPage> read_subtree(const SessionSource& source, const Access& access, std::string_view project,
                              const ReadRequest& request) {
    auto dom = open_visible(source, access, project, request.session);
    if (!dom) return std::unexpected(dom.error());
    std::optional<core::NodeId> node = request.node;
    if (!node && (*dom)->root()) node = (*dom)->root()->id;
    if (!node) return std::unexpected(Error::session_not_found(request.session));
    if (!(*dom)->contains(*node)) return std::unexpected(Error::node_not_found(request.session, node->value));
    const auto rendered = render_subtree(**dom, *node);
    const auto page = read_pages::PAGES.bytes(rendered, request.offset, request.max_bytes);
    return ReadPage{request.session, *node, rendered.substr(page.range.start, page.range.size()), page.range.start, page.next,
                    rendered.size()};
}

std::string render_subtree(const core::Dom& dom, core::NodeId node) {
    std::string out;
    const auto base = dom.ancestors(node).size();
    for (const auto& n : dom.descendants(node)) {
        std::string indent;
        for (auto depth = dom.ancestors(n.id).size() - base; depth > 0; --depth) indent += "  ";
        out += indent + std::string(core::as_str(n.kind)) + " #" + core::to_string(n.id);
        for (const auto& [name, value] : n.attrs) out += " " + name + "=" + render_value(value);
        out.push_back('\n');
        for (auto line : text::lines(n.text)) out += indent + "  " + std::string(line) + "\n";
    }
    return out;
}

} // namespace loa::projects
