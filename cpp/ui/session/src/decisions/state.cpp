#include "loa/ui/session/decisions/state.hpp"

#include <charconv>
#include <vector>

namespace loa::ui::session::decisions {

std::optional<Shortcut> shortcut(const KeyPress& key) {
    if (key.ctrl || key.alt || key.logo) return std::nullopt;
    switch (key.named) {
    case KeyPress::Named::Enter: return Shortcut::confirm();
    case KeyPress::Named::Escape: return Shortcut::dismiss();
    case KeyPress::Named::None: break;
    }
    std::string_view t = key.text;
    if (t.size() > 1 && t[0] == '+') t.remove_prefix(1);
    std::size_t n = 0;
    const auto [ptr, ec] = std::from_chars(t.data(), t.data() + t.size(), n);
    if (t.empty() || ec != std::errc{} || ptr != t.data() + t.size() || n < 1 || n > 9) return std::nullopt;
    return Shortcut::pick(n - 1);
}

void State::sync(const loa::core::Dom& dom) {
    std::vector<NodeId> seen;
    for (auto& decision : Decision::all(dom)) {
        seen.push_back(decision.id);
        if (auto it = cards_.find(decision.id); it != cards_.end()) it->second.refresh(std::move(decision));
        else cards_.emplace(decision.id, Card(std::move(decision)));
    }
    std::erase_if(cards_, [&](const auto& entry) { return std::ranges::find(seen, entry.first) == seen.end(); });
}

void State::answer_failed(NodeId decision) {
    if (auto it = cards_.find(decision); it != cards_.end()) it->second.pending = false;
}

void State::answering(NodeId decision) {
    if (auto it = cards_.find(decision); it != cards_.end() && it->second.open()) it->second.pending = true;
}

std::optional<NodeId> State::focused() const {
    if (viewer_.view_only) return std::nullopt;
    for (const auto& [id, card] : cards_)
        if (card.open()) return id;
    return std::nullopt;
}

const Card* State::card(NodeId decision) const {
    const auto it = cards_.find(decision);
    return it == cards_.end() ? nullptr : &it->second;
}

std::optional<Footer> State::footer_of(const Card& c) const {
    std::optional<std::string_view> name;
    if (local_name_) name = *local_name_;
    return footer(c, viewer_.view_only, viewer_.by, name);
}

std::pair<Task, std::optional<Intent>> State::update(Message message) {
    if (const auto* link = std::get_if<msg::Link>(&message))
        return {Task::none(), loa::view::session::decisions::intent::OpenLink{link->url}};
    if (const auto* toggle = std::get_if<msg::ToggleDiff>(&message)) {
        if (auto it = cards_.find(toggle->id); it != cards_.end()) it->second.show_full = !it->second.show_full;
        return {Task::none(), std::nullopt};
    }
    std::optional<NodeId> id;
    if (std::holds_alternative<msg::Key>(message)) {
        id = focused();
    } else if (!viewer_.view_only) {
        id = std::visit(
            [](const auto& m) -> std::optional<NodeId> {
                using T = std::decay_t<decltype(m)>;
                if constexpr (requires { m.id; }) return m.id;
                else return std::nullopt;
            },
            message);
    }
    if (!id) return {Task::none(), std::nullopt};
    const auto it = cards_.find(*id);
    if (it == cards_.end() || !it->second.open()) return {Task::none(), std::nullopt};
    Card& card = it->second;
    const CardKind& kind = card.kind();
    std::optional<Reply> reply = std::visit(
        [&](auto& m) -> std::optional<Reply> {
            using T = std::decay_t<decltype(m)>;
            if constexpr (std::is_same_v<T, msg::Key>) {
                switch (m.shortcut.kind) {
                case Shortcut::Kind::Pick: card.pick(m.shortcut.index); return std::nullopt;
                case Shortcut::Kind::Confirm: return kind.confirm(card);
                case Shortcut::Kind::Dismiss: return kind.dismiss(card);
                }
                return std::nullopt;
            } else if constexpr (std::is_same_v<T, msg::Submit>) {
                return kind.confirm(card);
            } else if constexpr (std::is_same_v<T, msg::Skip>) {
                return kind.dismiss(card);
            } else if constexpr (std::is_same_v<T, msg::Choose>) {
                return card.choose(m.index);
            } else if constexpr (std::is_same_v<T, msg::SubmitText>) {
                return kind.submit_text(card);
            } else if constexpr (std::is_same_v<T, msg::Text>) {
                card.text = std::move(m.text);
                return std::nullopt;
            } else if constexpr (std::is_same_v<T, msg::EditPlan>) {
                if (card.editor) card.editor = std::move(m.text);
                return std::nullopt;
            } else if constexpr (std::is_same_v<T, msg::CancelEdit>) {
                card.editor = std::nullopt;
                return std::nullopt;
            } else if constexpr (std::is_same_v<T, msg::Field>) {
                card.set_field(m.index, m.input);
                return std::nullopt;
            } else {
                return std::nullopt;
            }
        },
        message);
    if (!reply) return {Task::none(), std::nullopt};
    card.pending = true;
    return {Task::none(), loa::view::session::decisions::intent::Answer{
                              *id, loa::contracts::Answer{std::move(reply->values), std::move(reply->reason), viewer_.by}}};
}

} // namespace loa::ui::session::decisions
