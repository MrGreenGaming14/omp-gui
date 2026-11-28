// The MCP servers section. Port of `crates/loa-ui-panel/src/tools/view/servers.rs`.

#include <format>

#include <QCheckBox>

#include "view.hpp"

namespace loa::ui::panel::tools::view {

using namespace pw;

namespace {

QWidget* server_view(const State& s, const ServerView& view, const Emit& send) {
    const auto& srv = *view.server;
    const std::string& name = srv.name;
    const std::string key = "server:" + name;
    const bool open = s.block_open(key), filtering = !s.filter().empty();
    Tone tone = Tone::Muted;
    std::string status;
    if (std::holds_alternative<server_status::Running>(srv.status)) tone = Tone::Ok, status = "running";
    else if (std::holds_alternative<server_status::Starting>(srv.status)) status = "starting";
    else if (std::holds_alternative<server_status::Stopped>(srv.status)) status = "stopped";
    else tone = Tone::Danger, status = "error: " + std::get<server_status::Error>(srv.status).message;
    static constexpr std::array<std::string_view, 3> KINDS{"", "surface", "app"};
    const auto kind = KINDS[static_cast<std::size_t>(srv.kind)];

    auto* status_label = kw::label(status, TextRole::Caption, tone);
    status_label->setAlignment(Qt::AlignRight | Qt::AlignVCenter);
    auto* title = hrow({status_dot(tone), bold(name, TextRole::Label), kw::label(kind, TextRole::Caption, Tone::Faint)}, spi(SpaceRole::S));
    add(title, status_label, 1);
    add(title, kw::label(std::to_string(srv.tools.size()), TextRole::Caption, Tone::Faint));
    if (!filtering) {
        auto* chevron = new QLabel;
        chevron->setPixmap(kit::tinted(open ? Icon::ChevronUp : Icon::ChevronDown, TextRole::Caption, Tone::Muted));
        add(title, chevron);
    }
    auto* title_button = row_button(title, name, filtering ? std::function<void()>{} : on(send, msg::BlockOpen{key, !open}));

    const bool running = std::holds_alternative<server_status::Running>(srv.status) || std::holds_alternative<server_status::Starting>(srv.status);
    auto act = [&](std::string_view l, ServerAction a, bool enabled) {
        return ghost(l, std::nullopt, enabled ? on(send, msg::Server{name, a}) : std::function<void()>{});
    };
    auto* toggle = ghost(std::format("Tools: {}", label(view.toggle)), std::nullopt,
                         s.project() ? on(send, msg::ServerToggled{name}) : std::function<void()>{});
    auto* controls = hrow({toggle}, spi(SpaceRole::Xs) / 2);
    // App hosted servers run with the app; only their tools toggle.
    if (srv.kind != ServerKind::AppHosted) {
        add(controls, act("Start", ServerAction::Start, !running));
        add(controls, act("Stop", ServerAction::Stop, running));
        add(controls, act("Restart", ServerAction::Restart, running));
        auto* remove = act("Remove", ServerAction::Remove, true);
        kit::qt::style_button(*remove, ButtonKind::danger());
        add(controls, remove);
    }
    stretch(controls);
    std::string transport;
    if (const auto* st = std::get_if<loa::contracts::McpStdio>(&srv.transport)) {
        transport = st->command;
        for (const auto& a : st->args) transport += " " + a;
    } else {
        transport = std::get<loa::contracts::McpHttp>(srv.transport).url;
    }
    auto* transport_label = kw::label(transport, TextRole::Mono, Tone::Faint);
    transport_label->setWordWrap(true);
    auto* below = vcol({transport_label, controls}, spi(SpaceRole::Xs) / 2);
    below->layout()->setContentsMargins(spi(SpaceRole::M), 0, spi(SpaceRole::Xs), spi(SpaceRole::Xs));
    std::vector<QWidget*> rows{vcol({title_button, below})};
    if (open) {
        if (view.tools.empty()) rows.push_back(note_row("No tools declared"));
        else for (auto* r : tool_rows(s, key, view.tools, send)) rows.push_back(r);
    }
    return card(rows);
}

QWidget* import_view(const State& s, const Emit& send) {
    if (!s.imports()) return nullptr;
    const auto& entries = *s.imports();
    auto* header = hrow({bold("Import from .claude, .cursor, .codex", TextRole::Label)});
    stretch(header);
    add(header, ghost("Close", Icon::X, on(send, msg::ImportClose{})));
    header->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::Xs), spi(SpaceRole::Xs) / 2, spi(SpaceRole::Xs));
    std::vector<QWidget*> rows{header};
    if (s.import_scanning()) rows.push_back(note_row("Scanning..."));
    else if (entries.empty()) rows.push_back(note_row("No server configs found"));
    bool any = false;
    for (std::size_t i = 0; i < entries.size(); ++i) {
        const auto& e = entries[i];
        any = any || e.selected;
        auto* check = new QCheckBox;
        check->setChecked(e.selected);
        check->setAccessibleName(QString::fromStdString(e.candidate.server.name));
        QObject::connect(check, &QCheckBox::toggled, check, [send, i](bool v) { send(msg::ImportSelect{i, v}); });
        auto* source = kw::label(std::format("{} {}", loa::view::panel::label(e.candidate.source), e.candidate.path), TextRole::Caption, Tone::Faint);
        auto* line = hrow({check, kw::label(e.candidate.server.name, TextRole::Label, Tone::Normal)}, spi(SpaceRole::S));
        add(line, source, 1);
        QLabel* status = nullptr;
        if (!e.outcome) {
            if (e.exists) status = kw::label("already registered", TextRole::Caption, Tone::Warn);
        } else if (std::holds_alternative<import_outcome::Imported>(*e.outcome)) {
            status = kw::label("imported", TextRole::Caption, Tone::Ok);
        } else if (const auto* sk = std::get_if<import_outcome::Skipped>(&*e.outcome)) {
            status = kw::label("skipped: " + sk->reason, TextRole::Caption, Tone::Muted);
        } else {
            status = kw::label("failed: " + std::get<import_outcome::Failed>(*e.outcome).message, TextRole::Caption, Tone::Danger);
        }
        add(line, status);
        line->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
        rows.push_back(line);
    }
    auto* submit = hrow({primary("Import selected", any ? on(send, msg::ImportSubmit{}) : std::function<void()>{})});
    stretch(submit);
    submit->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
    rows.push_back(submit);
    return card(rows);
}

} // namespace

