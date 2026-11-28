#include "loa/ui/session/transcript/rows.hpp"

#include <algorithm>
#include <array>
#include <cmath>
#include <limits>

#include <QFrame>
#include <QHBoxLayout>
#include <QLabel>
#include <QPushButton>
#include <QVBoxLayout>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/text.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/session/transcript/view.hpp"

namespace loa::ui::session::transcript {

namespace core = loa::core;
namespace attr = loa::core::attr;
namespace kit = loa::ui::kit;
using core::ElementKind;
using kit::ButtonKind;
using kit::Icon;
using kit::Tone;
using kit::theme::SpaceRole;
using kit::theme::TextRole;

// ---- Plain helpers -------------------------------------------------------------------------

std::string_view word(Status s) {
    static constexpr std::array<std::string_view, 4> WORDS = {"running\u2026", "done", "failed", "cancelled"};
    return WORDS[static_cast<std::size_t>(s)];
}

namespace {

struct Verb {
    std::vector<std::string_view> names;
    std::string_view verb, one, many;
};

const std::vector<Verb>& verbs() {
    static const std::vector<Verb> VERBS = {
        {{"read", "read_file", "view"}, "Read", "file", "files"},
        {{"edit", "multiedit", "apply_patch", "ast_edit"}, "Edited", "file", "files"},
        {{"write", "write_file"}, "Wrote", "file", "files"},
        {{"bash", "shell", "exec", "exec_command"}, "Ran", "command", "commands"},
        {{"grep", "search"}, "Searched", "pattern", "patterns"},
        {{"glob", "find", "ls"}, "Listed", "pattern", "patterns"},
        {{"fetch", "webfetch", "web_fetch"}, "Fetched", "page", "pages"},
        {{"websearch", "web_search"}, "Searched the web for", "query", "queries"},
        {{"task", "agent"}, "Ran", "agent", "agents"},
    };
    return VERBS;
}

bool iequals(std::string_view a, std::string_view b) {
    return a.size() == b.size() && std::equal(a.begin(), a.end(), b.begin(), [](char x, char y) {
               const auto lower = [](char c) { return c >= 'A' && c <= 'Z' ? static_cast<char>(c - 'A' + 'a') : c; };
               return lower(x) == lower(y);
           });
}

template <class E> std::optional<E> attr_enum(const core::Node& n, std::string_view name) {
    return n.str_attr(name).and_then([](std::string_view s) { return core::parse<E>(s); });
}

bool is_failed(const core::Dom& dom, const core::Node& call) {
    if (attr_enum<core::CallStatus>(call, attr::STATUS) == core::CallStatus::Error) return true;
    for (const auto* r : dom.children_of_kind(call.id, ElementKind::Result))
        if (r->bool_attr(attr::IS_ERROR) == true) return true;
    return false;
}

bool continuation(char c) { return (static_cast<unsigned char>(c) & 0xC0) == 0x80; }

float text_lines(const core::Node& node) { return static_cast<float>(node.text.size() / CHARS_PER_LINE + 1); }

} // namespace

std::tuple<std::string_view, std::string_view, std::string_view> tool_words(std::string_view tool) {
    for (const auto& v : verbs())
        for (const auto n : v.names)
            if (iequals(tool, n)) return {v.verb, v.one, v.many};
    return {tool, "call", "calls"};
}

CallSummary summarize(const core::Dom& dom, const std::vector<NodeId>& calls) {
    std::vector<const core::Node*> nodes;
    for (const auto id : calls)
        if (const auto* n = dom.get(id)) nodes.push_back(n);
    const auto tool = nodes.empty() ? std::string_view() : nodes.front()->str_attr(attr::TOOL).value_or("");
    const auto [verb, one, many] = tool_words(tool);
    const auto title = nodes.empty() ? std::nullopt : nodes.front()->str_attr(attr::TITLE);
    std::string label;
    if (title && nodes.size() == 1) label = std::string(verb) + " " + std::string(*title);
    else if (nodes.size() == 1 && verb == tool) label = std::string(tool);
    else label = std::string(verb) + " " + std::to_string(nodes.size()) + " " + std::string(nodes.size() == 1 ? one : many);
    bool running = false, timed = true;
    std::size_t cancelled = 0, failed = 0;
    std::int64_t lo = std::numeric_limits<std::int64_t>::max(), hi = std::numeric_limits<std::int64_t>::min();
    for (const auto* call : nodes) {
        const auto status = attr_enum<core::CallStatus>(*call, attr::STATUS);
        if (!status || *status == core::CallStatus::Pending || *status == core::CallStatus::Running) running = true;
        else if (*status == core::CallStatus::Cancelled) ++cancelled;
        failed += is_failed(dom, *call) ? 1 : 0;
        const auto s = call->int_attr(attr::STARTED), e = call->int_attr(attr::ENDED);
        if (s && e) {
            lo = std::min(lo, *s);
            hi = std::max(hi, *e);
        } else {
            timed = false;
        }
    }
    Status status = Status::Done;
    if (running) status = Status::Running;
    else if (failed > 0) status = Status::Error;
    else if (cancelled > 0 && cancelled == nodes.size()) status = Status::Cancelled;
    std::optional<std::int64_t> duration;
    if (timed && !running && !nodes.empty()) duration = std::max<std::int64_t>(hi - lo, 0);
    return {std::move(label), status, duration, failed};
}

std::pair<std::string_view, std::size_t> leading(std::string_view text) {
    auto end = std::min(MAX_BYTES, text.size());
    while (end > 0 && end < text.size() && continuation(text[end])) --end;
    std::size_t seen = 0;
    for (std::size_t i = 0; i < end; ++i)
        if (text[i] == '\n' && ++seen == MAX_LINES) {
            end = i;
            break;
        }
    return {text.substr(0, end), text.size() - end};
}

std::pair<std::string_view, std::size_t> trailing(std::string_view text) {
    auto start = text.size() > MAX_BYTES ? text.size() - MAX_BYTES : 0;
    while (start < text.size() && continuation(text[start])) ++start;
    auto rest = text.substr(start);
    while (rest.ends_with('\n')) rest.remove_suffix(1);
    std::size_t seen = 0;
    for (auto i = rest.size(); i-- > 0;)
        if (rest[i] == '\n' && ++seen == MAX_LINES) {
            start += i + 1;
            break;
        }
    return {text.substr(start), start};
}

std::string thinking_label(const core::Node& node) {
    return attr_enum<core::MessageStatus>(node, attr::STATUS) == core::MessageStatus::Streaming ? "Thinking\u2026" : "Thought";
}

std::string compaction_label(const core::Node& node) {
    const auto* covers = node.attr(attr::COVERS);
    const auto list = covers ? covers->as_list() : std::nullopt;
    const auto n = list ? list->size() : 0;
    if (n == 0) return "Conversation compacted";
    if (n == 1) return "Compacted 1 turn";
    return "Compacted " + std::to_string(n) + " turns";
}

float RowRenderer::estimate(const Item& item, const RowCtx& cx) const {
    return cx.expanded ? ROW + static_cast<float>(std::max<std::size_t>(item.calls.size(), 1)) * 12.0F * LINE : ROW;
}

// ---- Widgets -------------------------------------------------------------------------------

namespace {

int spi(SpaceRole r) { return static_cast<int>(std::lround(kit::theme::sp(r))); }
QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }

QLabel* label(std::string_view s, TextRole role, Tone tone, bool wrap = false) {
    auto* l = kit::widgets::label(s, role, tone);
    l->setWordWrap(wrap);
    l->setTextFormat(Qt::PlainText);
    return l;
}
QLabel* muted(std::string_view s, TextRole role = TextRole::Body, bool wrap = false) { return label(s, role, Tone::Muted, wrap); }
QLabel* danger(std::string_view s, TextRole role = TextRole::Body) { return label(s, role, Tone::Danger); }

QLabel* glyph(Icon icon, bool failed) {
    auto* l = new QLabel;
    l->setPixmap(kit::tinted(icon, TextRole::Body, failed ? Tone::Danger : Tone::Muted));
    l->setAccessibleName(qs(kit::icon_file(icon)));
    return l;
}

QWidget* column(const std::vector<QWidget*>& children, int spacing) {
    auto* w = new QWidget;
    auto* box = new QVBoxLayout(w);
    box->setContentsMargins(0, 0, 0, 0);
    box->setSpacing(spacing);
    for (auto* c : children)
        if (c) box->addWidget(c);
    return w;
}

QHBoxLayout* hbox(const std::vector<QWidget*>& parts) {
    auto* line = new QHBoxLayout;
    line->setSpacing(spi(SpaceRole::S));
    for (auto* p : parts) line->addWidget(p);
    line->addStretch(1);
    return line;
}

// A header that toggles the row: `parts` in a line, then a chevron. Expanded, `body` follows
// behind a thin guide.
QWidget* foldable(const std::vector<QWidget*>& parts, std::string_view name, NodeId key, QWidget* body, const Emit& post) {
    auto* head = new QPushButton;
    auto all = parts;
    auto* chevron = new QLabel;
    chevron->setPixmap(kit::muted(body ? Icon::ChevronDown : Icon::ChevronRight, kit::theme::text_size(TextRole::Caption)));
    all.push_back(chevron);
    auto* line = hbox(all);
    line->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::Xs), spi(SpaceRole::M), spi(SpaceRole::Xs));
    head->setLayout(line);
    head->setMinimumHeight(line->sizeHint().height());
    head->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
    head->setAccessibleName(qs(name));
    kit::qt::style_button(*head, ButtonKind::row(false));
    QObject::connect(head, &QPushButton::clicked, head, [post, key] { post(msg::Toggle{key}); });
    if (!body) return head;
    auto* rest = new QWidget;
    auto* row = new QHBoxLayout(rest);
    row->setContentsMargins(spi(SpaceRole::L) + 1, 0, 0, spi(SpaceRole::Xs));
    row->setSpacing(spi(SpaceRole::L));
    auto* guide = new QFrame;
    guide->setFrameShape(QFrame::VLine);
    guide->setAccessibleName(QStringLiteral("guide"));
    row->addWidget(guide);
    row->addWidget(body, 1);
    return column({head, rest}, spi(SpaceRole::Xs));
}

