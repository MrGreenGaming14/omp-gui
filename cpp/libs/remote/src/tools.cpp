#include "loa/remote/tools.hpp"

#include <asio.hpp>

#include <functional>
#include <thread>

#include "loa/contracts/schema.hpp"
#include "wire.hpp"

namespace loa::remote {

namespace s = loa::contracts::schema;
using loa::contracts::Awaitable;
using loa::contracts::CallCtx;
using loa::contracts::Json;
using loa::contracts::ToolAnnotations;
using loa::contracts::ToolOutput;
using loa::contracts::ToolResult;

namespace {

// serde's `Unexpected` text for a JSON value.
std::string unexpected_text(const Json& v) {
    if (v.is_null()) return "null";
    if (const auto* b = v.as_bool()) return std::string("boolean `") + (*b ? "true" : "false") + "`";
    if (const auto* str = v.as_string()) return "string \"" + *str + "\"";
    if (v.is_array()) return "sequence";
    if (v.is_object()) return "map";
    if (const auto i = v.as_i64()) return "integer `" + std::to_string(*i) + "`";
    if (const auto u = v.as_u64()) return "integer `" + std::to_string(*u) + "`";
    return "floating point `" + loa::runtime::json::dump(v) + "`";
}

// A unit enum field: `nullopt` when absent (or null and `optional`).
template <typename E>
std::expected<std::optional<E>, std::string> enum_field(const Json& json, std::string_view field, std::string_view type,
                                                        std::initializer_list<std::pair<std::string_view, E>> variants, bool optional) {
    const auto* v = json.find(field);
    if (!v || (optional && v->is_null())) return std::optional<E>{};
    const auto* str = v->as_string();
    if (!str) return std::unexpected("invalid type: " + unexpected_text(*v) + ", expected enum " + std::string(type));
    std::string expected;
    std::size_t i = 0;
    for (const auto& [name, value] : variants) {
        if (*str == name) return std::optional<E>{value};
        expected += (i++ == 0 ? "`" : ", `") + std::string(name) + "`";
    }
    return std::unexpected("unknown variant `" + *str + "`, expected one of " + expected);
}

Json nullable_default(Json schema) { return s::with_default(s::nullable(std::move(schema)), Json()); }

// Runs `work` on its own thread; cancelling the awaiting coroutine cancels its token, so the
// cancellation is still audited.
template <typename T>
Awaitable<T> detached(std::function<T(const CancelToken&)> work) {
    co_return co_await asio::async_initiate<const asio::use_awaitable_t<>&, void(T)>(
        [](auto handler, std::function<T(const CancelToken&)> work) {
            CancelToken cancel;
            auto slot = asio::get_associated_cancellation_slot(handler);
            if (slot.is_connected()) slot.assign([cancel](asio::cancellation_type) { cancel.cancel(); });
            auto guard = asio::make_work_guard(asio::get_associated_executor(handler));
            std::thread([handler = std::move(handler), work = std::move(work), cancel, guard = std::move(guard)]() mutable {
                T result = work(cancel);
                auto executor = asio::get_associated_executor(handler);
                asio::post(executor, [handler = std::move(handler), result = std::move(result)]() mutable {
                    auto slot = asio::get_associated_cancellation_slot(handler);
                    if (slot.is_connected()) slot.clear();
                    std::move(handler)(std::move(result));
                });
            }).detach();
        },
        asio::use_awaitable, std::move(work));
}

// The JSON the model reads; an error result when `failed` or on error.
ToolResult output(const Result<Json>& result, bool failed) {
    if (!result) return ToolOutput::error(result.error().message());
    auto text = loa::runtime::json::dump_pretty(*result);
    return failed ? ToolOutput::error(std::move(text)) : ToolOutput::text(std::move(text));
}

constexpr ToolAnnotations hints() { return ToolAnnotations{std::nullopt, false, true}; }

} // namespace

std::string BashArgs::schema() {
    auto object = s::Object()
                      .description("Arguments of `remote_bash`.")
                      .property("host", s::describe(s::string(), "Host name from the Remote hosts list."))
                      .property("command", s::describe(s::string(), "Shell command, run by the remote user's login shell."))
                      .property("cwd", s::describe(nullable_default(s::string()), "Absolute directory to run the command in."), false)
                      .property("timeout_secs", s::describe(nullable_default(s::unsigned_integer("uint64")), "Seconds before the command is stopped."),
                                false);
    return s::Root("BashArgs", std::move(object)).dump();
}

std::expected<BashArgs, std::string> BashArgs::from_json(const Json& json) {
    namespace a = loa::contracts::args;
    if (auto ok = a::object(json, "struct BashArgs"); !ok) return std::unexpected(ok.error());
    BashArgs out;
    auto host = a::string(json, "host");
    if (!host) return std::unexpected(host.error());
    auto command = a::string(json, "command");
    if (!command) return std::unexpected(command.error());
    auto cwd = a::optional_string(json, "cwd");
    if (!cwd) return std::unexpected(cwd.error());
    auto timeout = a::optional_u64(json, "timeout_secs");
    if (!timeout) return std::unexpected(timeout.error());
    return BashArgs{std::move(*host), std::move(*command), std::move(*cwd), *timeout};
}

std::string FileArgs::schema() {
    auto object = s::Object()
                      .description("Arguments of `remote_file`.")
                      .property("host", s::describe(s::string(), "Host name from the Remote hosts list."))
                      .property("op", s::ref("FileOpName"))
                      .property("path", s::describe(s::string(), "Absolute path on the host."))
                      .property("content", s::describe(nullable_default(s::string()), "`write`: the new file content."), false)
                      .property("encoding", s::describe(s::nullable(s::ref("Encoding")), "`write`: how `content` is encoded. Defaults to `utf8`."),
                                false)
                      .property("old", s::describe(nullable_default(s::string()), "`edit`: exact text to replace; must occur exactly once."), false)
                      .property("new", s::describe(nullable_default(s::string()), "`edit`: replacement text."), false);
    return s::Root("FileArgs", std::move(object))
        .define("FileOpName", s::describe(s::string_enum({"read", "write", "list", "edit"}), "What `remote_file` does."))
        .define("Encoding", s::describe(s::string_enum({"utf8", "base64"}), "How `content` is encoded."))
        .dump();
}

std::expected<FileArgs, std::string> FileArgs::from_json(const Json& json) {
    namespace a = loa::contracts::args;
    if (auto ok = a::object(json, "struct FileArgs"); !ok) return std::unexpected(ok.error());
    FileArgs out;
    auto host = a::string(json, "host");
    if (!host) return std::unexpected(host.error());
    out.host = std::move(*host);
    auto op = enum_field<FileOpName>(json, "op", "FileOpName",
                                     {{"read", FileOpName::read}, {"write", FileOpName::write}, {"list", FileOpName::list}, {"edit", FileOpName::edit}},
                                     false);
    if (!op) return std::unexpected(op.error());
    if (!*op) return std::unexpected(std::string("missing field `op`"));
    out.op = **op;
    auto path = a::string(json, "path");
    if (!path) return std::unexpected(path.error());
    out.path = std::move(*path);
    auto content = a::optional_string(json, "content");
    if (!content) return std::unexpected(content.error());
    out.content = std::move(*content);
    auto encoding = enum_field<Encoding>(json, "encoding", "Encoding", {{"utf8", Encoding::utf8}, {"base64", Encoding::base64}}, true);
    if (!encoding) return std::unexpected(encoding.error());
    out.encoding = *encoding;
    auto old_text = a::optional_string(json, "old");
    if (!old_text) return std::unexpected(old_text.error());
    out.old_text = std::move(*old_text);
    auto new_text = a::optional_string(json, "new");
    if (!new_text) return std::unexpected(new_text.error());
    out.new_text = std::move(*new_text);
    return out;
}

std::expected<FileRequest, std::string> FileArgs::into_request() && {
    const auto missing = [](std::string_view field, std::string_view op) {
        return std::unexpected("`" + std::string(field) + "` is required for op `" + std::string(op) + "`");
    };
    FileOp op;
    switch (this->op) {
    case FileOpName::read: op = ReadOp{}; break;
    case FileOpName::list: op = ListOp{}; break;
    case FileOpName::write: {
        if (!content) return missing("content", "write");
        if (encoding.value_or(Encoding::utf8) == Encoding::base64) {
            std::string error;
            auto decoded = wire::base64_decode(*content, &error);
            if (!decoded) return std::unexpected("`content` is not valid base64: " + error);
            op = WriteOp{std::move(*decoded)};
        } else {
            op = WriteOp{std::move(*content)};
        }
        break;
    }
    case FileOpName::edit:
        if (!old_text) return missing("old", "edit");
        if (!new_text) return missing("new", "edit");
        op = EditOp{std::move(*old_text), std::move(*new_text)};
        break;
    }
    return FileRequest{std::move(host), std::move(path), std::move(op)};
}

std::string_view RemoteBash::description() const {
    return "Run a shell command on a configured SSH host. Returns stdout, stderr and the exit status. Subject to the host's limits and the "
           "user's approval.";
}

ToolAnnotations RemoteBash::annotations() const { return hints(); }

Awaitable<ToolResult> RemoteBash::run(const CallCtx& ctx, BashArgs a) const {
    auto service = service_;
    auto session = ctx.session_id;
    BashRequest request{std::move(a.host), std::move(a.command), std::move(a.cwd), a.timeout_secs};
    auto result = co_await detached<Result<ExecOutput>>(
        [service, session, request](const CancelToken& cancel) { return service->bash(session, request, cancel); });
    if (!result) co_return output(std::unexpected(result.error()), true);
    co_return output(result->to_json(), result->exit_status != std::optional<std::uint32_t>(0));
}

std::string_view RemoteFile::description() const {
    return "Read, list, write or edit a file on a configured SSH host. `edit` replaces one exact, unique occurrence of `old` with `new`. "
           "Writes are atomic where the server allows.";
}

ToolAnnotations RemoteFile::annotations() const { return hints(); }

Awaitable<ToolResult> RemoteFile::run(const CallCtx& ctx, FileArgs args) const {
    auto request = std::move(args).into_request();
    if (!request) co_return std::unexpected(request.error());
    auto service = service_;
    auto session = ctx.session_id;
    auto result = co_await detached<Result<FileOutput>>(
        [service, session, request = std::move(*request)](const CancelToken& cancel) { return service->file(session, request, cancel); });
    if (!result) co_return output(std::unexpected(result.error()), false);
    co_return output(to_json(*result), false);
}

loa::contracts::ToolSet tool_set(std::shared_ptr<RemoteService> service) {
    loa::contracts::ToolSet set;
    set.add(std::make_shared<RemoteBash>(service)).add(std::make_shared<RemoteFile>(std::move(service)));
    return set;
}

} // namespace loa::remote
