#include "loa/ui/session/composer/editor.hpp"

#include <algorithm>
#include <utility>

#include "utf8.hpp"

namespace loa::ui::session::composer {

Content Content::with_text(std::string_view text) {
    Content c;
    c.lines_.clear();
    std::size_t at = 0;
    for (auto nl = text.find('\n'); nl != std::string_view::npos; nl = text.find('\n', at)) {
        auto line = text.substr(at, nl - at);
        if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
        c.lines_.emplace_back(line);
        at = nl + 1;
    }
    c.lines_.emplace_back(text.substr(at));
    return c;
}

std::string Content::text() const {
    std::string out;
    for (std::size_t i = 0; i < lines_.size(); ++i) {
        if (i > 0) out += '\n';
        out += lines_[i];
    }
    return out;
}

Cursor Content::cursor() const {
    std::optional<Position> selection;
    if (anchor_ && *anchor_ != position_) selection = anchor_;
    return Cursor{position_, selection};
}

std::optional<std::string> Content::selection() const {
    if (!anchor_ || *anchor_ == position_) return std::nullopt;
    auto [from, to] = std::minmax(*anchor_, position_);
    if (from.line == to.line) return lines_[from.line].substr(from.column, to.column - from.column);
    std::string out = lines_[from.line].substr(from.column);
    for (auto l = from.line + 1; l < to.line; ++l) out += '\n' + lines_[l];
    out += '\n' + lines_[to.line].substr(0, to.column);
    return out;
}

Position Content::clamp(Position p) const {
    p.line = std::min(p.line, lines_.size() - 1);
    const auto& line = lines_[p.line];
    p.column = std::min(p.column, line.size());
    while (!utf8::is_boundary(line, p.column)) --p.column;
    return p;
}

void Content::move_to(Cursor cursor) {
    position_ = clamp(cursor.position);
    anchor_ = cursor.selection ? std::optional(clamp(*cursor.selection)) : std::nullopt;
}

Position Content::moved(Position p, Motion m) const {
    const auto& line = lines_[p.line];
    switch (m) {
    case Motion::Left:
        if (p.column > 0) return {p.line, utf8::prev_boundary(line, p.column)};
        if (p.line > 0) return {p.line - 1, lines_[p.line - 1].size()};
        return p;
    case Motion::Right:
        if (p.column < line.size()) {
            auto i = p.column;
            (void)utf8::next(line, i);
            return {p.line, i};
        }
        if (p.line + 1 < lines_.size()) return {p.line + 1, 0};
        return p;
    case Motion::Up:
        if (p.line == 0) return {0, 0};
        return clamp({p.line - 1, p.column});
    case Motion::Down:
        if (p.line + 1 >= lines_.size()) return {p.line, line.size()};
        return clamp({p.line + 1, p.column});
    case Motion::WordLeft: {
        if (p.column == 0) return moved(p, Motion::Left);
        auto i = p.column;
        while (i > 0 && utf8::is_space(utf8::prev(line, i))) i = utf8::prev_boundary(line, i);
        if (i > 0) {
            const bool word = utf8::is_alnum(utf8::prev(line, i));
            while (i > 0) {
                const auto c = utf8::prev(line, i);
                if (utf8::is_space(c) || utf8::is_alnum(c) != word) break;
                i = utf8::prev_boundary(line, i);
            }
        }
        return {p.line, i};
    }
    case Motion::WordRight: {
        if (p.column >= line.size()) return moved(p, Motion::Right);
        auto i = p.column;
        auto peek = [&](std::size_t at) { return utf8::next(line, at); };
        while (i < line.size() && utf8::is_space(peek(i))) (void)utf8::next(line, i);
        if (i < line.size()) {
            const bool word = utf8::is_alnum(peek(i));
            while (i < line.size()) {
                const auto c = peek(i);
                if (utf8::is_space(c) || utf8::is_alnum(c) != word) break;
                (void)utf8::next(line, i);
            }
        }
        return {p.line, i};
    }
    case Motion::Home: return {p.line, 0};
    case Motion::End: return {p.line, line.size()};
    case Motion::DocumentStart: return {0, 0};
    case Motion::DocumentEnd: return {lines_.size() - 1, lines_.back().size()};
    }
    return p;
}

bool Content::delete_selection() {
    if (!anchor_ || *anchor_ == position_) {
        anchor_.reset();
        return false;
    }
    auto [from, to] = std::minmax(*anchor_, position_);
    auto tail = lines_[to.line].substr(to.column);
    lines_[from.line].resize(from.column);
    lines_[from.line] += tail;
    lines_.erase(lines_.begin() + static_cast<std::ptrdiff_t>(from.line) + 1, lines_.begin() + static_cast<std::ptrdiff_t>(to.line) + 1);
    position_ = from;
    anchor_.reset();
    return true;
}

void Content::insert(std::string_view text) {
    std::string normalized;
    normalized.reserve(text.size());
    for (std::size_t i = 0; i < text.size(); ++i) {
        if (text[i] == '\r') {
            if (i + 1 < text.size() && text[i + 1] == '\n') continue;
            normalized += '\n';
        } else {
            normalized += text[i];
        }
    }
    auto& line = lines_[position_.line];
    auto tail = line.substr(position_.column);
    line.resize(position_.column);
    std::size_t at = 0;
    auto nl = normalized.find('\n');
    if (nl == std::string::npos) {
        line += normalized;
        position_.column = line.size();
        line += tail;
        return;
    }
    line += normalized.substr(0, nl);
    std::vector<std::string> added;
    at = nl + 1;
    for (nl = normalized.find('\n', at); nl != std::string::npos; nl = normalized.find('\n', at)) {
        added.push_back(normalized.substr(at, nl - at));
        at = nl + 1;
    }
    added.push_back(normalized.substr(at));
    const auto column = added.back().size();
    added.back() += tail;
    const auto line_index = position_.line;
    lines_.insert(lines_.begin() + static_cast<std::ptrdiff_t>(line_index) + 1, added.begin(), added.end());
    position_ = {line_index + added.size(), column};
}

void Content::perform(const Action& action) {
    std::visit(
        [&]<class A>(const A& a) {
            if constexpr (std::is_same_v<A, action::Edit>) {
                std::visit(
                    [&]<class E>(const E& e) {
                        if constexpr (std::is_same_v<E, edit::Insert>) {
                            delete_selection();
                            std::string s;
                            utf8::push(s, e.c);
                            insert(s);
                        } else if constexpr (std::is_same_v<E, edit::Paste>) {
                            delete_selection();
                            insert(e.text);
                        } else if constexpr (std::is_same_v<E, edit::Enter>) {
                            delete_selection();
                            insert("\n");
                        } else if constexpr (std::is_same_v<E, edit::Backspace>) {
                            if (delete_selection()) return;
                            anchor_ = position_;
                            position_ = moved(position_, Motion::Left);
                            delete_selection();
                        } else {
                            if (delete_selection()) return;
                            anchor_ = position_;
                            position_ = moved(position_, Motion::Right);
                            delete_selection();
                        }
                    },
                    a.edit);
            } else if constexpr (std::is_same_v<A, action::Move>) {
                if (anchor_ && *anchor_ != position_ && (a.motion == Motion::Left || a.motion == Motion::Right)) {
                    auto [from, to] = std::minmax(*anchor_, position_);
                    position_ = a.motion == Motion::Left ? from : to;
                } else {
                    position_ = moved(position_, a.motion);
                }
                anchor_.reset();
            } else if constexpr (std::is_same_v<A, action::Select>) {
                if (!anchor_) anchor_ = position_;
                position_ = moved(position_, a.motion);
            } else if constexpr (std::is_same_v<A, action::SelectAll>) {
                anchor_ = Position{0, 0};
                position_ = moved(position_, Motion::DocumentEnd);
            } else if constexpr (std::is_same_v<A, action::Click>) {
                position_ = clamp(a.position);
                anchor_.reset();
            } else {
                if (!anchor_) anchor_ = position_;
                position_ = clamp(a.position);
            }
        },
        action);
}

} // namespace loa::ui::session::composer