// A monospace well of `body` with the bytes left out noted below.
QWidget* block(std::string_view body, std::size_t left_out, bool error) {
    auto* well = new QFrame;
    well->setObjectName(QStringLiteral("transcript-block"));
    well->setAccessibleName(QStringLiteral("output"));
    kit::qt::style_container(*well, kit::Surface::Well);
    auto* col = new QVBoxLayout(well);
    col->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
    col->setSpacing(spi(SpaceRole::Xs));
    auto* text = label(body, TextRole::Mono, error ? Tone::Danger : Tone::Normal, true);
    text->setTextInteractionFlags(Qt::TextSelectableByMouse);
    col->addWidget(text);
    if (left_out > 0) col->addWidget(muted("\u2026 " + kit::text::format_bytes(left_out) + " more", TextRole::Label));
    return well;
}

QWidget* inline_row(const std::vector<QWidget*>& parts) {
    auto* w = new QWidget;
    auto* line = hbox(parts);
    line->setContentsMargins(spi(SpaceRole::M), 0, spi(SpaceRole::M), 0);
    w->setLayout(line);
    return w;
}

QLabel* mono(std::string_view s) {
    auto* l = label(s, TextRole::Label, Tone::Normal);
    auto f = kit::qt::font(TextRole::Mono);
    f.setPixelSize(static_cast<int>(std::lround(kit::theme::text_size(TextRole::Label))));
    l->setFont(f);
    return l;
}

