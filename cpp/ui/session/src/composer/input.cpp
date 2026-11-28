#include "loa/ui/session/composer/input.hpp"

#include <algorithm>
#include <type_traits>
#include <utility>

#include "loa/runtime/text.hpp"
#include "utf8.hpp"

namespace loa::ui::session::composer {

EditKind edit_kind(const Edit& edit) {
    return std::visit(
        []<class E>(const E& e) -> EditKind {
            if constexpr (std::is_same_v<E, edit::Insert>) {
                return EditKind::insert(utf8::is_space(e.c));
            } else if constexpr (std::is_same_v<E, edit::Paste>) {
                if (e.text.empty()) return EditKind::other();
                std::size_t i = 0;
                const auto c = utf8::next(e.text, i);
                if (i == e.text.size() && c != '\n' && c != '\r') return EditKind::insert(utf8::is_space(c));
                return EditKind::other();
            } else if constexpr (std::is_same_v<E, edit::Backspace> || std::is_same_v<E, edit::Delete>) {
                return EditKind::del();
            } else {
                return EditKind::other();
            }
        },
        edit);
}

// ---- UndoHistory -----------------------------------------------------------------------------

bool UndoHistory::continues(EditKind kind) const {
    if (!open_) return false;
    if (open_->kind == EditKind::Kind::Insert && kind.kind == EditKind::Kind::Insert) return !open_->whitespace || kind.whitespace;
    return open_->kind == EditKind::Kind::Delete && kind.kind == EditKind::Kind::Delete;
}

void UndoHistory::begin(EditKind kind, Snapshot before) {
    redo_.clear();
    push_undo(std::move(before));
    open_ = kind.kind != EditKind::Kind::Other ? std::optional(kind) : std::nullopt;
}

void UndoHistory::push_undo(Snapshot s) {
    undo_.push_back(std::move(s));
    if (undo_.size() > depth_) undo_.pop_front();
}

std::optional<Snapshot> UndoHistory::undo(Snapshot current) {
    open_.reset();
    if (undo_.empty()) return std::nullopt;
    auto previous = std::move(undo_.back());
    undo_.pop_back();
    redo_.push_back(std::move(current));
    return previous;
}

std::optional<Snapshot> UndoHistory::redo(Snapshot current) {
    open_.reset();
    if (redo_.empty()) return std::nullopt;
    auto next = std::move(redo_.back());
    redo_.pop_back();
    push_undo(std::move(current));
    return next;
}

// ---- Popups and mentions ---------------------------------------------------------------------

std::optional<Trigger> detect(std::size_t index, std::string_view line, std::size_t column) {
    column = std::min(column, line.size());
    if (!utf8::is_boundary(line, column)) return std::nullopt;
    const auto before = line.substr(0, column);
    std::size_t start = 0;
    for (auto i = before.size(); i > 0;) {
        const auto at = utf8::prev_boundary(before, i);
        auto j = at;
        if (utf8::is_space(utf8::next(before, j))) {
            start = j;
            break;
        }
        i = at;
    }
    if (start >= line.size()) return std::nullopt;
    PopupKind kind;
    if (line[start] == '/' && index == 0 && start == 0) kind = PopupKind::Command;
    else if (line[start] == '@') kind = PopupKind::File;
    else return std::nullopt;
    const auto space = utf8::find_space(line, column);
    const auto end = space == std::string_view::npos ? line.size() : space;
    // The cursor right before the `@` or `/` is not inside the word yet.
    if (start + 1 > before.size()) return std::nullopt;
    return Trigger{kind, index, start, end, std::string(before.substr(start + 1))};
}

void Popup::step(std::int64_t by) {
    const auto n = static_cast<std::int64_t>(matches.size());
    if (n > 0) selected = static_cast<std::size_t>((((static_cast<std::int64_t>(selected) + by) % n) + n) % n);
}

std::string token(std::string_view path) {
    if (utf8::find_space(path) != std::string_view::npos) return "@\"" + std::string(path) + "\"";
    return "@" + std::string(path);
}

std::vector<std::string_view> mentioned(std::string_view text) {
    std::vector<std::string_view> found;
    auto rest = text;
    bool at_word_start = true;
    while (!rest.empty()) {
        std::size_t i = 0;
        const auto c = utf8::next(rest, i);
        if (c != '@' || !at_word_start) {
            at_word_start = utf8::is_space(c);
            rest = rest.substr(i);
            continue;
        }
        auto after = rest.substr(1);
        at_word_start = false;
        if (!after.empty() && after.front() == '"') {
            const auto q = after.substr(1);
            if (const auto close = q.find('"'); close != std::string_view::npos) {
                found.push_back(q.substr(0, close));
                rest = q.substr(close + 1);
                continue;
            }
        }
        const auto space = utf8::find_space(after);
        const auto end = space == std::string_view::npos ? after.size() : space;
        const auto word = after.substr(0, end);
        auto trimmed = word;
        while (!trimmed.empty() && std::string_view(",.;:!?)]\"'").find(trimmed.back()) != std::string_view::npos) trimmed.remove_suffix(1);
        const std::size_t take = trimmed.size() == word.size() ? 1 : 2;
        std::size_t taken = 0;
        for (auto w : {word, trimmed}) {
            if (taken == take) break;
            if (w.empty()) continue;
            found.push_back(w);
            ++taken;
        }
        rest = after.substr(end);
    }
    return found;
}

std::vector<loa::view::session::Attachment> resolve(std::string_view text, const std::function<bool(std::string_view)>& is_known) {
    std::vector<loa::view::session::Attachment> out;
    for (const auto path : mentioned(text)) {
        if (!is_known(path)) continue;
        if (std::ranges::any_of(out, [&](const auto& a) { return a.path.native() == path; })) continue;
        loa::view::session::Attachment a;
        a.path = std::filesystem::path(std::string(path));
        if (const auto mime = loa::runtime::text::mime_of(a.path)) a.mime = std::string(*mime);
        out.push_back(std::move(a));
    }
    return out;
}

// ---- Queue -----------------------------------------------------------------------------------

void Queue::push(std::string text) { items.push_back(QueuedMessage{next_id_++, std::move(text)}); }

bool Queue::edit(std::uint64_t id, std::optional<std::string> text) {
    const auto it = std::ranges::find(items, id, &QueuedMessage::id);
    if (it == items.end()) return false;
    if (text) it->text = std::move(*text);
    else items.erase(it);
    return true;
}

std::optional<QueuedMessage> Queue::pop_sendable() {
    while (!items.empty()) {
        auto m = std::move(items.front());
        items.pop_front();
        if (!utf8::trim(m.text).empty()) return m;
    }
    return std::nullopt;
}

// ---- Key bindings ----------------------------------------------------------------------------

#if defined(__APPLE__)
bool command(Modifiers m) { return m.logo; }
bool jump(Modifiers m) { return m.alt; }
bool macos_command(Modifiers m) { return m.logo; }
#else
bool command(Modifiers m) { return m.control; }
bool jump(Modifiers m) { return m.control; }
bool macos_command(Modifiers) { return false; }
#endif

namespace {

Binding of(Message m) { return Binding{Binding::Clipboard::None, {std::move(m)}}; }
Binding of_action(Action a) { return of(msg::Edit{std::move(a)}); }
Binding sequence(std::initializer_list<Action> actions) {
    Binding b;
    for (const auto& a : actions) b.messages.emplace_back(msg::Edit{a});
    return b;
}

// iced `Binding::from_key_press`: the editor's own keys.
std::optional<Binding> from_key_press(const KeyPress& press) {
    const auto m = press.modifiers;
    const auto motion = [&](Motion plain, Motion jumped) { return jump(m) ? jumped : plain; };
    const auto go = [&](Motion mo) { return of_action(m.shift ? Action{action::Select{mo}} : Action{action::Move{mo}}); };
    switch (press.key) {
    case Key::Enter: return of_action(action::Edit{edit::Enter{}});
    case Key::Backspace: return of_action(action::Edit{edit::Backspace{}});
    case Key::Delete: return of_action(action::Edit{edit::Delete{}});
    case Key::Escape: return std::nullopt; // unfocus
    case Key::ArrowLeft: return go(macos_command(m) ? Motion::Home : motion(Motion::Left, Motion::WordLeft));
    case Key::ArrowRight: return go(macos_command(m) ? Motion::End : motion(Motion::Right, Motion::WordRight));
    case Key::ArrowUp: return go(macos_command(m) ? Motion::DocumentStart : Motion::Up);
    case Key::ArrowDown: return go(macos_command(m) ? Motion::DocumentEnd : Motion::Down);
    case Key::Home: return go(command(m) ? Motion::DocumentStart : Motion::Home);
    case Key::End: return go(command(m) ? Motion::DocumentEnd : Motion::End);
    case Key::Tab: return std::nullopt;
    case Key::Character: {
        if (command(m)) {
            if (press.text == "c") return Binding{Binding::Clipboard::Copy, {}};
            if (press.text == "x") return Binding{Binding::Clipboard::Cut, {}};
            if (press.text == "a") return of_action(action::SelectAll{});
        }
        if (press.text.empty() || command(m)) return std::nullopt;
        std::size_t i = 0;
        const auto c = utf8::next(press.text, i);
        if (c < 0x20 || c == 0x7F) return std::nullopt;
        if (i == press.text.size()) return of_action(action::Edit{edit::Insert{c}});
        return of_action(action::Edit{edit::Paste{press.text}});
    }
    case Key::Other: return std::nullopt;
    }
    return std::nullopt;
}

} // namespace

std::optional<Binding> bind(const KeyPress& press, KeyContext ctx) {
    const auto key = press.key;
    if (ctx.composing && (key == Key::Enter || key == Key::Escape || key == Key::Tab || key == Key::ArrowUp || key == Key::ArrowDown))
        return std::nullopt;
    const auto m = press.modifiers;
    if (command(m) && key == Key::Character && !press.text.empty()) {
        std::size_t i = 0;
        auto latin = utf8::next(press.text, i);
        if (latin >= 'A' && latin <= 'Z') latin += 'a' - 'A';
        switch (latin) {
        case 'z': return of(m.shift ? Message{msg::Redo{}} : Message{msg::Undo{}});
        case 'y': return of(msg::Redo{});
        // A paste also asks the app whether the clipboard holds an image.
        case 'v': return Binding{Binding::Clipboard::Paste, {msg::PasteImage{}}};
        case 'c':
        case 'x':
        case 'a': break;
        default:
            // Other Ctrl/Cmd chords are app shortcuts. On Windows AltGr arrives as Ctrl+Alt and types.
#if defined(_WIN32)
            if (m.alt) break;
#endif
            return std::nullopt;
        }
    }
    switch (key) {
    case Key::Enter:
        if (m.shift) return of_action(action::Edit{edit::Enter{}});
        if (ctx.popup_open) return of(msg::PopupAccept{});
        return of(msg::Submit{});
    case Key::Tab:
        if (ctx.popup_open && !m.shift) return of(msg::PopupAccept{});
        break;
    case Key::ArrowUp:
        if (ctx.popup_open) return of(msg::PopupStep{-1});
        break;
    case Key::ArrowDown:
        if (ctx.popup_open) return of(msg::PopupStep{1});
        break;
    case Key::Escape:
        if (ctx.popup_open) return of(msg::PopupClose{});
        if (ctx.busy) return of(msg::Interrupt{});
        break;
    case Key::Backspace:
    case Key::Delete:
        // Ctrl+Backspace / Ctrl+Delete (Alt on macOS) delete a word, or just the selection; on
        // macOS Cmd+Backspace deletes to the start of the line.
        if (ctx.selection && (jump(m) || macos_command(m)))
            return key == Key::Backspace ? of_action(action::Edit{edit::Backspace{}}) : of_action(action::Edit{edit::Delete{}});
        if (key == Key::Backspace && macos_command(m)) return sequence({action::Select{Motion::Home}, action::Edit{edit::Backspace{}}});
        if (key == Key::Backspace && jump(m)) return sequence({action::Select{Motion::WordLeft}, action::Edit{edit::Backspace{}}});
        if (key == Key::Delete && jump(m)) return sequence({action::Select{Motion::WordRight}, action::Edit{edit::Delete{}}});
        break;
    default: break;
    }
    return from_key_press(press);
}

} // namespace loa::ui::session::composer
