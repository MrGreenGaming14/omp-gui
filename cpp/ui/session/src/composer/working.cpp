#include "loa/ui/session/composer/working.hpp"

#include <cmath>
#include <format>

#include "loa/core/dom.hpp"
#include "loa/ui/kit/text.hpp"
#include "utf8.hpp"

namespace loa::ui::session::composer {

namespace working {

std::string ongoing(std::string_view what) {
    const auto cut = kit::text::truncate_trailing(what, MAX_TITLE);
    std::string_view s = cut;
    for (;;) {
        if (s.ends_with("\u2026")) s.remove_suffix(3);
        else if (s.ends_with('.') || s.ends_with(' ')) s.remove_suffix(1);
        else break;
    }
    return std::string(s) + "\u2026";
}

std::string elapsed(std::int64_t ms) {
    const auto s = (ms < 0 ? 0 : ms) / 1000;
    if (s < 60) return std::format("{}s", s);
    if (s < 3600) return std::format("{}m {:02}s", s / 60, s % 60);
    return std::format("{}h {:02}m", s / 3600, s % 3600 / 60);
}

std::string trailer(std::optional<std::int64_t> started, std::int64_t now) {
    if (!started) return std::string(INTERRUPT_HINT);
    return std::format("{} \u00b7 {}", elapsed(now - *started), INTERRUPT_HINT);
}

std::vector<Run> runs(std::string_view label, float center) {
    std::vector<Run> out;
    std::size_t index = 0;
    for (std::size_t at = 0; at < label.size(); ++index) {
        const auto end = utf8::grapheme_end(label, at);
        const auto distance = std::fabs(static_cast<float>(index) - center);
        const std::size_t level = static_cast<std::size_t>(distance < BAND) + static_cast<std::size_t>(distance < BAND / 3.0F);
        if (!out.empty() && out.back().level == level) out.back().end = end;
        else out.push_back({at, end, level});
        at = end;
    }
    return out;
}

float band_center(std::string_view label, float progress) {
    std::size_t graphemes = 0;
    for (std::size_t at = 0; at < label.size(); at = utf8::grapheme_end(label, at)) ++graphemes;
    return progress * (static_cast<float>(graphemes) + 2.0F * BAND) - BAND;
}

} // namespace working

ActivityModel ActivityModel::of(const core::Dom& dom) {
    ActivityModel model;
    const auto* root = dom.root();
    if (!root) return model;
    const core::Node* turn = nullptr;
    for (auto it = root->children.rbegin(); it != root->children.rend() && !turn; ++it)
        if (const auto* n = dom.get(*it); n && n->kind == core::ElementKind::Turn) turn = n;
    if (!turn) return model;
    namespace attr = core::attr;
    if (const auto s = turn->str_attr(attr::STATUS)) model.status = core::parse<core::TurnStatus>(*s);
    model.started = turn->int_attr(attr::STARTED);
    for (const auto id : turn->children) {
        const auto* n = dom.get(id);
        if (!n) continue;
        Item item;
        const auto str = [&](std::string_view name) -> std::optional<std::string> {
            if (const auto v = n->str_attr(name)) return std::string(*v);
            return std::nullopt;
        };
        if (n->kind == core::ElementKind::Call) {
            item.kind = Item::Kind::Call;
            if (const auto s = n->str_attr(attr::STATUS)) item.call.status = core::parse<core::CallStatus>(*s);
            item.call.title = str(attr::TITLE);
            item.call.tool = str(attr::TOOL);
        } else if (n->kind == core::ElementKind::Message) {
            item.kind = Item::Kind::Message;
            if (const auto s = n->str_attr(attr::STATUS)) item.message.status = core::parse<core::MessageStatus>(*s);
            if (const auto k = n->str_attr(attr::KIND)) item.message.kind = core::parse<core::MessageKind>(*k);
        }
        model.items.push_back(std::move(item));
    }
    return model;
}

Activity Activity::of(const ActivityModel& model) {
    if (model.status == core::TurnStatus::Waiting) return waiting();
    if (model.status != core::TurnStatus::Running) return idle();
    for (auto it = model.items.rbegin(); it != model.items.rend(); ++it) {
        if (it->kind == ActivityModel::Item::Kind::Call && it->call.status == core::CallStatus::Running) {
            std::string_view title;
            if (it->call.title) {
                std::string_view t = *it->call.title;
                t = t.substr(0, t.find('\n'));
                if (t.ends_with('\r')) t.remove_suffix(1);
                title = utf8::trim(t);
            }
            if (title.empty() && it->call.tool) title = *it->call.tool;
            return working(working::ongoing(title), model.started);
        }
        if (it->kind == ActivityModel::Item::Kind::Message && it->message.status == core::MessageStatus::Streaming) {
            const bool thinking = it->message.kind == core::MessageKind::Thinking;
            return working(std::string(thinking ? working::THINKING : working::WORKING), model.started);
        }
    }
    return working(std::string(working::WORKING), model.started);
}

} // namespace loa::ui::session::composer
