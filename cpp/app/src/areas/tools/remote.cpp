#include "areas/tools/remote.hpp"

#include <algorithm>
#include <cinttypes>
#include <cstdio>

#include "loa/app/area.hpp"
#include "loa/core/patch.hpp"
#include "loa/remote/hosts.hpp"
#include "loa/remote/known_hosts.hpp"
#include "loa/remote/tools.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::app::areas::tools {

namespace {

namespace attr = core::attr;
namespace vp = view::panel;
using core::DecisionStatus;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

std::string prefixed(std::uint64_t id) { return std::string(REQUEST_PREFIX) + std::to_string(id); }

// The session a question belongs to: an approval's own, a host key question's the active
// session's (else any session's) running turn.
const app_runtime::Session* target_session(const remote::Prompt& prompt, const Ctx& ctx) {
    const auto& sessions = ctx.runtime.sessions();
    if (const auto* request = std::get_if<remote::ApprovalRequest>(&prompt)) return sessions.get(request->session_id);
    if (const auto* active = sessions.active(); active && active->is_busy()) return active;
    for (const auto& [_, session] : sessions.iter())
        if (session.is_busy()) return &session;
    return nullptr;
}

// Writes a waiting card for `prompt` into its session, under its latest turn.
std::optional<Card> raise(const remote::PromptView& prompt, const Ctx& ctx) {
    const auto* session = target_session(prompt.prompt, ctx);
    if (!session) return std::nullopt;
    const auto& writer = session->writer();
    auto parent = writer->with_dom([](const core::Dom& dom) -> std::optional<core::NodeId> {
        auto turns = app_runtime::dom_turns(dom);
        if (!turns.empty()) return turns.back()->id;
        if (const auto* root = dom.root()) return root->id;
        return std::nullopt;
    });
    if (!parent) return std::nullopt;
    auto [kind, options] = decision_options(prompt.prompt);
    core::Attrs attrs{
        {std::string(attr::KIND), core::Value(kind)},
        {std::string(attr::STATUS), core::Value(DecisionStatus::Waiting)},
        {std::string(attr::TITLE), core::Value(title(prompt.prompt))},
        {std::string(attr::OPTIONS), std::move(options)},
        {std::string(attr::REQUEST_ID), core::Value(prefixed(prompt.id))},
        {std::string(attr::CREATED), core::Value(core::now_ms())},
    };
    auto committed = writer->commit(core::Draft::insert(*parent, core::ElementKind::Decision, std::move(attrs), body(prompt.prompt)));
    if (!committed) return std::nullopt;
    return Card{session->id(), committed->node};
}

// Closes a card: answered with `answer`, or cancelled.
void finish(const Ctx& ctx, const Card& card, DecisionStatus status, const contracts::Answer* answer) {
    const auto* session = ctx.runtime.sessions().get(card.session.as_str());
    if (!session) return;
    const auto& writer = session->writer();
    const bool waiting = writer->with_dom([&](const core::Dom& dom) {
        const auto* node = dom.get(card.decision);
        return node && node->str_attr(attr::STATUS) == core::as_str(DecisionStatus::Waiting);
    });
    if (!waiting) return;
    std::vector<core::Draft> drafts;
    if (answer) {
        drafts.push_back(core::Draft::set(card.decision, std::string(attr::ANSWER), core::Value(answer->values)));
        drafts.push_back(core::Draft::set(card.decision, std::string(attr::BY), core::Value(answer->by)));
        if (answer->reason) drafts.push_back(core::Draft::set(card.decision, std::string(attr::REASON), core::Value(*answer->reason)));
    }
    drafts.push_back(core::Draft::set(card.decision, std::string(attr::ENDED), core::Value(core::now_ms())));
    drafts.push_back(core::Draft::set(card.decision, std::string(attr::STATUS), core::Value(status)));
    for (auto& draft : drafts)
        if (!writer->commit(std::move(draft))) return;
}

constexpr std::string_view NOT_WAITING = "This remote host question is no longer waiting";

} // namespace

std::pair<Remote, std::optional<std::string>> Remote::make(const std::filesystem::path& data, remote::RemoteServiceConfig config) {
    const auto path = data / AUDIT_FILE;
    std::shared_ptr<remote::AuditSink> audit;
    std::optional<std::string> warning;
    if (auto opened = remote::SqliteAudit::open(path)) {
        audit = std::shared_ptr<remote::AuditSink>(std::move(*opened));
    } else {
        warning = "Can't open the remote audit log " + path.string() + " (" + opened.error().message() + "); this run logs in memory";
        audit = std::make_shared<remote::MemoryAudit>();
    }
    auto queue = std::make_shared<remote::PromptQueue>();
    auto service = std::make_shared<remote::RemoteService>(std::make_shared<remote::HostRegistry>(), std::move(audit), queue, queue,
                                                           std::move(config));
    Remote out;
    out.tools = std::make_shared<contracts::ToolSet>(remote::tool_set(service));
    out.service = std::move(service);
    out.queue = std::move(queue);
    return {std::move(out), std::move(warning)};
}

