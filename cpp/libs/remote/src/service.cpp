#include "loa/remote/service.hpp"

#include <algorithm>
#include <cstdio>

#include "loa/remote/limits.hpp"
#include "loa/runtime/exec.hpp"
#include "ops.hpp"
#include "sftp.hpp"
#include "text.hpp"
#include "wire.hpp"

namespace loa::remote {

using detail::FileAttributes;
using detail::Sftp;
using loa::runtime::json::Object;
using loa::runtime::json::Value;

std::string_view to_string(EntryKind kind) noexcept {
    switch (kind) {
    case EntryKind::file: return "file";
    case EntryKind::dir: return "dir";
    case EntryKind::symlink: return "symlink";
    case EntryKind::other: return "other";
    }
    return "other";
}

std::string_view to_string(WriteMode mode) noexcept { return mode == WriteMode::atomic ? "atomic" : "in_place"; }

namespace {

// The `Debug` name serde's snake_case `WriteMode` has in Rust (`Atomic`, `InPlace`).
std::string_view debug_name(WriteMode mode) noexcept { return mode == WriteMode::atomic ? "Atomic" : "InPlace"; }

template <typename T>
Value opt(const std::optional<T>& v) {
    return v ? Value(static_cast<std::uint64_t>(*v)) : Value();
}

Value entry_json(const DirEntry& e) {
    Object o;
    o.emplace_back("name", e.name);
    o.emplace_back("kind", std::string(to_string(e.kind)));
    o.emplace_back("size", opt(e.size));
    o.emplace_back("mtime", opt(e.mtime));
    o.emplace_back("permissions", opt(e.permissions));
    return Value(std::move(o));
}

DirEntry dir_entry(std::string name, const FileAttributes& a) {
    const auto kind = a.is_symlink() ? EntryKind::symlink : a.is_dir() ? EntryKind::dir : a.is_regular() ? EntryKind::file : EntryKind::other;
    std::optional<std::uint32_t> permissions;
    if (a.permissions) permissions = *a.permissions & 07777;
    return DirEntry{std::move(name), kind, a.size, a.mtime, permissions};
}

// What an audit row says about a successful result.
std::pair<Outcome, std::string> summary(const ExecOutput& out) {
    const auto outcome = out.end == ExecEnd::exited ? Outcome::ok : out.end == ExecEnd::timed_out ? Outcome::timed_out : Outcome::cancelled;
    std::string status;
    if (out.exit_status) status = "exit " + std::to_string(*out.exit_status);
    else if (out.exit_signal) status = "signal " + *out.exit_signal;
    else status = "no exit status";
    const auto o = out.stdout_text.size() + out.stdout_dropped;
    const auto e = out.stderr_text.size() + out.stderr_dropped;
    return {outcome, status + "; stdout " + std::to_string(o) + " bytes, stderr " + std::to_string(e) + " bytes"};
}

std::pair<Outcome, std::string> summary(const FileOutput& out) {
    return std::visit(
        [](const auto& v) -> std::pair<Outcome, std::string> {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, ReadOutput>) return {Outcome::ok, std::to_string(v.size) + " bytes"};
            else if constexpr (std::is_same_v<T, ListOutput>) return {Outcome::ok, std::to_string(v.entries.size()) + " entries"};
            else return {Outcome::ok, std::to_string(v.bytes) + " bytes, " + std::string(debug_name(v.mode))};
        },
        out);
}

std::pair<Outcome, std::string> summary(const std::pair<Shell, std::string>& shell) { return {Outcome::ok, shell.second}; }

Outcome outcome_of(const Error& e) {
    switch (e.kind()) {
    case ErrorKind::refused:
    case ErrorKind::unknown_host: return Outcome::refused;
    case ErrorKind::denied: return Outcome::denied;
    case ErrorKind::cancelled: return Outcome::cancelled;
    case ErrorKind::timed_out: return Outcome::timed_out;
    default: return Outcome::error;
    }
}

// Rust's `{:?}` of a `&str`.
std::string debug_str(std::string_view s) {
    std::string out = "\"";
    for (const char c : s) {
        switch (c) {
        case '"': out += "\\\""; break;
        case '\\': out += "\\\\"; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        case '\0': out += "\\0"; break;
        default:
            if (static_cast<unsigned char>(c) < 0x20 || c == 0x7f) {
                char buf[16];
                std::snprintf(buf, sizeof buf, "\\u{%x}", static_cast<unsigned>(static_cast<unsigned char>(c)));
                out += buf;
            } else {
                out += c;
            }
        }
    }
    return out + "\"";
}

Error refused(std::string reason) { return Error::refused(std::move(reason)); }

// Resolves `path` on the host and checks the real path against the (resolved) allowed prefixes,
// so a symlink cannot lead outside them. A write to a missing file resolves its parent instead.
Result<std::string> real_target(const Host& host, Sftp& sftp, const std::string& path, bool creating, std::string& target) {
    std::string real;
    std::optional<FileAttributes> existing;
    if (creating) {
        auto st = sftp.stat(path, true);
        if (!st) return std::unexpected(st.error());
        existing = *st;
    }
    if (creating && !existing) {
        const auto [dir, name] = split(path);
        auto parent = sftp.realpath(std::string(dir));
        if (!parent) return std::unexpected(parent.error());
        auto trimmed = std::string_view(*parent);
        while (trimmed.ends_with('/')) trimmed.remove_suffix(1);
        real = std::string(trimmed) + "/" + std::string(name);
    } else {
        auto resolved = sftp.realpath(path);
        if (!resolved) return std::unexpected(resolved.error());
        real = std::move(*resolved);
    }
    auto normalized = normalize(real);
    if (!normalized) return std::unexpected(Error::sftp(host.name, "server returned an unusable real path: " + normalized.error()));
    real = std::move(*normalized);
    if (real != path) target = path + " -> " + real;
    if (host.limits.allows_every_path()) return real;
    std::vector<std::string> prefixes;
    for (auto& prefix : host.limits.prefixes()) {
        auto p = sftp.realpath(prefix);
        if (p) {
            if (auto n = normalize(*p)) prefixes.push_back(std::move(*n));
        } else if (!detail::is_server_status(p.error())) {
            return std::unexpected(p.error());
        }
        prefixes.push_back(std::move(prefix));
    }
    if (std::ranges::any_of(prefixes, [&](const std::string& p) { return within(real, p); })) return real;
    return std::unexpected(refused(path + " resolves to " + real + ", outside the allowed paths"));
}

} // namespace

