#include "loa/ui/session/composer/state.hpp"

#include <algorithm>
#include <type_traits>

#include "loa/core/value.hpp"
#include "loa/runtime/text.hpp"
#include "loa/ui/kit/text.hpp"
#include "utf8.hpp"

namespace loa::ui::session::composer {

namespace intent = loa::view::session::composer::intent;
using loa::view::session::Attachment;
using loa::view::session::Prompt;
using loa::view::session::SessionMode;

namespace {

// A listed picker option: its id for `pick_key`, label, note and whether it can be picked.
std::string key_of(const HarnessOption& o) { return o.id; }
std::string key_of(const ModelOption& o) { return o.id; }
std::string key_of(const AccessOption& o) { return o.id; }
std::string key_of(const Workspace& o) { return o.full; }
std::string key_of(const Label& o) { return o.text; }
std::string key_of(const WorkflowOption& o) { return o.role; }

MenuRow row_of(const HarnessOption& o) { return {o.label, o.detail, o.available}; }
MenuRow row_of(const ModelOption& o) { return {o.name, o.role ? o.provider + " \u00b7 " + *o.role : o.provider, true}; }
MenuRow row_of(const AccessOption& o) { return {o.label, o.detail, true}; }
MenuRow row_of(const Workspace& o) { return {o.path, o.branch, true}; }
MenuRow row_of(const Label& o) { return {o.text, std::nullopt, true}; }
MenuRow row_of(const WorkflowOption& o) { return {o.label, std::nullopt, true}; }

// Selects option `i` of `p` and returns it when that changes the selection.
template <class T>
std::optional<T> select(Picker<T>& p, std::size_t i) {
    if (i >= p.options.size() || p.selected == i) return std::nullopt;
    p.selected = i;
    return p.options[i];
}

template <class T>
Picker<T> select_by(std::vector<T> options, std::optional<std::string_view> current) {
    Picker<T> p;
    if (current) {
        const auto it = std::ranges::find_if(options, [&](const T& o) { return key_of(o) == *current; });
        if (it != options.end()) p.selected = static_cast<std::size_t>(it - options.begin());
    }
    p.options = std::move(options);
    return p;
}

std::vector<Label> mode_labels() {
    std::vector<Label> out;
    for (const auto m : core::all<SessionMode>()) out.push_back(Label{std::string(core::as_str(m))});
    return out;
}

bool contains(std::string_view haystack, std::string_view needle) { return haystack.find(needle) != std::string_view::npos; }

} // namespace

// ---- Draft -----------------------------------------------------------------------------------

void Draft::prepend(Draft earlier) {
    if (earlier.is_empty()) return;
    const auto offset = next_image;
    auto text_ = std::move(earlier.text);
    auto imgs = std::move(earlier.images);
    // Highest first, so a renumbered token is never renumbered again.
    std::ranges::sort(imgs, std::greater{}, &Image::n);
    for (auto& image : imgs) {
        text_ = utf8::replace_all(text_, image_token(image.n), image_token(image.n + offset));
        image.n += offset;
    }
    images.insert(images.end(), std::make_move_iterator(imgs.begin()), std::make_move_iterator(imgs.end()));
    std::ranges::stable_sort(images, {}, &Image::n);
    for (auto& mention : earlier.mentions)
        if (std::ranges::find(mentions, mention) == mentions.end()) mentions.push_back(std::move(mention));
    next_image += earlier.next_image;
    const auto lines = static_cast<std::size_t>(std::ranges::count(text_, '\n'));
    if (text.empty()) {
        const auto last_nl = text_.rfind('\n');
        const Position end{lines, last_nl == std::string::npos ? text_.size() : text_.size() - last_nl - 1};
        cursor = offset == 0 ? earlier.cursor : Cursor{end, std::nullopt};
        text = std::move(text_);
        return;
    }
    const auto shift = [&](Position p) { return Position{p.line + lines + 2, p.column}; };
    cursor = Cursor{shift(cursor.position), cursor.selection ? std::optional(shift(*cursor.selection)) : std::nullopt};
    text = text_ + "\n\n" + text;
}

// ---- State -----------------------------------------------------------------------------------

State::State() {
    pickers_.mode = Picker<Label>{mode_labels(), 0};
    pickers_.project = Picker<Label>{{Label{std::string(NO_PROJECT)}}, 0};
}

void State::set_folders(std::vector<Workspace> folders) {
    pickers_.folders = std::move(folders);
    rebuild_folders();
}

void State::set_workspace(std::optional<Workspace> workspace) {
    pickers_.folder = {};
    if (workspace) {
        pickers_.folder.options.push_back(std::move(*workspace));
        pickers_.folder.selected = 0;
    }
    rebuild_folders();
}

void State::rebuild_folders() {
    auto& p = pickers_;
    p.folder.options.resize(p.folder.selected ? std::min<std::size_t>(1, p.folder.options.size()) : 0);
    for (const auto& folder : p.folders)
        if (std::ranges::none_of(p.folder.options, [&](const Workspace& f) { return f.full == folder.full; }))
            p.folder.options.push_back(folder);
}

void State::set_commands(std::vector<Command> commands) {
    commands_ = std::move(commands);
    refilter();
}

void State::set_files(std::vector<std::string> files) {
    files_ = std::move(files);
    refilter();
}

void State::set_models(std::vector<ModelOption> models, std::optional<std::string_view> current) {
    pickers_.model = select_by(std::move(models), current);
}

void State::set_efforts(std::vector<std::string> efforts, std::optional<std::string> current) {
    std::vector<Label> labels;
    for (auto& e : efforts) labels.push_back(Label{std::move(e)});
    pickers_.effort = select_by(std::move(labels), current ? std::optional<std::string_view>(*current) : std::nullopt);
}

std::pair<std::vector<std::string>, std::optional<std::string>> State::efforts() const {
    std::vector<std::string> out;
    for (const auto& e : pickers_.effort.options) out.push_back(e.text);
    const auto* c = pickers_.effort.chosen();
    return {std::move(out), c ? std::optional(c->text) : std::nullopt};
}

void State::set_mode(SessionMode mode) {
    const auto all = core::all<SessionMode>();
    const auto it = std::ranges::find(all, mode);
    pickers_.mode.selected = it == all.end() ? std::nullopt : std::optional(static_cast<std::size_t>(it - all.begin()));
}

void State::set_access(std::vector<AccessOption> options, std::optional<std::string_view> current) {
    pickers_.access = select_by(std::move(options), current);
}

void State::set_harnesses(std::vector<HarnessOption> harnesses, std::optional<std::string_view> current) {
    pickers_.harness = select_by(std::move(harnesses), current);
}

void State::set_workflows(std::vector<WorkflowOption> options, bool enabled) {
    const auto it = std::ranges::find_if(options, &WorkflowOption::current);
    pickers_.workflow.selected = it == options.end() ? std::nullopt : std::optional(static_cast<std::size_t>(it - options.begin()));
    pickers_.workflow.options = std::move(options);
    pickers_.workflows_enabled = enabled;
    if (!enabled && pickers_.open == PickerId::Workflow) pickers_.open.reset();
}

void State::set_projects(std::vector<std::string> projects, std::optional<std::string> current) {
    std::vector<Label> options{Label{std::string(NO_PROJECT)}};
    for (auto& p : projects) options.push_back(Label{std::move(p)});
    std::optional<std::size_t> selected = 0;
    if (current) {
        selected.reset();
        for (std::size_t i = 1; i < options.size(); ++i)
            if (options[i].text == *current) {
                selected = i;
                break;
            }
    }
    pickers_.project = Picker<Label>{std::move(options), selected};
}

// Calls `f` with the picker `id` names.
#define LOA_WITH_PICKER(id, f)                                                                                         \
    [&]() -> decltype(auto) {                                                                                          \
        switch (id) {                                                                                                  \
        case PickerId::Harness: return f(pickers_.harness);                                                            \
        case PickerId::Model: return f(pickers_.model);                                                                \
        case PickerId::Workflow: return f(pickers_.workflow);                                                          \
        case PickerId::Effort: return f(pickers_.effort);                                                              \
        case PickerId::Access: return f(pickers_.access);                                                              \
        case PickerId::Mode: return f(pickers_.mode);                                                                  \
        case PickerId::Folder: return f(pickers_.folder);                                                              \
        case PickerId::Project: break;                                                                                 \
        }                                                                                                              \
        return f(pickers_.project);                                                                                    \
    }()

std::size_t State::menu_len(PickerId picker) const {
    return LOA_WITH_PICKER(picker, [](const auto& p) { return p.options.size(); });
}

std::string State::menu_key(PickerId picker, std::size_t i) const {
    return LOA_WITH_PICKER(picker, [&](const auto& p) { return key_of(p.options[i]); });
}

std::vector<MenuRow> State::menu_rows(PickerId picker) const {
    return LOA_WITH_PICKER(picker, [](const auto& p) {
        std::vector<MenuRow> out;
        for (const auto& o : p.options) out.push_back(row_of(o));
        return out;
    });
}

std::optional<std::size_t> State::menu_current(PickerId picker) const {
    return LOA_WITH_PICKER(picker, [](const auto& p) { return p.selected; });
}

#undef LOA_WITH_PICKER

std::vector<std::string> State::menu_actions(PickerId picker) const {
    std::vector<std::string> out;
    switch (picker) {
    case PickerId::Model:
        for (const auto& a : pickers_.model_actions) out.push_back(a.label);
        break;
    case PickerId::Folder:
        for (const auto a : FOLDER_ACTIONS) out.emplace_back(a);
        break;
    case PickerId::Workflow: out.emplace_back(WORKFLOW_ACTION); break;
    default: break;
    }
    return out;
}

std::optional<Message> State::pick_key(PickerId picker, std::string_view key) const {
    const auto n = menu_len(picker);
    for (std::size_t i = 0; i < n; ++i)
        if (menu_key(picker, i) == key) return msg::Pick{picker, i};
    if (picker == PickerId::Model)
        for (std::size_t i = 0; i < pickers_.model_actions.size(); ++i)
            if (pickers_.model_actions[i].id == key) return msg::Pick{picker, n + i};
    return std::nullopt;
}

std::vector<Chip> State::chips() const {
    const auto& p = pickers_;
    const auto open = [&](PickerId id) { return p.open == id; };
    std::vector<Chip> out;
    if (p.harness.options.size() > 1)
        out.push_back({PickerId::Harness, harness() ? harness()->label : "Harness", open(PickerId::Harness), true});
    if (p.model.options.size() + p.model_actions.size() > 0)
        out.push_back({PickerId::Model, p.model.chosen() ? short_model_name(p.model.chosen()->name) : "Model", open(PickerId::Model), true});
    out.push_back({PickerId::Workflow, p.workflow.chosen() ? p.workflow.chosen()->label : "Default", open(PickerId::Workflow),
                   p.workflows_enabled});
    if (!p.effort.options.empty())
        out.push_back({PickerId::Effort, capitalized(p.effort.chosen() ? p.effort.chosen()->text : "Effort"), open(PickerId::Effort), true});
    if (!p.access.options.empty())
        out.push_back({PickerId::Access, p.access.chosen() ? p.access.chosen()->label : "Permissions", open(PickerId::Access), true});
    return out;
}

std::vector<Chip> State::bar_chips() const {
    const auto& p = pickers_;
    return {
        {PickerId::Project, p.project.chosen() ? p.project.chosen()->text : std::string(NO_PROJECT), p.open == PickerId::Project, true},
        {PickerId::Mode, capitalized(p.mode.chosen() ? p.mode.chosen()->text : ""), p.open == PickerId::Mode, true},
    };
}

std::optional<std::size_t> State::effort_shown() const {
    if (effort_drag_) return *effort_drag_;
    return pickers_.effort.selected;
}

std::string State::effort_title() const {
    const auto shown = effort_shown();
    if (shown && *shown < pickers_.effort.options.size()) return capitalized(pickers_.effort.options[*shown].text);
    return "Effort";
}

std::optional<std::string> State::effort_model() const {
    if (const auto* m = pickers_.model.chosen()) return short_model_name(m->name);
    return std::nullopt;
}

std::optional<Intent> State::turn_ended() {
    phase_ = Phase::Idle;
    auto next = queue_.pop_sendable();
    if (!next) return std::nullopt;
    return send(utf8::trim(next->text));
}

std::optional<std::string_view> State::indicator() const {
    if (phase_ == Phase::Idle || activity_.kind == Activity::Kind::Waiting) return std::nullopt;
    if (activity_.kind == Activity::Kind::Working) return std::string_view(activity_.label);
    return phase_ == Phase::Starting ? working::STARTING : working::WORKING;
}

std::optional<std::string> State::trailer() const {
    if (phase_ != Phase::Running) return std::nullopt;
    const auto started = activity_.kind == Activity::Kind::Working ? activity_.started : std::nullopt;
    return working::trailer(started, now_);
}

void State::follow_session(bool busy) {
    if (busy) {
        phase_ = Phase::Running;
    } else if (phase_ == Phase::Running) {
        phase_ = Phase::Idle;
        unqueue();
    }
}

Draft State::take_draft() {
    Draft draft{content_.text(), content_.cursor(), {}, {}, 0};
    if (queue_.items.empty()) {
        draft.images = std::exchange(images_, {});
        draft.mentions = std::exchange(mentions_, {});
        draft.next_image = std::exchange(next_image_, 0);
    } else {
        draft.images = images_;
        draft.mentions = mentions_;
        draft.next_image = next_image_;
    }
    content_ = Content();
    reset_editing();
    return draft;
}

void State::restore_draft(Draft draft) {
    auto kept = queued_attachments();
    kept.prepend(std::move(draft));
    show_draft(std::move(kept));
}

void State::prepend_draft(Draft draft) {
    Draft current{content_.text(), content_.cursor(), std::exchange(images_, {}), std::exchange(mentions_, {}), next_image_};
    current.prepend(std::move(draft));
    show_draft(std::move(current));
}

Draft State::queued_attachments() {
    auto imgs = std::exchange(images_, {});
    auto ments = std::exchange(mentions_, {});
    if (queue_.items.empty()) return Draft{};
    return Draft{{}, {}, std::move(imgs), std::move(ments), next_image_};
}

void State::show_draft(Draft draft) {
    content_ = Content::with_text(draft.text);
    content_.move_to(draft.cursor);
    images_ = std::move(draft.images);
    mentions_ = std::move(draft.mentions);
    next_image_ = draft.next_image;
    reset_editing();
}

void State::reset_editing() {
    text_changed();
    history_.clear();
    popup_.reset();
    dismissed_.reset();
    composing_ = false;
}

Task State::give_back(std::string_view text) {
    phase_ = Phase::Idle;
    const auto typed = content_.text();
    if (utf8::trim(typed).empty()) return set_text(text);
    return set_text(std::string(text) + "\n\n" + std::string(utf8::trim_end(typed)));
}

Task State::set_text(std::string_view text) {
    history_.break_group();
    history_.begin(EditKind::other(), snapshot());
    replace(text);
    return Task::focus();
}

void State::replace(std::string_view text) {
    content_ = Content::with_text(text);
    const auto line = content_.line_count() - 1;
    content_.move_to(Cursor{{line, content_.line(line)->size()}, std::nullopt});
    text_changed();
}

Task State::attach_image(PastedImage image) {
    const auto n = ++next_image_;
    images_.push_back(Image{n, std::move(image), false});
    insert_word(image_token(n));
    return Task::focus();
}

Task State::mention_path(std::string_view path) {
    if (std::ranges::find(mentions_, path) == mentions_.end()) mentions_.emplace_back(path);
    insert_word(token(path));
    return Task::focus();
}

std::optional<PopupKind> State::popup_kind() const {
    const auto* p = visible_popup();
    return p ? std::optional(p->trigger.kind) : std::nullopt;
}

std::pair<std::vector<std::string>, std::size_t> State::popup_items() const {
    const auto* p = visible_popup();
    if (!p) return {{}, 0};
    std::vector<std::string> out;
    for (const auto i : p->matches) out.push_back(candidate(p->trigger.kind, i));
    return {std::move(out), p->selected};
}

std::vector<std::string> State::popup_details() const {
    const auto* p = visible_popup();
    std::vector<std::string> out;
    if (!p) return out;
    for (const auto i : p->matches) out.push_back(p->trigger.kind == PopupKind::Command ? commands_[i].description : std::string());
    return out;
}

KeyContext State::key_context() const {
    return KeyContext{visible_popup() != nullptr, composing_, is_busy(), content_.cursor().selection.has_value()};
}

std::string State::placeholder() const {
    if (is_busy()) return "Queue a message\u2026";
    if (const auto* h = harness()) return "Ask " + h->label + " anything. Type / for commands";
    return "Ask anything. Type / for commands";
}

std::optional<std::chrono::milliseconds> State::tick() const {
    const bool working_ = indicator().has_value();
    if (focused_ && keyword_) return std::chrono::duration_cast<std::chrono::milliseconds>(shimmer::EVERY_STEP);
    if (focused_) return working_ ? std::optional(working::FRAME) : std::nullopt;
    return working_ ? std::optional(working::UNFOCUSED) : std::nullopt;
}

std::pair<Task, std::optional<Intent>> State::update(Message message) {
    const bool focus = std::holds_alternative<msg::Send>(message) || std::holds_alternative<msg::MentionPressed>(message) ||
                       (std::holds_alternative<msg::PopupAccept>(message) && std::get<msg::PopupAccept>(message).row.has_value());
    if (std::holds_alternative<msg::Pick>(message) || std::holds_alternative<msg::PickerClosed>(message)) pickers_.open.reset();
    auto event = std::visit(
        [&]<class M>(M& m) -> std::optional<Intent> {
            if constexpr (std::is_same_v<M, msg::Edit>) return edit(m.action);
            else if constexpr (std::is_same_v<M, msg::Submit> || std::is_same_v<M, msg::Send>) return submit();
            else if constexpr (std::is_same_v<M, msg::Interrupt>) return stop();
            else if constexpr (std::is_same_v<M, msg::Undo>) return step(false);
            else if constexpr (std::is_same_v<M, msg::Redo>) return step(true);
            else if constexpr (std::is_same_v<M, msg::PopupStep>) {
                if (popup_) popup_->step(m.by);
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::PopupAccept>) return accept(m.row);
            else if constexpr (std::is_same_v<M, msg::PopupClose>) {
                dismissed_ = popup_ ? std::optional(popup_->trigger) : std::nullopt;
                popup_.reset();
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::Queue>) {
                if (queue_.edit(m.id, std::move(m.text))) return queue_changed();
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::Toggle>) {
                const bool opens = pickers_.open != m.picker && (m.picker != PickerId::Workflow || pickers_.workflows_enabled);
                pickers_.open = opens ? std::optional(m.picker) : std::nullopt;
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::Pick>) return pick(m.picker, m.row);
            else if constexpr (std::is_same_v<M, msg::OpenRules>) {
                pickers_.open.reset();
                if (pickers_.workflows_enabled) return Intent{intent::OpenRules{}};
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::PickerClosed>) return std::nullopt;
            else if constexpr (std::is_same_v<M, msg::Composing>) {
                composing_ = m.composing;
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::MentionPressed>) return start_mention();
            else if constexpr (std::is_same_v<M, msg::PasteImage>) return Intent{intent::PasteImage{}};
            else if constexpr (std::is_same_v<M, msg::RemoveImage>) return remove_image(m.n);
            else if constexpr (std::is_same_v<M, msg::Shimmer>) {
                // Unfocused, only the elapsed time moves on.
                now_ = m.now;
                if (focused_) shimmer_ = shimmer::phase_at(m.now);
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::WindowFocused>) {
                if (!window_ || *window_ == m.window) focused_ = m.focused;
                return std::nullopt;
            } else if constexpr (std::is_same_v<M, msg::EffortSlid>) {
                effort_drag_ = m.level;
                return std::nullopt;
            } else {
                // The card stays open, so the level can be moved again.
                const auto level = std::exchange(effort_drag_, std::nullopt);
                if (!level) return std::nullopt;
                if (auto e = select(pickers_.effort, *level)) return Intent{intent::EffortChanged{e->text}};
                return std::nullopt;
            }
        },
        message);
    return {focus ? Task::focus() : Task::none(), std::move(event)};
}

std::optional<Intent> State::pick(PickerId picker, std::size_t i) {
    auto& p = pickers_;
    switch (picker) {
    case PickerId::Harness:
        if (i >= p.harness.options.size() || !p.harness.options[i].available) return std::nullopt;
        if (auto h = select(p.harness, i)) return Intent{intent::HarnessChanged{h->id}};
        return std::nullopt;
    case PickerId::Model:
        if (i >= p.model.options.size()) {
            const auto a = i - p.model.options.size();
            if (a < p.model_actions.size()) return Intent{intent::ModelAction{p.model_actions[a].id}};
            return std::nullopt;
        }
        if (auto m = select(p.model, i)) return Intent{intent::ModelChanged{*m}};
        return std::nullopt;
    case PickerId::Workflow:
        if (!p.workflows_enabled) return std::nullopt;
        if (i == p.workflow.options.size()) return Intent{intent::OpenRules{}};
        if (auto w = select(p.workflow, i)) return Intent{intent::WorkflowPicked{w->role}};
        return std::nullopt;
    case PickerId::Effort:
        if (auto e = select(p.effort, i)) return Intent{intent::EffortChanged{e->text}};
        return std::nullopt;
    case PickerId::Access:
        if (auto a = select(p.access, i)) return Intent{intent::AccessChanged{a->id}};
        return std::nullopt;
    case PickerId::Mode:
        if (select(p.mode, i)) return Intent{intent::ModeChanged{core::all<SessionMode>()[i]}};
        return std::nullopt;
    case PickerId::Project:
        if (auto l = select(p.project, i)) return Intent{intent::ProjectPicked{i > 0 ? std::optional(l->text) : std::nullopt}};
        return std::nullopt;
    case PickerId::Folder:
        if (i < p.folder.options.size()) return Intent{intent::FolderPicked{p.folder.options[i].full}};
        if (i == p.folder.options.size()) return Intent{intent::BrowseFolders{}};
        if (i == p.folder.options.size() + 1) return Intent{intent::OpenWorkspace{}};
        return std::nullopt;
    }
    return std::nullopt;
}

const Popup* State::visible_popup() const { return popup_ && !popup_->matches.empty() ? &*popup_ : nullptr; }

const std::string& State::candidate(PopupKind kind, std::size_t index) const {
    return kind == PopupKind::Command ? commands_[index].name : files_[index];
}

std::optional<Intent> State::edit(const Action& action) {
    pickers_.open.reset();
    const auto* e = std::get_if<action::Edit>(&action);
    if (!e) {
        history_.break_group();
        content_.perform(action);
        return refresh_popup();
    }
    const auto kind = edit_kind(e->edit);
    if (history_.continues(kind)) {
        content_.perform(action);
        history_.extend(kind);
    } else {
        auto before = snapshot();
        const auto line_count = content_.line_count();
        content_.perform(action);
        // The text is compared only when the cursor and line count stay put.
        const bool moved = content_.cursor() != before.cursor || content_.line_count() != line_count;
        if (!moved && content_.text() == before.text) history_.break_group();
        else history_.begin(kind, std::move(before));
    }
    text_changed();
    return refresh_popup();
}

std::optional<Intent> State::step(bool redo) {
    auto target = redo ? history_.redo(snapshot()) : history_.undo(snapshot());
    if (!target) return std::nullopt;
    content_ = Content::with_text(target->text);
    content_.move_to(target->cursor);
    text_changed();
    return refresh_popup();
}

std::optional<Intent> State::start_mention() {
    return edit(action::Edit{edit::Paste{after_word() ? " @" : "@"}});
}

bool State::after_word() const {
    const auto c = content_.cursor();
    const auto* line = content_.line(c.position.line);
    if (c.selection || !line || c.position.column == 0 || c.position.column > line->size()) return false;
    return !utf8::is_space(utf8::prev(*line, c.position.column));
}

void State::insert_word(std::string_view word) {
    const auto text = after_word() ? " " + std::string(word) + " " : std::string(word) + " ";
    history_.break_group();
    (void)edit(action::Edit{edit::Paste{text}});
    history_.break_group();
}

std::optional<Intent> State::remove_image(std::uint32_t n) {
    const auto tok = image_token(n);
    auto before = snapshot();
    if (contains(before.text, tok)) {
        const auto text = utf8::replace_all(utf8::replace_all(before.text, tok + " ", ""), tok, "");
        history_.break_group();
        history_.begin(EditKind::other(), std::move(before));
        replace(text);
    }
    // The text moved under any open popup's word.
    return refresh_popup();
}

void State::text_changed() {
    const auto& lines = content_.lines();
    can_send_ = !content_.is_empty() && std::ranges::any_of(lines, [](const std::string& l) { return !utf8::trim(l).empty(); });
    keyword_ = std::ranges::any_of(lines, [](const std::string& l) { return has_keyword(l); });
    for (auto& image : images_) {
        const auto tok = image_token(image.n);
        image.shown = std::ranges::any_of(lines, [&](const std::string& l) { return contains(l, tok); });
    }
}

std::optional<Intent> State::submit() {
    const auto all = content_.text();
    const auto message = std::string(utf8::trim(all));
    if (message.empty()) return std::nullopt;
    content_ = Content();
    text_changed();
    history_.clear();
    popup_.reset();
    dismissed_.reset();
    if (is_busy()) {
        queue_.push(message);
        return queue_changed();
    }
    return send(message);
}

Intent State::send(std::string_view message) {
    auto attachments = resolve(message, [&](std::string_view path) {
        return std::ranges::find(mentions_, path) != mentions_.end() || std::ranges::find(files_, path) != files_.end();
    });
    for (const auto& image : images_) {
        if (!contains(message, image_token(image.n))) continue;
        if (std::ranges::any_of(attachments, [&](const Attachment& a) { return a.path == image.image.path; })) continue;
        Attachment a{image.image.path, std::nullopt};
        if (const auto mime = loa::runtime::text::mime_of(image.image.path)) a.mime = std::string(*mime);
        attachments.push_back(std::move(a));
    }
    if (queue_.items.empty() && content_.is_empty()) {
        mentions_.clear();
        images_.clear();
        next_image_ = 0;
    }
    phase_ = Phase::Starting;
    return intent::Submit{Prompt{std::string(message), std::move(attachments), author_}};
}

std::optional<Intent> State::stop() {
    if (!is_busy()) return std::nullopt;
    unqueue();
    return Intent{intent::Stop{}};
}

void State::unqueue() {
    std::vector<std::string> queued;
    for (auto& m : queue_.items) queued.push_back(std::move(m.text));
    queue_.items.clear();
    if (queued.empty()) return;
    auto before = snapshot();
    if (!utf8::trim(before.text).empty()) queued.push_back(before.text);
    std::string text;
    for (std::size_t i = 0; i < queued.size(); ++i) text += (i ? "\n\n" : "") + queued[i];
    history_.break_group();
    history_.begin(EditKind::other(), std::move(before));
    replace(text);
    popup_.reset();
}

Intent State::queue_changed() const {
    std::vector<std::string> texts;
    for (const auto& m : queue_.items) texts.push_back(m.text);
    return intent::QueueChanged{std::move(texts)};
}

std::optional<Intent> State::accept(std::optional<std::size_t> row) {
    const auto* popup = visible_popup();
    if (!popup) return std::nullopt;
    const auto at_row = row.value_or(popup->selected);
    if (at_row >= popup->matches.size()) return std::nullopt;
    const auto index = popup->matches[at_row];
    const auto trigger = popup->trigger;
    std::string replacement;
    if (trigger.kind == PopupKind::Command) {
        replacement = "/" + commands_[index].name + " ";
    } else {
        const auto path = files_[index];
        replacement = token(path) + " ";
        if (std::ranges::find(mentions_, path) == mentions_.end()) mentions_.push_back(path);
    }
    auto before = snapshot();
    history_.break_group();
    content_.move_to(Cursor{{trigger.line, trigger.end}, Position{trigger.line, trigger.start}});
    content_.perform(action::Edit{edit::Paste{replacement}});
    text_changed();
    history_.begin(EditKind::other(), std::move(before));
    popup_.reset();
    dismissed_.reset();
    return std::nullopt;
}

std::optional<Intent> State::refresh_popup() {
    const auto c = content_.cursor();
    const auto* line = c.selection ? nullptr : content_.line(c.position.line);
    const auto trigger = line ? detect(c.position.line, *line, c.position.column) : std::nullopt;
    if (!trigger) {
        popup_.reset();
        dismissed_.reset();
        return std::nullopt;
    }
    if (dismissed_ && dismissed_->same_anchor(*trigger)) {
        popup_.reset();
        return std::nullopt;
    }
    dismissed_.reset();
    std::optional<Popup> previous;
    if (popup_ && popup_->trigger.same_anchor(*trigger)) previous = std::move(popup_);
    popup_.reset();
    std::optional<Intent> event;
    if (!previous && trigger->kind == PopupKind::File) event = intent::FilesRequested{trigger->query};
    const std::size_t selected = previous && previous->trigger.query == trigger->query ? previous->selected : 0;
    auto found = matches(*trigger);
    const auto last = found.empty() ? 0 : found.size() - 1;
    popup_ = Popup{*trigger, std::move(found), std::min(selected, last)};
    return event;
}

void State::refilter() {
    if (!popup_) return;
    auto found = matches(popup_->trigger);
    popup_->selected = std::min(popup_->selected, found.empty() ? 0 : found.size() - 1);
    popup_->matches = std::move(found);
}

std::vector<std::size_t> State::matches(const Trigger& trigger) const {
    std::vector<std::string_view> names;
    if (trigger.kind == PopupKind::Command)
        for (const auto& c : commands_) names.emplace_back(c.name);
    else
        for (const auto& f : files_) names.emplace_back(f);
    return kit::text::filter(trigger.query, names, MAX_ITEMS);
}

// ---- Free functions --------------------------------------------------------------------------

bool is_image(const std::filesystem::path& path) {
    const auto mime = loa::runtime::text::mime_of(path);
    return mime && mime->starts_with("image/") && *mime != "image/svg+xml";
}

PastedImage pasted(std::filesystem::path path, std::optional<std::pair<std::uint32_t, std::uint32_t>> size) {
    return PastedImage{std::move(path), size};
}

std::string short_model_name(std::string_view name) {
    if (name.starts_with("Claude ")) {
        const auto rest = name.substr(7);
        if (!utf8::trim(rest).empty()) return std::string(rest);
    }
    return std::string(name);
}

std::string capitalized(std::string_view label) {
    std::string out(label);
    if (!out.empty() && out[0] >= 'a' && out[0] <= 'z') out[0] = static_cast<char>(out[0] - 'a' + 'A');
    return out;
}

} // namespace loa::ui::session::composer
