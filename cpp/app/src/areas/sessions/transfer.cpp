// The import and export screen: native sessions found on this machine, a path field for any
// session file, and export of the active session as a native file or as debugging XML (port of
// `loa-app/src/areas/sessions/transfer.rs`).

#include "sessions.hpp"

#include <algorithm>
#include <system_error>

#include <QHBoxLayout>
#include <QLineEdit>
#include <QScrollArea>
#include <QVBoxLayout>
#include <QWidget>

#include "loa/harness/native.hpp"
#include "loa/runtime/time.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::app::areas::sessions {

namespace {

namespace native = loa::harness::native;
namespace kit = loa::ui::kit;
namespace widgets = loa::ui::kit::widgets;
namespace theme = loa::ui::kit::theme;
using kit::Tone;
using theme::LayoutRole;
using theme::SpaceRole;
using theme::TextRole;

std::string_view trim(std::string_view text) {
    const auto first = text.find_first_not_of(" \t\r\n");
    if (first == std::string_view::npos) return {};
    const auto last = text.find_last_not_of(" \t\r\n");
    return text.substr(first, last - first + 1);
}

QWidget* column(const std::vector<QWidget*>& items, SpaceRole spacing) {
    auto* box = new QWidget;
    auto* col = new QVBoxLayout(box);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(static_cast<int>(theme::sp(spacing)));
    for (auto* item : items)
        if (item) col->addWidget(item);
    return box;
}

QWidget* row(const std::vector<std::pair<QWidget*, int>>& items, SpaceRole spacing) {
    auto* box = new QWidget;
    auto* line = new QHBoxLayout(box);
    line->setContentsMargins(0, 0, 0, 0);
    line->setSpacing(static_cast<int>(theme::sp(spacing)));
    for (const auto& [item, stretch] : items) line->addWidget(item, stretch);
    return box;
}

QLineEdit* input(std::string_view placeholder, const std::string& value, std::string_view name,
                 std::function<void(std::string)> on, std::function<void()> submit = {}) {
    auto* field = new QLineEdit;
    field->setAccessibleName(QString::fromUtf8(name.data(), static_cast<int>(name.size())));
    field->setObjectName(field->accessibleName());
    field->setPlaceholderText(QString::fromUtf8(placeholder.data(), static_cast<int>(placeholder.size())));
    field->setFont(kit::qt::font(TextRole::Body));
    field->setText(QString::fromStdString(value));
    QObject::connect(field, &QLineEdit::textEdited, field, [on = std::move(on)](const QString& text) { on(text.toStdString()); });
    if (submit) QObject::connect(field, &QLineEdit::returnPressed, field, std::move(submit));
    return field;
}

QWidget* hint(std::string_view text) {
    auto* label = widgets::label(text, TextRole::Caption, Tone::Muted);
    label->setWordWrap(true);
    return label;
}

std::string_view harness_label(std::string_view harness) {
    if (harness == native::OMP) return "omp";
    if (harness == native::CLAUDE) return "Claude Code";
    if (harness == native::CODEX) return "Codex";
    if (harness == native::OPENCODE) return "OpenCode";
    return harness;
}

QWidget* found_row(const loa::harness::kit::NativeSession& session, std::int64_t now, const std::function<void(transfer::Message)>& send) {
    const std::string name = session.title ? *session.title : session.path.filename().string();
    const auto when = loa::runtime::time::ago(now, session.modified);
    const std::string detail = session.project ? *session.project + " · " + when : when;
    auto* text = column({widgets::label(name, TextRole::Body, Tone::Normal), widgets::label(detail, TextRole::Caption, Tone::Muted)},
                        SpaceRole::Xs);
    auto* import = widgets::text_button("Import", kit::ButtonKind::secondary(),
                                        [send, path = session.path] { send(transfer::msg::Import{path}); });
    import->setAccessibleName(QString::fromStdString("Import " + name));
    auto* line = row({{widgets::badge(harness_label(session.harness), Tone::Normal), 0}, {text, 1}, {import, 0}}, SpaceRole::S);
    const int pad = static_cast<int>(theme::sp(SpaceRole::Xs));
    line->layout()->setContentsMargins(pad, pad, pad, pad);
    return line;
}

} // namespace

