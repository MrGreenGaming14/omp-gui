#include "loa/p2p/sessions.hpp"

#include <algorithm>

#include "loa/bus/bus.hpp"

namespace loa::p2p {

namespace {

std::string_view trim(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto first = s.find_first_not_of(ws);
    if (first == std::string_view::npos) return {};
    return s.substr(first, s.find_last_not_of(ws) - first + 1);
}

// The first `PREVIEW_CHARS` characters of the turn's user message, `…` when cut.
std::string preview(const core::Dom& dom, core::NodeId turn) {
    std::string_view text;
    for (const auto& n : dom.descendants(turn))
        if (n.kind == core::ElementKind::Message && n.str_attr(core::attr::ROLE) == "user") {
            text = trim(n.text);
            break;
        }
    std::size_t chars = 0, end = 0;
    while (end < text.size() && chars < PREVIEW_CHARS) {
        const auto lead = static_cast<unsigned char>(text[end]);
        end += lead < 0x80 ? 1 : lead < 0xE0 ? 2 : lead < 0xF0 ? 3 : 4;
        ++chars;
    }
    end = std::min(end, text.size());
    std::string out(text.substr(0, end));
    if (end < text.size()) out += "\u2026";
    return out;
}

} // namespace

std::optional<LocalSession> LocalSession::from_dom(const core::Dom& dom) {
    auto facts = trust::SessionFacts::from_dom(dom);
    if (!facts) return std::nullopt;
    const auto* root = dom.root();
    if (!root) return std::nullopt;
    LocalSession out{.facts = std::move(*facts),
                     .title = std::string(root->str_attr(core::attr::TITLE).value_or("")),
                     .harness = std::string(root->str_attr(core::attr::HARNESS).value_or("")),
                     .last_seq = dom.seq(),
                     .turns = {}};
    for (const auto* turn : dom.children_of_kind(root->id, core::ElementKind::Turn))
        out.turns.push_back({turn->id, preview(dom, turn->id)});
    return out;
}

std::string LocalSession::label() const {
    if (!title.empty()) return title;
    if (!turns.empty() && !turns.front().preview.empty()) return turns.front().preview;
    return facts.id;
}

std::vector<LocalSession> BusSessions::sessions() const {
    std::vector<LocalSession> out;
    for (const auto& id : bus_->ids()) {
        auto writer = bus_->get(id.as_str());
        if (!writer || !writer->is_writer()) continue;
        if (auto session = writer->with_dom([](const core::Dom& dom) { return LocalSession::from_dom(dom); }))
            out.push_back(std::move(*session));
    }
    std::ranges::sort(out, [](const LocalSession& a, const LocalSession& b) {
        if (a.facts.created_ms != b.facts.created_ms) return a.facts.created_ms > b.facts.created_ms;
        return a.facts.id < b.facts.id;
    });
    return out;
}

std::shared_ptr<bus::SessionWriter> BusSessions::writer(std::string_view id) const {
    auto writer = bus_->get(id);
    return writer && writer->is_writer() ? writer : nullptr;
}

} // namespace loa::p2p