Value to_json(const FileOutput& output) {
    return std::visit(
        [](const auto& v) -> Value {
            using T = std::decay_t<decltype(v)>;
            Object o;
            if constexpr (std::is_same_v<T, ReadOutput>) {
                o.emplace_back("op", "read");
                o.emplace_back("path", v.path);
                o.emplace_back("encoding", v.encoding);
                o.emplace_back("content", v.content);
                o.emplace_back("size", Value(v.size));
            } else if constexpr (std::is_same_v<T, ListOutput>) {
                o.emplace_back("op", "list");
                o.emplace_back("path", v.path);
                loa::runtime::json::Array entries;
                for (const auto& e : v.entries) entries.push_back(entry_json(e));
                o.emplace_back("entries", Value(std::move(entries)));
            } else {
                o.emplace_back("op", std::is_same_v<T, WriteOutput> ? "write" : "edit");
                o.emplace_back("path", v.path);
                o.emplace_back("bytes", Value(v.bytes));
                o.emplace_back("mode", std::string(to_string(v.mode)));
            }
            return Value(std::move(o));
        },
        output);
}

std::expected<std::string, std::string> apply_edit(std::string_view text, std::string_view old_text, std::string_view new_text) {
    if (old_text.empty()) return std::unexpected(std::string("edit needs a non-empty `old` string"));
    std::size_t count = 0;
    std::size_t first = std::string_view::npos;
    for (auto at = text.find(old_text); at != std::string_view::npos; at = text.find(old_text, at + old_text.size())) {
        if (count++ == 0) first = at;
    }
    if (count == 0) return std::unexpected(std::string("`old` string not found"));
    if (count > 1) return std::unexpected("`old` string occurs " + std::to_string(count) + " times; it must be unique");
    std::string out(text.substr(0, first));
    out += new_text;
    out += text.substr(first + old_text.size());
    return out;
}