void Remote::forget(std::string_view session) const {
    queue->deny_session(session);
    service->end_session(session);
    if (http) http->forget(session);
}

std::pair<std::vector<vp::RemotePrompt>, bool> Remote::sync(const Ctx& ctx) {
    const auto pending = queue->pending();
    std::set<std::uint64_t> ids;
    for (const auto& prompt : pending) ids.insert(prompt.id);
    std::vector<std::uint64_t> gone;
    for (const auto& [id, _] : cards_)
        if (!ids.contains(id)) gone.push_back(id);
    for (auto id : gone) {
        auto node = cards_.extract(id);
        finish(ctx, node.mapped(), DecisionStatus::Cancelled, nullptr);
    }
    bool fresh = false;
    for (const auto& prompt : pending) {
        if (seen_.insert(prompt.id).second) {
            fresh = true;
            if (auto card = raise(prompt, ctx)) cards_.insert_or_assign(prompt.id, std::move(*card));
        }
    }
    std::erase_if(seen_, [&](std::uint64_t id) { return !ids.contains(id); });
    std::vector<vp::RemotePrompt> prompts;
    prompts.reserve(pending.size());
    for (const auto& prompt : pending) prompts.push_back(panel_prompt(prompt));
    return {std::move(prompts), fresh};
}

bool Remote::answer_panel(std::uint64_t id, vp::PromptAnswer answer, const Ctx& ctx) {
    bool taken = false;
    std::string_view value;
    switch (answer) {
    case vp::PromptAnswer::Trust: taken = queue->answer(id, true), value = TRUST; break;
    case vp::PromptAnswer::Reject: taken = queue->answer(id, false), value = REJECT; break;
    case vp::PromptAnswer::AllowOnce: taken = queue->answer(id, remote::Approval::allow_once()), value = core::answer::ALLOW_ONCE; break;
    case vp::PromptAnswer::AllowSession:
        taken = queue->answer(id, remote::Approval::allow_session()), value = core::answer::ALLOW_SESSION;
        break;
    case vp::PromptAnswer::Deny: taken = queue->answer(id, remote::Approval::deny()), value = core::answer::DENY; break;
    }
    if (auto node = cards_.extract(id)) {
        contracts::Answer recorded{{std::string(value)}, std::nullopt, ctx.services.node.key};
        finish(ctx, node.mapped(), taken ? DecisionStatus::Answered : DecisionStatus::Cancelled, taken ? &recorded : nullptr);
    }
    return taken;
}

std::expected<void, std::string> Remote::answer_card(const app_runtime::SessionId& session, core::NodeId decision,
                                                     const contracts::Answer& answer, const Ctx& ctx) {
    std::optional<std::uint64_t> id;
    for (const auto& [key, card] : cards_)
        if (card.session == session && card.decision == decision) {
            id = key;
            break;
        }
    const Card card{session, decision};
    if (!id) {
        finish(ctx, card, DecisionStatus::Cancelled, nullptr);
        return std::unexpected(std::string(NOT_WAITING));
    }
    cards_.erase(*id);
    const std::string value = answer.values.empty() ? std::string() : answer.values.front();
    std::optional<remote::Prompt> kind;
    for (auto& prompt : queue->pending())
        if (prompt.id == *id) {
            kind = std::move(prompt.prompt);
            break;
        }
    bool taken = false;
    if (kind) {
        if (std::holds_alternative<remote::HostKeyPrompt>(*kind)) {
            taken = queue->answer(*id, value == TRUST);
        } else {
            auto approval = remote::Approval::from_answer(value, answer.reason).value_or(remote::Approval::deny(answer.reason));
            taken = queue->answer(*id, std::move(approval));
        }
    }
    if (!taken) {
        finish(ctx, card, DecisionStatus::Cancelled, nullptr);
        return std::unexpected(std::string(NOT_WAITING));
    }
    finish(ctx, card, DecisionStatus::Answered, &answer);
    return {};
}

void Remote::cancel_stale(const app_runtime::SessionId& session, const Ctx& ctx) const {
    const auto* open = ctx.runtime.sessions().get(session.as_str());
    if (!open) return;
    auto stale = open->writer()->with_dom([&](const core::Dom& dom) {
        std::vector<core::NodeId> out;
        for (const auto& each : dom.iter()) {
            const auto* node = &each;
            if (node->kind != core::ElementKind::Decision) continue;
            const auto request = node->str_attr(attr::REQUEST_ID);
            if (!request || !request->starts_with(REQUEST_PREFIX)) continue;
            if (node->str_attr(attr::STATUS) != core::as_str(DecisionStatus::Waiting)) continue;
            const bool raised = std::ranges::any_of(cards_, [&](const auto& entry) {
                return entry.second.session == session && entry.second.decision == node->id;
            });
            if (!raised) out.push_back(node->id);
        }
        return out;
    });
    for (auto decision : stale) finish(ctx, Card{session, decision}, DecisionStatus::Cancelled, nullptr);
}