class User final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx& cx, const Emit& post) const override {
        const std::string value = cx.node->text;
        auto* bubble = new QFrame;
        bubble->setObjectName(QStringLiteral("transcript-user"));
        bubble->setAccessibleName(QStringLiteral("user message"));
        kit::qt::style_container(*bubble, kit::Surface::Raised);
        auto* inner = new QVBoxLayout(bubble);
        inner->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
        inner->addWidget(selectable(value, post));
        auto* actions = new QHBoxLayout;
        actions->setSpacing(spi(SpaceRole::Xs));
        actions->addStretch(1);
        if (!cx.read_only) {
            const auto node = cx.node->id;
            auto* rewind = kit::widgets::text_button("Rewind", ButtonKind::row(false), [post, node] { post(msg::Rewind{node}); });
            actions->addWidget(rewind);
        }
        actions->addWidget(new CopyButton("Copy message", [value] { return value; }, post));
        auto* message = new QWidget;
        message->setMaximumWidth(static_cast<int>(kit::theme::text_size(TextRole::Body) * 40.0F));
        auto* col = new QVBoxLayout(message);
        col->setContentsMargins(0, 0, 0, 0);
        col->setSpacing(spi(SpaceRole::Xs));
        col->addWidget(bubble, 0, Qt::AlignRight);
        col->addLayout(actions);
        auto* outer = new QWidget;
        auto* row = new QHBoxLayout(outer);
        row->setContentsMargins(0, 0, 0, 0);
        row->addStretch(1);
        row->addWidget(message);
        return outer;
    }
    float estimate(const Item&, const RowCtx& cx) const override { return LINE * text_lines(*cx.node) + PADDING; }
};

class System final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx& cx, const Emit&) const override { return muted(cx.node->text, TextRole::Label, true); }
    float estimate(const Item&, const RowCtx& cx) const override { return LINE * text_lines(*cx.node) + PADDING; }
};