std::filesystem::path system_known_hosts([[maybe_unused]] std::optional<std::string> program_data) {
#if defined(_WIN32)
    std::filesystem::path base = program_data && !program_data->empty() ? std::filesystem::path(*program_data)
                                                                        : std::filesystem::path(R"(C:\ProgramData)");
    return base / "ssh" / "ssh_known_hosts";
#else
    return "/etc/ssh/ssh_known_hosts";
#endif
}

RemoteServiceConfig RemoteServiceConfig::from_home(const std::filesystem::path& home) {
    const auto user = home / ".ssh/known_hosts";
    std::optional<std::string> program_data;
    if (const char* v = std::getenv("ProgramData")) program_data = v;
    return RemoteServiceConfig{{user, home / ".ssh/known_hosts2", system_known_hosts(program_data)}, user, ConnectorConfig::from_env()};
}

RemoteService::RemoteService(std::shared_ptr<HostRegistry> hosts, std::shared_ptr<AuditSink> audit, std::shared_ptr<HostKeyDecider> decider,
                             std::shared_ptr<Approver> approver, RemoteServiceConfig config)
    : hosts_(std::move(hosts)), audit_(std::move(audit)), approver_(std::move(approver)) {
    auto known = std::make_shared<KnownHosts>(std::move(config.known_hosts), std::move(config.known_hosts_write));
    connector_ = std::make_unique<Connector>(std::move(known), std::move(decider), std::move(config.connector));
}

void RemoteService::disconnect(const std::string& host) { connector_->disconnect(host); }

void RemoteService::end_session(std::string_view session_id) {
    std::lock_guard lock(grants_mu_);
    std::erase_if(grants_, [&](const auto& g) { return std::get<0>(g) == session_id; });
}

template <typename T>
Result<T> RemoteService::audited(std::string_view session, std::string_view host, Action action, std::string_view target, Result<T> result) {
    auto [outcome, detail] = result ? summary(*result) : std::pair{outcome_of(result.error()), result.error().message()};
    auto recorded = audit_->record(NewRecord{std::chrono::system_clock::now(), std::string(host), std::string(session), action,
                                             std::string(target), outcome, std::move(detail)});
    if (!recorded) return std::unexpected(recorded.error());
    return result;
}

Result<Host> RemoteService::host(const std::string& name) const {
    if (auto h = hosts_->get(name)) return std::move(*h);
    return std::unexpected(Error::unknown_host(name));
}

Result<void> RemoteService::approve(std::string_view session_id, const Host& host, Action action, std::string_view target, std::string detail,
                                    const CancelToken& cancel) {
    if (!host.limits.needs_approval(action)) return {};
    auto key = std::tuple{std::string(session_id), host.name, action == Action::command};
    {
        std::lock_guard lock(grants_mu_);
        if (grants_.contains(key)) return {};
    }
    auto answer = approver_->approve(ApprovalRequest{std::string(session_id), host.name, action, std::string(target), std::move(detail)}, cancel);
    if (cancel.cancelled()) return std::unexpected(Error::cancelled());
    switch (answer.kind) {
    case Approval::Kind::allow_once: return {};
    case Approval::Kind::allow_session: {
        std::lock_guard lock(grants_mu_);
        grants_.insert(std::move(key));
        return {};
    }
    case Approval::Kind::deny: break;
    }
    return std::unexpected(Error::denied(std::move(answer.reason)));
}

