// The remote hosts section. Port of `crates/loa-ui-panel/src/tools/view/hosts.rs`.

#include <format>

#include <QCheckBox>
#include <QFrame>

#include "view.hpp"

namespace loa::ui::panel::tools::view {

using namespace pw;

namespace {

// Height of the audit log's own scroll area.
constexpr int AUDIT_HEIGHT = 260;

QCheckBox* check(bool on_, std::string_view text, Limit limit, const Emit& send) {
    auto* c = new QCheckBox(qs(text));
    c->setChecked(on_);
    c->setAccessibleName(qs(text));
    QObject::connect(c, &QCheckBox::toggled, c, [send, limit](bool v) { send(msg::LimitSet{limit, v}); });
    return c;
}

// A host's audit log, newest first, in a scroll area of its own.
QWidget* audit_log(const std::vector<AuditRow>& rows, const Emit& send) {
    auto* header = hrow({kw::label("Audit log", TextRole::Caption, Tone::Muted), kw::label(std::to_string(rows.size()), TextRole::Caption, Tone::Faint)},
                        spi(SpaceRole::Xs));
    stretch(header);
    add(header, ghost("Close", Icon::X, on(send, msg::CloseAudit{})));
    if (rows.empty()) return vcol({header, kw::label("No entries", TextRole::Caption, Tone::Faint)}, spi(SpaceRole::Xs));
    auto* entries = vcol({}, spi(SpaceRole::Xs));
    for (std::size_t i = 0; i < rows.size(); ++i) {
        const auto& e = rows[i];
        const bool ok = e.outcome == "ok" || e.outcome.starts_with("ok:");
        if (i > 0) {
            auto* rule = new QFrame;
            rule->setFrameShape(QFrame::HLine);
            add(entries, rule);
        }
        auto* what = hrow({kw::label(loa::view::panel::label(e.action), TextRole::Caption, Tone::Normal)}, spi(SpaceRole::S));
        add(what, kw::label(e.detail, TextRole::Mono, Tone::Normal), 1);
        add(what, kw::label(e.when, TextRole::Caption, Tone::Faint));
        auto* how = hrow({}, spi(SpaceRole::S));
        add(how, kw::label(e.outcome, TextRole::Caption, ok ? Tone::Ok : Tone::Danger), 1);
        add(how, kw::label(e.session, TextRole::Caption, Tone::Faint));
        add(entries, vcol({what, how}, spi(SpaceRole::Xs) / 2));
    }
    auto* log = scroll(entries, "audit log");
    log->setMaximumHeight(AUDIT_HEIGHT);
    return vcol({header, log}, spi(SpaceRole::Xs));
}

// A host key or approval question with its answers.
QWidget* prompt_card(const RemotePrompt& p, const Emit& send) {
    auto answer = [&](std::string_view s, PromptAnswer a) { return action(s, on(send, msg::AnswerPrompt{p.id, a})); };
    auto danger = [](QPushButton* b) {
        kit::qt::style_button(*b, ButtonKind::danger());
        return b;
    };
    auto* answers = hrow({}, spi(SpaceRole::Xs) / 2);
    if (p.kind == PromptKind::HostKey) {
        add(answers, answer("Trust", PromptAnswer::Trust));
        add(answers, danger(answer("Reject", PromptAnswer::Reject)));
    } else {
        add(answers, answer("Allow once", PromptAnswer::AllowOnce));
        add(answers, answer("Allow for session", PromptAnswer::AllowSession));
        add(answers, danger(answer("Deny", PromptAnswer::Deny)));
    }
    stretch(answers);
    auto* icon = new QLabel;
    icon->setPixmap(kit::tinted(Icon::TriangleAlert, TextRole::Caption, Tone::Warn));
    auto* title = hrow({icon, kw::label(p.title, TextRole::Label, Tone::Normal)}, spi(SpaceRole::S));
    stretch(title);
    auto* body = kw::label(p.body, TextRole::Mono, Tone::Muted);
    return surface(vcol({title, body, answers}, spi(SpaceRole::Xs)), Surface::Attention, spi(SpaceRole::M));
}

// Alias and address, the limits in a line and the actions; the limits editor and audit log open under it.
QWidget* host_view(const State& s, const RemoteHost& host, bool wide, const Emit& send) {
    const std::string& alias = host.alias;
    const std::string target = std::format("{}{}{}{}", host.user ? *host.user + "@" : std::string(), host.hostname,
                                           host.port ? std::format(":{}", *host.port) : std::string(),
                                           host.proxy_jump ? " via " + *host.proxy_jump : std::string());
    const auto& l = host.limits;
    std::string paths;
    for (const auto& p : l.allowed_paths) paths += (paths.empty() ? "" : ", ") + p;
    if (paths.empty()) paths = "no paths";
    const auto summary_text = std::format("{} \u00b7 {} \u00b7 {} \u00b7 {}", l.read_only ? "read only" : "read write",
                                          l.commands_allowed ? "commands allowed" : "no commands",
                                          l.auto_approve ? "runs without asking" : "asks first", paths);
    auto* summary = kw::label(summary_text, TextRole::Caption, Tone::Faint);
    auto* actions = hrow({action("Edit limits", on(send, msg::EditLimits{alias})), action("Terminal", on(send, msg::OpenTerminal{alias})),
                          action("Audit log", on(send, msg::ShowAudit{alias}))},
                         spi(SpaceRole::Xs) / 2);
    auto* remove = action("Remove", on(send, msg::RemoveHost{alias}));
    kit::qt::style_button(*remove, ButtonKind::danger());
    add(actions, remove);
    auto* name = hrow({bold(alias, TextRole::Label)}, spi(SpaceRole::S));
    add(name, kw::label(target, TextRole::Mono, Tone::Muted), 1);
    QWidget* head = wide ? vcol({hrow({name, actions}, spi(SpaceRole::S)), summary}) : vcol({name, summary, actions});
    head->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::Xs), spi(SpaceRole::Xs), spi(SpaceRole::Xs));
    auto* col = vcol({head});
    if (const auto& d = s.limits(); d && d->host == host.alias) {
        auto* flags = hrow({check(d->limits.read_only, "Read only", Limit::ReadOnly, send), check(d->limits.commands_allowed, "Commands allowed", Limit::Commands, send)},
                           spi(SpaceRole::L));
        stretch(flags);
        auto* list = kw::list_editor(
            d->limits.allowed_paths, [send](std::size_t i) { send(msg::LimitsRemovePath{i}); }, nullptr, d->new_path,
            [send](std::string v) { send(msg::FormEdit{FormId::Limits, "path", std::move(v)}); }, on(send, msg::LimitsAddPath{}));
        auto* editor = vcol({flags, check(d->limits.auto_approve, "Run writes and commands without asking", Limit::AutoApprove, send),
                             kw::label("Allowed paths", TextRole::Caption, Tone::Muted), list},
                            spi(SpaceRole::S));
        if (d->path_error) add(editor, kw::label(*d->path_error, TextRole::Caption, Tone::Danger));
        add(editor, form_buttons("Save", FormId::Limits, send));
        add(col, inset(editor));
    }
    if (const auto& a = s.audit(); a && a->first == alias) add(col, inset(audit_log(a->second, send)));
    return col;
}

} // namespace