class Assistant final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx& cx, const Emit& post) const override {
        const auto status = attr_enum<core::MessageStatus>(*cx.node, attr::STATUS);
        std::vector<QWidget*> parts;
        parts.push_back(cx.streamed ? prose_widget(cx.streamed->blocks(), cx.node->text, post) : selectable(cx.node->text, post));
        if (status == core::MessageStatus::Cancelled) parts.push_back(muted("[interrupted]"));
        if (!cx.node->text.empty()) {
            auto* copy = new CopyButton("Copy message", [value = cx.node->text] { return value; }, post);
            auto* line = new QWidget;
            auto* h = new QHBoxLayout(line);
            h->setContentsMargins(0, 0, 0, 0);
            h->addWidget(copy);
            h->addStretch(1);
            parts.push_back(line);
        }
        if (status == core::MessageStatus::Error) parts.push_back(danger("Failed", TextRole::Label));
        return column(parts, spi(SpaceRole::Xs));
    }
    float estimate(const Item&, const RowCtx& cx) const override {
        // Constant time for a drawn reply, so streaming can refresh it on every append.
        if (cx.streamed) return LINE * static_cast<float>(cx.streamed->lines() + cx.streamed->fed() / CHARS_PER_LINE + 1);
        const auto& t = cx.node->text;
        return LINE * static_cast<float>(static_cast<std::size_t>(std::count(t.begin(), t.end(), '\n')) + t.size() / CHARS_PER_LINE + 1);
    }
};

// A collapsed note whose text shows when expanded: thinking and compactions.
class Note final : public RowRenderer {
  public:
    Note(std::string (*label)(const core::Node&), bool italic) : label_(label), italic_(italic) {}
    QWidget* view(const Item&, const RowCtx& cx, const Emit& post) const override {
        const auto text = label_(*cx.node);
        auto* head = muted(text);
        auto font = head->font();
        font.setItalic(italic_);
        head->setFont(font);
        QWidget* body = nullptr;
        if (cx.expanded) {
            auto* b = muted(cx.node->text, TextRole::Label, true);
            b->setFont([&] {
                auto f = b->font();
                f.setItalic(italic_);
                return f;
            }());
            body = b;
        }
        return foldable({head}, text, cx.node->id, body, post);
    }
    float estimate(const Item&, const RowCtx& cx) const override { return cx.expanded ? ROW + LINE * text_lines(*cx.node) : ROW; }

  private:
    std::string (*label_)(const core::Node&);
    bool italic_;
};

QWidget* call_details(const core::Dom& dom, const core::Node& call) {
    const auto tool = call.str_attr(attr::TOOL).value_or("");
    const auto title = call.str_attr(attr::TITLE);
    const auto line = title ? std::string(std::get<0>(tool_words(tool))) + " " + std::string(*title) : std::string(tool);
    std::vector<QWidget*> parts{label(line, TextRole::Label, Tone::Normal)};
    for (const auto* part : dom.children(call.id)) {
        if (part->kind != ElementKind::Input && part->kind != ElementKind::Result) continue;
        const bool error = part->bool_attr(attr::IS_ERROR) == true;
        QLabel* head = nullptr;
        if (part->kind == ElementKind::Input) head = muted("Input", TextRole::Caption);
        else if (error) head = danger("Error", TextRole::Caption);
        else head = muted("Result", TextRole::Caption);
        const auto [shown, left_out] = leading(part->text);
        parts.push_back(head);
        parts.push_back(block(shown, left_out, error));
    }
    const bool silent = attr_enum<core::CallStatus>(call, attr::STATUS) == core::CallStatus::Error &&
                        dom.children_of_kind(call.id, ElementKind::Result).empty();
    if (silent) parts.push_back(danger("Failed", TextRole::Label));
    return column(parts, spi(SpaceRole::Xs));
}

class Calls final : public RowRenderer {
  public:
    QWidget* view(const Item& item, const RowCtx& cx, const Emit& post) const override {
        const auto s = summarize(*cx.dom, item.calls);
        std::vector<QWidget*> parts{glyph(Icon::FileText, s.status == Status::Error), muted(s.label)};
        if (s.status == Status::Running) parts.push_back(muted(word(Status::Running)));
        else if (s.duration_ms) parts.push_back(muted(kit::text::format_duration(*s.duration_ms)));
        if (s.failed > 0) parts.push_back(danger(std::to_string(s.failed) + " failed"));
        if (s.status == Status::Cancelled) parts.push_back(muted("cancelled"));
        QWidget* body = nullptr;
        if (cx.expanded) {
            std::vector<QWidget*> calls;
            for (const auto id : item.calls)
                if (const auto* c = cx.dom->get(id)) calls.push_back(call_details(*cx.dom, *c));
            body = column(calls, spi(SpaceRole::S));
        }
        return foldable(parts, s.label, item.key, body, post);
    }
};