namespace transfer {

QWidget* view(const State& state, std::optional<std::pair<std::string_view, std::string_view>> active, std::int64_t now,
              const std::function<void(Message)>& send) {
    auto heading = [](std::string_view s) { return widgets::label(s, TextRole::Heading, Tone::Normal); };
    auto* header = widgets::section_header(
        "Import and export sessions",
        {widgets::text_button("Rescan", kit::ButtonKind::secondary(), [send] { send(msg::Refresh{}); }),
         widgets::text_button("Done", kit::ButtonKind::secondary(), [send] { send(msg::Close{}); })});

    const bool importable = !trim(state.path).empty();
    auto* import = widgets::text_button("Import", kit::ButtonKind::primary(),
                                        importable ? widgets::Callback([send] { send(msg::ImportPath{}); }) : widgets::Callback());
    auto* path = input("/path/to/session.jsonl", state.path, "import path", [send](std::string p) { send(msg::Path{std::move(p)}); },
                       [send] { send(msg::ImportPath{}); });
    auto* picker = column({heading("Import a session file"),
                           hint("An omp, Claude Code or Codex JSONL session, or a file `opencode export <id>` wrote."),
                           row({{path, 1}, {import, 0}}, SpaceRole::S)},
                          SpaceRole::S);

    const std::string found_title = state.found.empty()
                                        ? std::string("No native sessions found in ~/.omp, ~/.claude or ~/.codex")
                                        : "Sessions on this machine (" + std::to_string(state.found.size()) + ")";
    std::vector<QWidget*> rows;
    for (std::size_t i = 0; i < std::min(state.found.size(), LISTED); ++i) rows.push_back(found_row(state.found[i], now, send));
    auto* list = new QScrollArea;
    list->setAccessibleName(QStringLiteral("native sessions"));
    list->setObjectName(list->accessibleName());
    list->setWidgetResizable(true);
    list->setFrameShape(QFrame::NoFrame);
    list->setWidget(column(rows, SpaceRole::Xs));
    list->setFixedHeight(static_cast<int>(theme::width(LayoutRole::ListWidth)));
    std::vector<QWidget*> found_items{heading(found_title), list};
    if (state.found.size() > LISTED)
        found_items.push_back(hint(std::to_string(state.found.size() - LISTED) +
                                   " older sessions are not listed; import them by path."));
    auto* found = column(found_items, SpaceRole::S);

    QWidget* exporting = nullptr;
    if (active) {
        const auto [id, harness] = *active;
        auto* field = input("Export path", state.export_path, "export path", [send](std::string p) { send(msg::ExportPath{std::move(p)}); });
        auto* native_button = widgets::text_button("Export " + std::string(harness) + " file", kit::ButtonKind::secondary(),
                                                   [send] { send(msg::Export{false}); });
        auto* xml_button = widgets::text_button("Export XML (debug)", kit::ButtonKind::secondary(), [send] { send(msg::Export{true}); });
        exporting = column({heading("Export this session"),
                            hint(std::string(id) + " on " + std::string(harness) +
                                 ". Leave the path empty to write into the app's exports folder."),
                            row({{field, 1}, {native_button, 0}, {xml_button, 0}}, SpaceRole::S)},
                           SpaceRole::S);
    } else {
        exporting = widgets::label("Open a session to export it.", TextRole::Label, Tone::Muted);
    }

    std::vector<QWidget*> items{header, widgets::card(picker), widgets::card(found), widgets::card(exporting)};
    if (state.status) items.push_back(hint(*state.status));
    auto* body = column(items, SpaceRole::L);
    body->setMaximumWidth(static_cast<int>(theme::width(LayoutRole::ContentWidth)));

    auto* page = new QWidget;
    auto* centered = new QHBoxLayout(page);
    const int pad = static_cast<int>(theme::sp(SpaceRole::Xl));
    centered->setContentsMargins(pad, pad, pad, pad);
    centered->addStretch(1);
    centered->addWidget(body, 0, Qt::AlignTop);
    centered->addStretch(1);
    auto* scroll = new QScrollArea;
    scroll->setAccessibleName(QStringLiteral("Import and export sessions"));
    scroll->setObjectName(scroll->accessibleName());
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setWidget(page);
    return scroll;
}

} // namespace transfer