std::expected<std::vector<vp::AuditRow>, std::string> Remote::audit(const std::shared_ptr<remote::AuditSink>& audit, std::string host) {
    remote::AuditQuery query;
    query.host = std::move(host);
    query.limit = AUDIT_ROWS;
    auto records = audit->query(query);
    if (!records) return std::unexpected(records.error().message());
    std::vector<vp::AuditRow> rows;
    rows.reserve(records->size());
    for (const auto& record : *records) rows.push_back(audit_row(record));
    return rows;
}

std::pair<core::DecisionKind, core::Value> decision_options(const remote::Prompt& prompt) {
    if (std::holds_alternative<remote::HostKeyPrompt>(prompt))
        return {core::DecisionKind::Question, core::Value(std::vector<std::string>{std::string(TRUST), std::string(REJECT)})};
    return {core::DecisionKind::Approval,
            core::Value(std::vector<std::string>{std::string(core::answer::ALLOW_ONCE), std::string(core::answer::ALLOW_SESSION),
                                                 std::string(core::answer::DENY)})};
}

std::string title(const remote::Prompt& prompt) {
    return std::visit(overloaded{
                          [](const remote::HostKeyPrompt& p) {
                              if (p.change.kind == remote::HostKeyChange::Kind::new_key) return "Trust the host key of " + p.host + "?";
                              return "The host key of " + p.host + " CHANGED";
                          },
                          [](const remote::ApprovalRequest& r) {
                              switch (r.action) {
                              case remote::Action::command: return "Run a command on " + r.host + "?";
                              case remote::Action::write: return "Write a file on " + r.host + "?";
                              case remote::Action::edit: return "Edit a file on " + r.host + "?";
                              default: return "Allow " + std::string(remote::to_string(r.action)) + " on " + r.host + "?";
                              }
                          },
                      },
                      prompt);
}

std::string body(const remote::Prompt& prompt) {
    return std::visit(overloaded{
                          [](const remote::HostKeyPrompt& p) {
                              std::string out = remote::host_name(p.hostname, p.port) + " presented " + p.key_type + " key\n" + p.fingerprint;
                              if (p.change.kind == remote::HostKeyChange::Kind::new_key) {
                                  out += "\nNo key is on file for it yet.";
                              } else {
                                  out += "\nThis may be a man in the middle. Keys on file:";
                                  for (const auto& key : p.change.previous)
                                      out += "\n" + key.key_type + " " + key.fingerprint + " (" + key.file.string() + ":" +
                                             std::to_string(key.line) + ")";
                              }
                              return out;
                          },
                          [](const remote::ApprovalRequest& r) {
                              if (r.detail.empty()) return r.target;
                              return r.target + "\n" + r.detail;
                          },
                      },
                      prompt);
}

vp::RemotePrompt panel_prompt(const remote::PromptView& prompt) {
    const auto kind = std::holds_alternative<remote::HostKeyPrompt>(prompt.prompt) ? vp::PromptKind::HostKey : vp::PromptKind::Approval;
    return {prompt.id, kind, title(prompt.prompt), body(prompt.prompt)};
}

vp::AuditRow audit_row(const remote::AuditRecord& record) {
    vp::AuditAction action = vp::AuditAction::Read;
    switch (record.action) {
    case remote::Action::read: action = vp::AuditAction::Read; break;
    case remote::Action::list: action = vp::AuditAction::List; break;
    case remote::Action::write: action = vp::AuditAction::Write; break;
    case remote::Action::edit: action = vp::AuditAction::Edit; break;
    case remote::Action::command: action = vp::AuditAction::Command; break;
    case remote::Action::terminal: action = vp::AuditAction::Terminal; break;
    }
    std::string outcome(remote::to_string(record.outcome));
    if (!record.detail.empty()) outcome += ": " + record.detail;
    return {utc(record.at), record.session_id, action, record.target, std::move(outcome)};
}

std::string utc(std::chrono::system_clock::time_point at) {
    using namespace std::chrono;
    const auto since = at.time_since_epoch();
    const std::int64_t secs = since.count() < 0 ? 0 : duration_cast<seconds>(since).count();
    const year_month_day date{sys_days{days{secs / 86'400}}};
    const std::int64_t rest = secs % 86'400;
    char buf[48];
    std::snprintf(buf, sizeof buf, "%04d-%02u-%02u %02" PRId64 ":%02" PRId64 ":%02" PRId64 " UTC", static_cast<int>(date.year()),
                  static_cast<unsigned>(date.month()), static_cast<unsigned>(date.day()), rest / 3'600, rest / 60 % 60, rest % 60);
    return buf;
}

SshPaths SshPaths::from_env() {
    auto home = loa::runtime::fs::home().value_or(std::filesystem::path("/"));
    auto resolver = std::make_shared<remote::OpenSshCli>(home / ".ssh" / "config");
    return {std::move(home), std::move(resolver)};
}

} // namespace loa::app::areas::tools