Result<ExecOutput> RemoteService::bash(std::string_view session_id, const BashRequest& request, const CancelToken& cancel) {
    const auto command = request.cwd ? "cd " + loa::runtime::exec::shell_quote(*request.cwd) + " && " + request.command : request.command;
    auto result = bash_inner(session_id, request, command, cancel);
    return audited(session_id, request.host, Action::command, command, std::move(result));
}

Result<ExecOutput> RemoteService::bash_inner(std::string_view session_id, const BashRequest& request, const std::string& command,
                                             const CancelToken& cancel) {
    auto host = this->host(request.host);
    if (!host) return std::unexpected(host.error());
    const auto& limits = host->limits;
    if (auto ok = limits.check_action(Action::command); !ok) return std::unexpected(refused(ok.error()));
    if (std::ranges::all_of(request.command, [](char c) { return std::isspace(static_cast<unsigned char>(c)) != 0; })) {
        return std::unexpected(Error::invalid_request("command is empty"));
    }
    if (request.cwd) {
        auto normalized = normalize(*request.cwd);
        if (!normalized) return std::unexpected(refused(normalized.error()));
        if (auto ok = limits.check_path(*normalized); !ok) return std::unexpected(refused(ok.error()));
    }
    const auto secs = std::max<std::uint64_t>(std::min(request.timeout_secs.value_or(limits.default_timeout_secs), limits.max_timeout_secs), 1);
    auto detail = request.cwd ? "in " + *request.cwd + ", timeout " + std::to_string(secs) + "s" : "timeout " + std::to_string(secs) + "s";
    if (cancel.cancelled()) return std::unexpected(Error::cancelled());
    if (auto ok = approve(session_id, *host, Action::command, request.command, std::move(detail), cancel); !ok) return std::unexpected(ok.error());
    auto session = connector_->session(*host, cancel);
    if (cancel.cancelled()) return std::unexpected(Error::cancelled());
    if (!session) return std::unexpected(session.error());
    if (request.cwd && !limits.allows_every_path()) {
        auto sftp = (*session)->sftp();
        if (!sftp) return std::unexpected(sftp.error());
        std::string ignored;
        if (auto real = real_target(*host, **sftp, *request.cwd, false, ignored); !real) return std::unexpected(real.error());
    }
    return detail::run(*(*session)->link, host->name, command, limits.max_output_bytes, std::chrono::seconds(secs), cancel);
}

Result<Shell> RemoteService::open_shell(const std::string& host_name, std::uint16_t cols, std::uint16_t rows) {
    auto result = [&]() -> Result<std::pair<Shell, std::string>> {
        auto host = this->host(host_name);
        if (!host) return std::unexpected(host.error());
        auto session = connector_->session(*host, CancelToken());
        if (!session) return std::unexpected(session.error());
        auto shell = detail::open_shell(*session, cols, rows);
        if (!shell) return std::unexpected(shell.error());
        return std::pair{std::move(*shell), std::to_string(cols) + "x" + std::to_string(rows)};
    }();
    auto audited_result = audited(TERMINAL_SESSION, host_name, Action::terminal, "login shell", std::move(result));
    if (!audited_result) return std::unexpected(audited_result.error());
    return std::move(audited_result->first);
}

Result<FileOutput> RemoteService::file(std::string_view session_id, const FileRequest& request, const CancelToken& cancel) {
    const auto action = std::visit(
        [](const auto& op) {
            using T = std::decay_t<decltype(op)>;
            if constexpr (std::is_same_v<T, ReadOp>) return Action::read;
            else if constexpr (std::is_same_v<T, ListOp>) return Action::list;
            else if constexpr (std::is_same_v<T, WriteOp>) return Action::write;
            else return Action::edit;
        },
        request.op);
    std::string target = request.path;
    auto result = file_inner(session_id, request, action, target, cancel);
    if (!result && cancel.cancelled()) result = std::unexpected(Error::cancelled());
    return audited(session_id, request.host, action, target, std::move(result));
}

