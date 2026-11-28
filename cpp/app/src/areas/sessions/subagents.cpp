// Per-parent child subtabs, live transcript polling and subagent messaging (port of
// `loa-app/src/areas/sessions/subagents.rs`).

#include "sessions.hpp"

#include <algorithm>

#include <QHBoxLayout>
#include <QLineEdit>
#include <QScrollArea>
#include <QVBoxLayout>
#include <QWidget>

#include "loa/app/perform.hpp"
#include "loa/contracts/session.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/session/transcript/view.hpp"

namespace loa::app::areas::sessions {

namespace {

namespace attr = core::attr;
namespace kit = loa::ui::kit;
namespace widgets = loa::ui::kit::widgets;
namespace theme = loa::ui::kit::theme;
using kit::Tone;
using theme::SpaceRole;
using theme::TextRole;
using Icon = loa::view::theme::Icon;

// Thickness of the rules around the transcript (Rust `rule::horizontal(1)`).
constexpr int RULE_WIDTH = 1;

bool trimmed_empty(std::string_view text) { return text.find_first_not_of(" \t\r\n") == std::string_view::npos; }

bool terminal(std::optional<core::JobStatus> status) {
    return status == core::JobStatus::Done || status == core::JobStatus::Error || status == core::JobStatus::Cancelled;
}

contracts::Awaitable<std::expected<contracts::SubagentTranscript, std::string>> read_transcript(app_runtime::HarnessPtr harness,
                                                                                                 std::string id) {
    auto result = co_await harness->subagent_transcript(std::move(id));
    if (!result) co_return std::unexpected(result.error().message());
    co_return std::move(*result);
}

contracts::Awaitable<std::expected<void, SendError>> deliver(app_runtime::HarnessPtr harness, std::string id, std::string text) {
    auto result = co_await harness->message_subagent(std::move(id), std::move(text));
    if (result) co_return std::expected<void, SendError>{};
    if (const auto* gone = result.error().get<contracts::AdapterError::NotAddressable>())
        co_return std::unexpected(SendError{SendError::Kind::NotAddressable, gone->reason});
    co_return std::unexpected(SendError{SendError::Kind::Other, result.error().message()});
}

// `body` padded by `[vertical, horizontal]` spacing roles.
QWidget* padded(QWidget* body, SpaceRole vertical, SpaceRole horizontal) {
    auto* box = new QWidget;
    auto* layout = new QVBoxLayout(box);
    const int v = static_cast<int>(theme::sp(vertical));
    const int h = static_cast<int>(theme::sp(horizontal));
    layout->setContentsMargins(h, v, h, v);
    layout->addWidget(body);
    return box;
}

QWidget* rule() {
    auto* line = new QWidget;
    line->setAccessibleName(QStringLiteral("rule"));
    line->setFixedHeight(RULE_WIDTH);
    kit::qt::style_container(*line, kit::Surface::Badge);
    return line;
}

QWidget* error_box(std::string_view text) {
    auto* box = padded(widgets::label(text, TextRole::Caption, Tone::Danger), SpaceRole::Xs, SpaceRole::S);
    kit::qt::style_container(*box, kit::Surface::ErrorBox);
    return box;
}

QWidget* centered(std::string_view text, TextRole role, Tone tone) {
    auto* label = widgets::label(text, role, tone);
    label->setAlignment(Qt::AlignCenter);
    return label;
}

QWidget* explanation(std::string_view title, std::string_view detail) {
    auto* body = new QWidget;
    auto* col = new QVBoxLayout(body);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(static_cast<int>(theme::sp(SpaceRole::Xs)));
    col->addWidget(widgets::label(title, TextRole::Label, Tone::Normal));
    auto* text = widgets::label(detail, TextRole::Caption, Tone::Muted);
    text->setWordWrap(true);
    col->addWidget(text);
    return padded(widgets::card(body), SpaceRole::S, SpaceRole::M);
}

} // namespace

// ---- tabs ----------------------------------------------------------------------------------

ChildTab::ChildTab(std::string id_, std::uint64_t generation_, std::string title_, std::optional<core::JobStatus> status_)
    : id(std::move(id_)), generation(generation_), title(std::move(title_)), tab_label(title + " · " + id), status(status_) {
    transcript.set_read_only(true);
}

void SubagentTabs::open(std::string_view parent, const std::string& child, std::string title,
                        std::optional<core::JobStatus> status) {
    auto& entry = parents[std::string(parent)];
    if (std::ranges::find(entry.order, child) == entry.order.end()) entry.order.push_back(child);
    if (auto it = entry.children.find(child); it != entry.children.end()) {
        auto& tab = it->second;
        tab.title = title;
        tab.tab_label = title + " · " + tab.id;
        if (status) tab.status = status;
    } else {
        ++next_generation_;
        entry.children.emplace(child, ChildTab(child, next_generation_, std::move(title), status));
    }
    entry.selected = child;
}

void SubagentTabs::select(std::string_view parent, std::optional<std::string> child) {
    auto it = parents.find(parent);
    if (it == parents.end()) return;
    auto& entry = it->second;
    entry.selected = child && entry.children.contains(*child) ? std::move(child) : std::nullopt;
}

void SubagentTabs::close(std::string_view parent, std::string_view child) {
    auto it = parents.find(parent);
    if (it == parents.end()) return;
    auto& entry = it->second;
    std::erase(entry.order, child);
    if (auto tab = entry.children.find(child); tab != entry.children.end()) entry.children.erase(tab);
    if (entry.selected == child) entry.selected.reset();
}

std::optional<std::string_view> SubagentTabs::selected_of(std::string_view parent) const {
    auto it = parents.find(parent);
    if (it == parents.end() || !it->second.selected) return std::nullopt;
    return std::string_view(*it->second.selected);
}

std::optional<std::string> SubagentTabs::selected_live(std::string_view parent) const {
    auto it = parents.find(parent);
    if (it == parents.end() || !it->second.selected) return std::nullopt;
    auto child = it->second.children.find(*it->second.selected);
    if (child == it->second.children.end() || !child->second.live()) return std::nullopt;
    return child->second.id;
}

ChildTab* SubagentTabs::tab(std::string_view parent, std::string_view child) {
    auto it = parents.find(parent);
    if (it == parents.end()) return nullptr;
    auto tab = it->second.children.find(child);
    return tab == it->second.children.end() ? nullptr : &tab->second;
}

const ChildTab* SubagentTabs::tab(std::string_view parent, std::string_view child) const {
    return const_cast<SubagentTabs*>(this)->tab(parent, child);
}

ChildTab* SubagentTabs::current(std::string_view parent, std::string_view child, std::uint64_t generation) {
    auto* found = tab(parent, child);
    return found && found->generation == generation ? found : nullptr;
}

// ---- the area's subagent parts --------------------------------------------------------------

void State::sync_active_parent(const Ctx& ctx) {
    const auto* active = ctx.runtime.sessions().active();
    active_parent_ = active ? std::optional<std::string>(active->id().str()) : std::nullopt;
    active_subagent_messages_ = active && active->harness() &&
                                detail::capabilities(ctx, active->harness()->harness()).has(contracts::Feature::SubagentMessages);
}

bool State::should_poll_subagent() const {
    return active_subagent_messages_ && active_parent_ && subagents.selected_live(*active_parent_).has_value();
}

Task State::refresh_selected_subagent_on_activate(const Ctx& ctx) {
    const auto& parent = ctx.runtime.sessions().active_id();
    if (!parent) return Task::none();
    auto child = subagents.selected_of(parent->as_str());
    if (!child) return Task::none();
    const std::string child_id(*child);
    const auto* tab = subagents.tab(parent->as_str(), child_id);
    if (tab && (!tab->dom || tab->live())) return fetch_child_transcript(ctx, parent->str(), child_id);
    return Task::none();
}

Task State::open_child_subtab(const Ctx& ctx, std::string child) {
    const auto& parent = ctx.runtime.sessions().active_id();
    if (!parent) return Task::none();
    std::string title = child;
    std::optional<core::JobStatus> status;
    for (const auto& [_, job] : agents.jobs())
        if (job.id() == std::optional<std::string_view>(child)) {
            title = std::string(job.label());
            status = job.status();
            break;
        }
    subagents.open(parent->as_str(), child, std::move(title), status);
    return fetch_child_transcript(ctx, parent->str(), child);
}

Task State::select_child_subtab(const Ctx& ctx, std::optional<std::string> child) {
    const auto& parent = ctx.runtime.sessions().active_id();
    if (!parent) return Task::none();
    subagents.select(parent->as_str(), child);
    if (!child) return Task::none();
    const auto* tab = subagents.tab(parent->as_str(), *child);
    if (tab && (!tab->dom || tab->live())) return fetch_child_transcript(ctx, parent->str(), *child);
    return Task::none();
}

void State::close_child_subtab(const Ctx& ctx, const std::string& child) {
    if (const auto& parent = ctx.runtime.sessions().active_id()) subagents.close(parent->as_str(), child);
}

Task State::poll_selected_subagent(const Ctx& ctx) {
    const auto& parent = ctx.runtime.sessions().active_id();
    if (!parent) return Task::none();
    auto child = subagents.selected_live(parent->as_str());
    if (!child) return Task::none();
    return fetch_child_transcript(ctx, parent->str(), *child);
}

Task State::fetch_child_transcript(const Ctx& ctx, const std::string& parent, const std::string& child) {
    const auto* session = ctx.runtime.sessions().get(parent);
    if (!session || !session->harness()) return Task::none();
    auto harness = session->harness();
    if (!detail::capabilities(ctx, harness->harness()).has(contracts::Feature::SubagentMessages)) return Task::none();
    auto* tab = subagents.tab(parent, child);
    if (!tab) return Task::none();
    if (tab->loading) {
        tab->refetch_needed = true;
        return Task::none();
    }
    tab->loading = true;
    tab->refetch_needed = false;
    const auto generation = tab->generation;
    return perform([harness, child] { return read_transcript(harness, child); },
                   [parent, child, generation](std::expected<contracts::SubagentTranscript, std::string> result) -> loa::app::Message {
                       return message(msg::SubagentTranscriptLoaded{parent, child, generation, std::move(result)});
                   });
}

Task State::on_transcript_loaded(const Ctx& ctx, msg::SubagentTranscriptLoaded loaded) {
    auto* tab = subagents.current(loaded.parent, loaded.child, loaded.generation);
    if (!tab) return Task::none();
    tab->loading = false;
    const bool refetch = std::exchange(tab->refetch_needed, false);
    if (loaded.result) {
        tab->fetch_error.reset();
        if (!std::exchange(tab->addressable_stale, false)) tab->addressable = loaded.result->addressable;
        if (!tab->dom || !(*tab->dom == loaded.result->dom)) {
            tab->transcript.load(loaded.result->dom);
            tab->dom = std::move(loaded.result->dom);
        }
    } else {
        tab->fetch_error = std::move(loaded.result.error());
    }
    if (refetch) return fetch_child_transcript(ctx, loaded.parent, loaded.child);
    return Task::none();
}

void State::set_child_draft(const Ctx& ctx, const std::string& parent, const std::string& child, std::string text) {
    if (!can_message_child(ctx, parent, child)) return;
    if (auto* tab = subagents.tab(parent, child)) tab->draft = std::move(text);
}

Task State::send_child_message(const Ctx& ctx, const std::string& parent, const std::string& child) {
    const auto* session = ctx.runtime.sessions().get(parent);
    if (!session || !session->harness()) return Task::none();
    auto harness = session->harness();
    if (!detail::capabilities(ctx, harness->harness()).has(contracts::Feature::SubagentMessages)) return Task::none();
    auto* tab = subagents.tab(parent, child);
    if (!tab || !tab->addressable || tab->sending || trimmed_empty(tab->draft)) return Task::none();
    auto text = std::exchange(tab->draft, std::string());
    tab->sending = true;
    tab->send_error.reset();
    const auto generation = tab->generation;
    return perform([harness, child, text] { return deliver(harness, child, text); },
                   [parent, child, generation, text](std::expected<void, SendError> result) -> loa::app::Message {
                       return message(msg::SubagentMessageSent{parent, child, generation, text, std::move(result)});
                   });
}

Task State::on_message_sent(const Ctx& ctx, msg::SubagentMessageSent sent) {
    auto* tab = subagents.current(sent.parent, sent.child, sent.generation);
    if (!tab) return Task::none();
    tab->sending = false;
    tab->addressable_stale = tab->loading;
    if (sent.result) {
        tab->send_error.reset();
    } else {
        tab->draft = trimmed_empty(tab->draft) ? sent.text : sent.text + "\n" + tab->draft;
        auto& error = sent.result.error();
        if (error.kind == SendError::Kind::NotAddressable) tab->addressable = false;
        tab->send_error = std::move(error.message);
    }
    // Shows the delivered message, or the final state of a subagent that stopped taking them.
    return fetch_child_transcript(ctx, sent.parent, sent.child);
}

Task State::update_child_chat(Ctx& ctx, const std::string& parent, const std::string& child, chat::Message message) {
    auto* tab = subagents.tab(parent, child);
    if (!tab) return Task::none();
    // The child transcript is drawn anew from its state each time: its widget operations
    // (scrolling, the clipboard) have no live view to act on.
    auto [task, event] = tab->transcript.update(std::move(message));
    (void)task;
    if (event)
        if (const auto* link = std::get_if<loa::view::session::transcript::intent::OpenLink>(&*event)) open_link(ctx, link->url);
    return Task::none();
}

void State::detach_child_transcript(Ctx& ctx, const std::string& reference) {
    const auto& library = *ctx.runtime.library();
    auto listings = library.store().list();
    const bool stored = listings && std::ranges::any_of(*listings, [&](const auto& listing) {
                            const auto* info = std::get_if<bus::SessionInfo>(&listing);
                            return info && info->id == reference;
                        });
    core::Dom dom;
    if (stored) {
        auto loaded = library.load(reference);
        if (!loaded) return ctx.notify("Can't open the subagent's transcript: " + loaded.error().to_string());
        dom = std::move(*loaded);
    } else {
        const std::filesystem::path path(reference);
        std::error_code error;
        if (!std::filesystem::is_regular_file(path, error))
            return ctx.notify("The subagent's session " + reference + " isn't on this machine");
        const auto* parent = ctx.runtime.sessions().active();
        if (!parent) return;
        auto harness = parent->harness_id();
        auto adapter = harness ? ctx.runtime.harnesses().registry.get(*harness) : nullptr;
        if (!adapter) return ctx.notify("The parent session's harness isn't supported here");
        core::MemorySink sink;
        if (auto imported = adapter->import(path, sink); !imported)
            return ctx.notify("Can't open the subagent's transcript: " + imported.error().message());
        dom = sink.dom();
    }
    std::optional<std::string> title;
    for (const auto& [_, job] : agents.jobs())
        if (job.session() == std::optional<std::string_view>(reference)) {
            title = std::string(job.label());
            break;
        }
    if (!title)
        if (auto summary = contracts::SessionSummary::from_dom(dom, false)) title = summary->title;
    ctx.request(request::DetachTranscript{reference, title.value_or("Subagent transcript"), std::move(dom)});
}

Task State::on_job_patch(const Ctx& ctx, std::string_view session, NodeId node_id) {
    const auto* open = ctx.runtime.sessions().get(session);
    const auto* node = open ? open->dom().get(node_id) : nullptr;
    if (!node || node->kind != core::ElementKind::Job) return Task::none();
    auto kind = node->str_attr(attr::KIND);
    if (!kind || core::parse<core::JobKind>(*kind) != core::JobKind::Subagent) return Task::none();
    auto child_id = node->str_attr(attr::ID);
    if (!child_id || child_id->empty()) return Task::none();
    auto status_text = node->str_attr(attr::STATUS);
    const auto status = (status_text ? core::parse<core::JobStatus>(*status_text) : std::nullopt).value_or(core::JobStatus::Running);
    auto title = node->str_attr(attr::TITLE);
    if (!title || title->empty()) title = node->str_attr(attr::AGENT);
    auto* tab = subagents.tab(session, *child_id);
    if (!tab) return Task::none();
    if (title && !title->empty()) {
        tab->title = std::string(*title);
        tab->tab_label = tab->title + " · " + tab->id;
    }
    const bool was_terminal = terminal(tab->status);
    tab->status = status;
    if (!was_terminal && terminal(status)) return fetch_child_transcript(ctx, std::string(session), std::string(*child_id));
    return Task::none();
}

bool State::can_message_child(const Ctx& ctx, std::string_view parent, std::string_view child) const {
    const auto* session = ctx.runtime.sessions().get(parent);
    if (!session || !session->harness()) return false;
    if (!detail::capabilities(ctx, session->harness()->harness()).has(contracts::Feature::SubagentMessages)) return false;
    const auto* tab = subagents.tab(parent, child);
    return tab && tab->addressable && !tab->sending;
}

// ---- views ---------------------------------------------------------------------------------

std::optional<std::pair<QWidget*, QWidget*>> State::view_child_center(const Ctx& ctx, const Emit& send) const {
    const auto* parent = ctx.runtime.sessions().active();
    if (!parent) return std::nullopt;
    auto found = subagents.parents.find(parent->id().as_str());
    if (found == subagents.parents.end() || found->second.order.empty()) return std::nullopt;
    const auto& tabs = found->second;
    const std::string parent_title = parent->summary() ? parent->summary()->title : std::string("Conversation");

    std::vector<widgets::Tab> strip_tabs;
    strip_tabs.push_back({parent_title, Icon::Bot, !tabs.selected,
                          [send] { send(message(msg::SubagentSelect{std::nullopt})); }, nullptr});
    for (const auto& child_id : tabs.order) {
        auto child = tabs.children.find(child_id);
        if (child == tabs.children.end()) continue;
        auto* close = widgets::icon_button(Icon::X, "Close", [send, child_id] { send(message(msg::SubagentClose{child_id})); });
        close->setAccessibleName(QString::fromStdString("Close " + child->second.tab_label));
        strip_tabs.push_back({child->second.tab_label, Icon::Cpu, tabs.selected == child_id,
                              [send, child_id] { send(message(msg::SubagentSelect{child_id})); }, close});
    }
    auto* scroll = new QScrollArea;
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    scroll->setWidget(widgets::tab_strip(strip_tabs));
    auto* strip = padded(scroll, SpaceRole::Xs, SpaceRole::S);
    strip->setAccessibleName(QStringLiteral("subagent tabs"));
    strip->setObjectName(strip->accessibleName());
    kit::qt::style_container(*strip, kit::Surface::Side);

    QWidget* body = nullptr;
    if (tabs.selected)
        if (auto child = tabs.children.find(*tabs.selected); child != tabs.children.end())
            body = view_child_subtab(ctx, *parent, child->second, send);
    return std::pair{strip, body};
}

QWidget* State::view_child_subtab(const Ctx& ctx, const app_runtime::Session& parent, const ChildTab& child,
                                  const Emit& send) const {
    const std::string parent_id = parent.id().str();
    const bool has_harness = static_cast<bool>(parent.harness());
    const auto harness_id = parent.harness_id().value_or("");
    const bool supports_messages =
        has_harness && detail::capabilities(ctx, parent.harness()->harness()).has(contracts::Feature::SubagentMessages);
    const bool can_message = supports_messages && child.addressable && !child.sending;

    auto* page = new QWidget;
    page->setAccessibleName(QString::fromStdString("subagent " + child.id));
    page->setObjectName(page->accessibleName());
    auto* col = new QVBoxLayout(page);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(0);

    // Header: title, status badge and a syncing note.
    auto* header = new QWidget;
    auto* row = new QHBoxLayout(header);
    row->setContentsMargins(0, 0, 0, 0);
    row->setSpacing(static_cast<int>(theme::sp(SpaceRole::S)));
    row->addWidget(widgets::label(child.title, TextRole::Body, Tone::Normal));
    if (child.status) {
        Tone tone = Tone::Muted;
        if (*child.status == core::JobStatus::Done) tone = Tone::Ok;
        else if (*child.status == core::JobStatus::Error) tone = Tone::Danger;
        row->addWidget(widgets::badge(core::as_str(*child.status), tone));
    }
    row->addStretch(1);
    if (supports_messages && child.loading) row->addWidget(widgets::label("Syncing…", TextRole::Caption, Tone::Faint));
    col->addWidget(padded(header, SpaceRole::Xs, SpaceRole::M));
    col->addWidget(rule());

    if (!has_harness) {
        col->addWidget(explanation("Parent session is not running",
                                   "This conversation isn't running, so its subagents can't be reached. Resume it to "
                                   "message live subagents."));
    } else if (!supports_messages) {
        col->addWidget(explanation("Subagent messaging not supported",
                                   std::string(ctx.runtime.harnesses().label(harness_id)) +
                                       " doesn't let Loa message its subagents. Use Transcript in the Agents tab to read a copy."));
    }
    if (child.fetch_error) col->addWidget(error_box(*child.fetch_error));

    QWidget* transcript = nullptr;
    const bool has_rows = child.dom && child.dom->root() && !child.dom->children(child.dom->root()->id).empty();
    if (has_rows) {
        auto* view = new chat::TranscriptView;
        view->render(child.transcript, *child.dom);
        QObject::connect(view, &chat::TranscriptView::message, view, [send, parent_id, id = child.id](const chat::Message& m) {
            send(message(msg::SubagentChat{parent_id, id, m}));
        });
        transcript = view;
    } else if (child.dom && supports_messages) {
        transcript = centered("No transcript messages yet", TextRole::Label, Tone::Muted);
    } else if (!child.dom && supports_messages) {
        transcript = centered(child.loading ? "Loading subagent transcript…" : "No transcript loaded", TextRole::Label, Tone::Muted);
    } else {
        transcript = new QWidget;
    }
    col->addWidget(transcript, 1);
    col->addWidget(rule());

    if (supports_messages) {
        auto* composer = new QWidget;
        auto* lines = new QVBoxLayout(composer);
        lines->setContentsMargins(0, 0, 0, 0);
        lines->setSpacing(static_cast<int>(theme::sp(SpaceRole::Xs)));
        if (child.send_error) lines->addWidget(error_box(*child.send_error));
        if (!child.addressable && child.dom && !child.loading) {
            const auto note = terminal(child.status)
                                  ? child.id + " has finished, so OMP no longer accepts messages for it. Its conversation stays readable here."
                                  : child.id + " isn't accepting messages right now. Its conversation stays readable here.";
            auto* label = widgets::label(note, TextRole::Caption, Tone::Muted);
            label->setWordWrap(true);
            lines->addWidget(label);
        }
        const char* placeholder = child.sending       ? "Sending…"
                                  : !child.addressable ? "Subagent is not accepting messages"
                                                       : "Message subagent…";
        auto* input = new QLineEdit;
        input->setAccessibleName(QStringLiteral("subagent message"));
        input->setPlaceholderText(QString::fromUtf8(placeholder));
        input->setFont(kit::qt::font(TextRole::Body));
        input->setText(QString::fromStdString(child.draft));
        input->setEnabled(can_message);
        const bool sendable = can_message && !trimmed_empty(child.draft);
        if (can_message) {
            QObject::connect(input, &QLineEdit::textEdited, input, [send, parent_id, id = child.id](const QString& text) {
                send(message(msg::SubagentDraft{parent_id, id, text.toStdString()}));
            });
            if (sendable)
                QObject::connect(input, &QLineEdit::returnPressed, input,
                                 [send, parent_id, id = child.id] { send(message(msg::SubagentSend{parent_id, id})); });
        }
        widgets::Callback on_send;
        if (sendable) on_send = [send, parent_id, id = child.id] { send(message(msg::SubagentSend{parent_id, id})); };
        auto* button = widgets::text_button(child.sending ? "Sending…" : "Send", kit::ButtonKind::primary(), on_send);
        auto* send_row = new QWidget;
        auto* fields = new QHBoxLayout(send_row);
        fields->setContentsMargins(0, 0, 0, 0);
        fields->setSpacing(static_cast<int>(theme::sp(SpaceRole::S)));
        fields->addWidget(input, 1);
        fields->addWidget(button);
        lines->addWidget(send_row);
        col->addWidget(padded(composer, SpaceRole::S, SpaceRole::M));
    }
    return page;
}

} // namespace loa::app::areas::sessions