Hosts::Hosts(const State& s, bool wide, const Emit& send) {
    setAccessibleName(QStringLiteral("hosts"));
    auto* col = box(QBoxLayout::TopToBottom, {}, spi(SpaceRole::S), this);
    if (!s.prompts().empty()) {
        auto* prompts = vcol({hrow({kw::label("Waiting for you", TextRole::Caption, Tone::Warn),
                                    kw::label(std::to_string(s.prompts().size()), TextRole::Caption, Tone::Faint)},
                                   spi(SpaceRole::Xs))},
                             spi(SpaceRole::Xs));
        for (const auto& p : s.prompts()) add(prompts, prompt_card(p, send));
        add(col, prompts);
    }
    if (const auto& f = s.add_host()) {
        auto* form = kw::form_view(f->spec, form_errors(f->errors),
                                   [send](std::string_view k, std::string v) { send(msg::FormEdit{FormId::Host, std::string(k), std::move(v)}); },
                                   on(send, msg::FormSubmit{FormId::Host}));
        add(col, form_card("Add host", nullptr, form, "Add", FormId::Host, send));
    }
    const auto hosts = s.visible_hosts();
    auto* actions = hrow({action("Import from ssh config", on(send, msg::ImportSshHosts{})), ghost("Add host", Icon::Plus, on(send, msg::FormOpen{FormId::Host}))},
                         spi(SpaceRole::Xs) / 2);
    add(col, block_header(s, "hosts", "Hosts", hosts.size(), actions, send));
    if (s.block_open("hosts")) {
        std::vector<QWidget*> rows;
        if (hosts.empty()) rows.push_back(note_row("No remote hosts"));
        for (const auto* h : hosts) rows.push_back(host_view(s, *h, wide, send));
        add(col, card(rows));
    }
}

} // namespace loa::ui::panel::tools::view