Result<FileOutput> RemoteService::file_inner(std::string_view session_id, const FileRequest& request, Action action, std::string& target,
                                             const CancelToken& cancel) {
    auto host = this->host(request.host);
    if (!host) return std::unexpected(host.error());
    if (auto ok = host->limits.check_action(action); !ok) return std::unexpected(refused(ok.error()));
    auto normalized = normalize(request.path);
    if (!normalized) return std::unexpected(refused(normalized.error()));
    const auto path = std::move(*normalized);
    target = path;
    if (auto ok = host->limits.check_path(path); !ok) return std::unexpected(refused(ok.error()));
    const auto max = host->limits.max_file_bytes;
    if (const auto* w = std::get_if<WriteOp>(&request.op); w && w->content.size() > max) {
        return std::unexpected(Error::too_large(path, w->content.size(), max));
    }
    if (const auto* e = std::get_if<EditOp>(&request.op); e && e->old_text.empty()) {
        return std::unexpected(Error::invalid_request("edit needs a non-empty `old` string"));
    }
    if (cancel.cancelled()) return std::unexpected(Error::cancelled());
    auto session = connector_->session(*host, cancel);
    if (!session) return std::unexpected(session.error());

    auto result = [&]() -> Result<FileOutput> {
        auto opened = (*session)->sftp();
        if (!opened) return std::unexpected(opened.error());
        auto& sftp = **opened;
        const bool creating = std::holds_alternative<WriteOp>(request.op);
        auto real = real_target(*host, sftp, path, creating, target);
        if (!real) return std::unexpected(real.error());
        const auto& p = *real;
        if (std::holds_alternative<ReadOp>(request.op)) {
            auto bytes = sftp.read(p, max);
            if (!bytes) return std::unexpected(bytes.error());
            const auto size = static_cast<std::uint64_t>(bytes->size());
            if (text::valid_utf8(*bytes)) return ReadOutput{p, "utf8", std::move(*bytes), size};
            return ReadOutput{p, "base64", wire::base64_encode(*bytes), size};
        }
        if (std::holds_alternative<ListOp>(request.op)) {
            auto listed = sftp.list(p);
            if (!listed) return std::unexpected(listed.error());
            ListOutput out{p, {}};
            for (auto& [name, attrs] : *listed) out.entries.push_back(dir_entry(std::move(name), attrs));
            return out;
        }
        if (const auto* w = std::get_if<WriteOp>(&request.op)) {
            if (auto ok = approve(session_id, *host, Action::write, p, std::to_string(w->content.size()) + " bytes", cancel); !ok) {
                return std::unexpected(ok.error());
            }
            auto mode = sftp.write(p, w->content);
            if (!mode) return std::unexpected(mode.error());
            return WriteOutput{p, w->content.size(), *mode};
        }
        const auto& e = std::get<EditOp>(request.op);
        auto bytes = sftp.read(p, max);
        if (!bytes) return std::unexpected(bytes.error());
        if (!text::valid_utf8(*bytes)) return std::unexpected(Error::edit(p, "file is not UTF-8 text"));
        auto edited = apply_edit(*bytes, e.old_text, e.new_text);
        if (!edited) return std::unexpected(Error::edit(p, edited.error()));
        if (edited->size() > max) return std::unexpected(Error::too_large(p, edited->size(), max));
        auto preview = "replace " + debug_str(text::prefix_chars(e.old_text, 200)) + " with " + debug_str(text::prefix_chars(e.new_text, 200));
        if (auto ok = approve(session_id, *host, Action::edit, p, std::move(preview), cancel); !ok) return std::unexpected(ok.error());
        auto mode = sftp.write(p, *edited);
        if (!mode) return std::unexpected(mode.error());
        return EditOutput{p, edited->size(), *mode};
    }();
    if (!result && result.error().kind() == ErrorKind::sftp && !detail::is_server_status(result.error())) {
        // The channel itself failed; open a fresh one next time.
        (*session)->reset_sftp();
    }
    return result;
}

} // namespace loa::remote