void State::show_import(Ctx& ctx) {
    transfer.found = native::discover(home_);
    transfer.status.reset();
    ctx.show_screen(ID);
}

void State::transfer_update(Ctx& ctx, transfer::Message message) {
    namespace tm = transfer::msg;
    if (auto* m = std::get_if<tm::Path>(&message)) {
        transfer.path = std::move(m->path);
    } else if (auto* e = std::get_if<tm::ExportPath>(&message)) {
        transfer.export_path = std::move(e->path);
    } else if (std::holds_alternative<tm::Refresh>(message)) {
        transfer.found = native::discover(home_);
    } else if (std::holds_alternative<tm::Close>(message)) {
        ctx.close_screen();
    } else if (std::holds_alternative<tm::ImportPath>(message)) {
        import(ctx, std::filesystem::path(std::string(trim(transfer.path))));
    } else if (auto* i = std::get_if<tm::Import>(&message)) {
        import(ctx, i->path);
    } else if (auto* x = std::get_if<tm::Export>(&message)) {
        auto wanted = trim(transfer.export_path);
        std::optional<std::filesystem::path> path;
        if (!wanted.empty()) path = std::filesystem::path(std::string(wanted));
        export_session(ctx, x->xml, std::move(path));
    }
}

void State::import(Ctx& ctx, const std::filesystem::path& path) {
    const auto& runtime = ctx.runtime;
    auto imported = [&]() -> std::expected<std::string, std::string> {
        auto harness = native::harness_of(path);
        if (!harness) return std::unexpected(path.string() + " isn't a session file of a known harness");
        auto adapter = runtime.harnesses().registry.get(*harness);
        if (!adapter || !adapter->capabilities().import)
            return std::unexpected("This app can't import " + std::string(*harness) + " sessions");
        auto id = app_runtime::new_session_id();
        auto live = runtime.library()->import(*adapter, path, id, ctx.services.node.key);
        if (!live) return std::unexpected(live.error().to_string());
        return id;
    }();
    if (imported) {
        transfer.status = "Imported " + path.string();
        ctx.request(request::OpenSession{std::move(*imported)});
    } else {
        transfer.status = "Can't import: " + imported.error();
        ctx.notify("Can't import: " + imported.error());
    }
}

void State::export_session(Ctx& ctx, bool xml, std::optional<std::filesystem::path> wanted) {
    const auto* session = ctx.runtime.sessions().active();
    if (!session) {
        ctx.notify("Open a session to export it");
        return;
    }
    const std::string id = session->id().str();
    const std::string harness(session->harness_id().value_or(""));
    const std::string_view extension = xml ? "xml" : harness == native::OPENCODE ? "json" : "jsonl";
    const auto path = wanted ? std::move(*wanted)
                             : ctx.services.storage.dirs.data / "exports" / (id + "." + std::string(extension));
    const auto& runtime = ctx.runtime;
    const auto& library = *runtime.library();
    auto result = [&]() -> std::expected<void, std::string> {
        if (const auto dir = path.parent_path(); !dir.empty()) {
            std::error_code error;
            std::filesystem::create_directories(dir, error);
            if (error) return std::unexpected(dir.string() + ": " + error.message());
        }
        if (xml) {
            auto written = library.export_xml(id, path);
            if (!written) return std::unexpected(written.error().to_string());
            return {};
        }
        auto adapter = runtime.harnesses().registry.get(harness);
        if (!adapter || !adapter->capabilities().export_)
            return std::unexpected(std::string(runtime.harnesses().label(harness)) + " sessions can't be exported");
        auto written = library.export_(*adapter, id, path);
        if (!written) return std::unexpected(written.error().to_string());
        return {};
    }();
    auto status = result ? "Exported to " + path.string() : "Can't export: " + result.error();
    transfer.status = status;
    ctx.notify(std::move(status));
}

} // namespace loa::app::areas::sessions