Servers::Servers(const State& s, const Catalog& catalog, bool wide, const Emit& send) {
    setAccessibleName(QStringLiteral("servers"));
    auto* col = box(QBoxLayout::TopToBottom, {}, spi(SpaceRole::S), this);
    auto* header = hrow({kw::label("Servers", TextRole::Caption, Tone::Muted),
                         kw::label(std::to_string(catalog.servers.size()), TextRole::Caption, Tone::Faint)},
                        spi(SpaceRole::Xs));
    stretch(header);
    add(header, ghost("Add server", Icon::Plus, on(send, msg::FormOpen{FormId::Server})));
    add(header, action("Import", on(send, msg::ImportScan{})));
    add(col, header);
    if (const auto& f = s.add_server()) {
        auto kind = [&](std::string_view l, TransportKind k) {
            auto* b = kw::text_button(l, ButtonKind::tab(f->spec.kind == k), on(send, msg::TransportPicked{k}));
            b->setAccessibleName(qs(l));
            return b;
        };
        auto* kinds = hrow({kind("stdio", TransportKind::Stdio), kind("HTTP", TransportKind::Http)}, spi(SpaceRole::Xs) / 2);
        auto* form = kw::form_view(f->spec, form_errors(f->errors),
                                   [send](std::string_view k, std::string v) { send(msg::FormEdit{FormId::Server, std::string(k), std::move(v)}); },
                                   on(send, msg::FormSubmit{FormId::Server}));
        add(col, form_card("Add server", kinds, form, "Add", FormId::Server, send));
    }
    add(col, import_view(s, send));
    if (catalog.servers.empty()) add(col, card({note_row(s.filter().empty() ? "No MCP servers" : "No server matches the filter")}));
    std::vector<QWidget*> cards;
    for (const auto& v : catalog.servers) cards.push_back(server_view(s, v, send));
    add(col, grid(cards, wide));
}

} // namespace loa::ui::panel::tools::view