class Job final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx& cx, const Emit& post) const override {
        const auto& node = *cx.node;
        const bool agent = attr_enum<core::JobKind>(node, attr::KIND) == core::JobKind::Subagent;
        std::string_view name;
        for (const auto a : {attr::TITLE, attr::COMMAND, attr::AGENT})
            if (const auto v = node.str_attr(a)) {
                name = *v;
                break;
            }
        Status status = Status::Running;
        switch (attr_enum<core::JobStatus>(node, attr::STATUS).value_or(core::JobStatus::Running)) {
        case core::JobStatus::Running: status = Status::Running; break;
        case core::JobStatus::Done: status = Status::Done; break;
        case core::JobStatus::Error: status = Status::Error; break;
        case core::JobStatus::Cancelled: status = Status::Cancelled; break;
        }
        const bool failed = status == Status::Error;
        std::vector<QWidget*> parts{glyph(agent ? Icon::Cpu : Icon::Terminal, failed), muted(agent ? "Agent" : "Shell"), mono(name),
                                    muted(word(status))};
        if (const auto code = node.int_attr(attr::EXIT_CODE); code && *code != 0) parts.push_back(danger("exit " + std::to_string(*code)));
        QWidget* body = nullptr;
        if (cx.expanded && !node.text.empty()) {
            const auto [shown, left_out] = trailing(node.text);
            body = column({left_out > 0 ? muted("\u2026 " + kit::text::format_bytes(left_out) + " more", TextRole::Label) : nullptr,
                           block(shown, 0, failed)},
                          spi(SpaceRole::Xs));
        }
        return foldable(parts, name, node.id, body, post);
    }
};

class Todo final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx& cx, const Emit&) const override {
        std::string_view mark = "\u2610";
        bool done = false;
        switch (attr_enum<core::TodoState>(*cx.node, attr::STATE).value_or(core::TodoState::Pending)) {
        case core::TodoState::Pending: break;
        case core::TodoState::InProgress: mark = "\u25d0"; break;
        case core::TodoState::Done: mark = "\u2611", done = true; break;
        case core::TodoState::Cancelled: mark = "\u2612", done = true; break;
        }
        const auto tone = done ? Tone::Muted : Tone::Normal;
        return inline_row({label(mark, TextRole::Body, tone), label(cx.node->text, TextRole::Body, tone)});
    }
    float estimate(const Item&, const RowCtx&) const override { return LINE; }
};

class SurfaceRow final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx& cx, const Emit&) const override {
        const auto name = cx.node->str_attr(attr::NAME).or_else([&] { return cx.node->str_attr(attr::URI); }).value_or("");
        return inline_row({muted("Surface"), mono(name), muted(cx.node->str_attr(attr::MODE).value_or(""))});
    }
    float estimate(const Item&, const RowCtx&) const override { return LINE; }
};

class Ext final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx& cx, const Emit& post) const override {
        const auto& node = *cx.node;
        const auto harness = node.str_attr(attr::HARNESS).value_or("");
        const auto type = node.str_attr(attr::TYPE).value_or("event");
        std::vector<QWidget*> parts{glyph(Icon::Wrench, false), muted(harness), muted(type)};
        const std::string_view raw = node.text.empty() ? node.str_attr(attr::RAW).value_or("") : std::string_view(node.text);
        QWidget* body = nullptr;
        if (cx.expanded) {
            const auto [shown, left_out] = leading(raw);
            body = block(shown, left_out, false);
        }
        return foldable(parts, std::string(harness) + " " + std::string(type), node.id, body, post);
    }
};

// A decision: the app fills the slot with its card, so this is never drawn.
class SlotRow final : public RowRenderer {
  public:
    QWidget* view(const Item&, const RowCtx&, const Emit&) const override { return new QWidget; }
    float estimate(const Item&, const RowCtx&) const override { return 6.0F * LINE; }
};

} // namespace

const RowRenderer& renderer(ItemKind kind) {
    static const User user;
    static const Assistant assistant;
    static const Note thinking(thinking_label, true);
    static const System system;
    static const Calls calls;
    static const SlotRow slot;
    static const Job job;
    static const Todo todo;
    static const SurfaceRow surface;
    static const Note compaction(compaction_label, false);
    static const Ext ext;
    static const std::array<const RowRenderer*, 11> ROWS = {&user, &assistant, &thinking, &system, &calls, &slot,
                                                           &job,  &todo,      &surface,  &compaction, &ext};
    return *ROWS[static_cast<std::size_t>(kind)];
}

} // namespace loa::ui::session::transcript
