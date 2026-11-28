#include "loa/harness/opencode/native.hpp"

#include <algorithm>
#include <cerrno>
#include <cstring>
#include <fstream>
#include <iterator>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

#include "loa/core/schema.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/opencode/ids.hpp"
#include "loa/harness/opencode/json.hpp"
#include "loa/harness/opencode/mapper.hpp"

namespace loa::harness::opencode {
namespace {
namespace json = loa::runtime::json;
using namespace std::string_view_literals;
namespace attr = loa::core::attr;
using loa::contracts::AdapterError;
using loa::contracts::Result;
using loa::core::Dom;
using loa::core::ElementKind;
using loa::core::Node;
using loa::core::NodeId;
using json::Value;

AdapterError io_error(int err) {
    const ErrorCode code = err == ENOENT ? ErrorCode::not_found
                           : err == EACCES || err == EPERM ? ErrorCode::permission_denied : ErrorCode::io;
    return AdapterError::Io{loa::Error(code, std::string(std::strerror(err)) + " (os error " + std::to_string(err) + ")")};
}

Result<Value> object(std::string_view text, const Node& node, std::string_view name) {
    auto value = json_util::parse_object(text);
    if (!value) return std::unexpected(AdapterError::Format{std::string(name) + " " + std::to_string(node.id.value) + ": " + value.error().message()});
    return std::move(*value);
}

Result<Value> oc(const Node& node) { return object(node.str_attr("x-oc").value_or(""), node, "x-oc of node"); }

void put(Value& value, std::string_view ptr, Value leaf) { json_util::put(value, ptr, std::move(leaf)); }

std::string_view str(const Node& node, std::string_view name) { return node.str_attr(name).value_or(""); }

std::pair<std::string_view, std::string_view> split_model(std::string_view model) {
    const auto slash = model.find('/');
    return slash == std::string_view::npos ? std::pair{""sv, model} : std::pair{model.substr(0, slash), model.substr(slash + 1)};
}

struct Out {
    std::string id;
    Value info;
    json::Array parts;
};

class Context {
  public:
    std::string_view session_id;
    std::string_view model;
    std::string_view cwd;

    Result<> turn(const Dom& dom, const Node& turn, json::Array& messages) const {
        const auto started = turn.int_attr(attr::STARTED).value_or(0);
        const std::string user_id = turn.str_attr(attr::ID).transform([](std::string_view s) { return std::string(s); })
                                        .value_or(ids::ascending("msg", started));
        auto user = oc(turn);
        if (!user) return std::unexpected(std::move(user.error()));
        put(*user, "/id", user_id);
        put(*user, "/sessionID", session_id);
        put(*user, "/role", "user");
        put(*user, "/time/created", started);
        if (!turn.attr("x-oc")) {
            const auto [provider, model_name] = split_model(model);
            put(*user, "/agent", "build");
            put(*user, "/model", json::Object{{"providerID", provider}, {"modelID", model_name}});
        }
        std::vector<Out> out;
        out.push_back(Out{user_id, std::move(*user), {}});
        std::optional<std::size_t> synthetic;
        for (const Node* node : dom.children(turn.id)) {
            const auto owner_id = node->str_attr("x-message");
            if (node->kind == ElementKind::Ext && node->str_attr(attr::TYPE) == "message") {
                if (!owner_id) return std::unexpected(AdapterError::Format{"message ext " + std::to_string(node->id.value) + " has no x-message"});
                auto info = object(node->text, *node, "ext");
                if (!info) return std::unexpected(std::move(info.error()));
                put(*info, "/id", *owner_id);
                put(*info, "/sessionID", session_id);
                out.push_back(Out{std::string(*owner_id), std::move(*info), {}});
                continue;
            }
            std::size_t message = 0;
            Value part;
            if (node->kind == ElementKind::Ext && node->str_attr(attr::TYPE) == "part" &&
                node->str_attr(attr::HARNESS) == "opencode") {
                message = owner(out, synthetic, owner_id, user_id, started, node->id);
                auto parsed = object(node->text, *node, "ext");
                if (!parsed) return std::unexpected(std::move(parsed.error()));
                part = std::move(*parsed);
                put(part, "/id", str(*node, "x-part"));
            } else if (node->kind == ElementKind::Message) {
                message = node->str_attr(attr::ROLE) == "user" && !owner_id ? 0
                    : owner(out, synthetic, owner_id, user_id, started, node->id);
                const bool thinking = node->str_attr(attr::KIND) == "thinking";
                auto parsed = oc(*node);
                if (!parsed) return std::unexpected(std::move(parsed.error()));
                part = std::move(*parsed);
                const std::string id = node->str_attr(attr::ID).transform([](std::string_view s) { return std::string(s); })
                                           .value_or(ids::ascending("prt", started));
                put(part, "/id", id);
                put(part, "/type", thinking ? "reasoning" : "text");
                put(part, "/text", node->text);
                if (thinking && !node->attr("x-oc")) put(part, "/time/start", started);
            } else if (node->kind == ElementKind::Call) {
                message = owner(out, synthetic, owner_id, user_id, started, node->id);
                auto tool = call(dom, *node);
                if (!tool) return std::unexpected(std::move(tool.error()));
                part = std::move(*tool);
            } else {
                continue;
            }
            put(part, "/sessionID", session_id);
            put(part, "/messageID", out[message].id);
            out[message].parts.push_back(std::move(part));
        }
        for (auto& m : out) messages.emplace_back(json::Object{{"info", std::move(m.info)}, {"parts", std::move(m.parts)}});
        return {};
    }

  private:
    std::size_t owner(std::vector<Out>& out, std::optional<std::size_t>& synthetic,
                      std::optional<std::string_view> id, std::string_view user_id,
                      std::int64_t started, NodeId node) const {
        if (id) {
            const auto it = std::find_if(out.begin(), out.end(), [id](const Out& m) { return m.id == *id; });
            if (it != out.end()) return static_cast<std::size_t>(it - out.begin());
        }
        if (synthetic) return *synthetic;
        const auto [provider, model_name] = split_model(model);
        const std::string generated = ids::ascending("msg", started + static_cast<std::int64_t>(node.value));
        Value info = json::Object{
            {"id", generated}, {"sessionID", session_id}, {"role", "assistant"},
            {"time", json::Object{{"created", started}}}, {"parentID", user_id},
            {"modelID", model_name}, {"providerID", provider}, {"mode", "build"}, {"agent", "build"},
            {"path", json::Object{{"cwd", cwd}, {"root", cwd}}}, {"cost", 0},
            {"tokens", json::Object{{"input", 0}, {"output", 0}, {"reasoning", 0},
                                    {"cache", json::Object{{"read", 0}, {"write", 0}}}}},
        };
        out.push_back(Out{generated, std::move(info), {}});
        synthetic = out.size() - 1;
        return *synthetic;
    }

    static Result<Value> call(const Dom& dom, const Node& node) {
        auto parsed = oc(node);
        if (!parsed) return std::unexpected(std::move(parsed.error()));
        Value part = std::move(*parsed);
        const auto started = node.int_attr(attr::STARTED);
        const std::string id = node.str_attr("x-part").transform([](std::string_view s) { return std::string(s); })
                                   .value_or(ids::ascending("prt", started.value_or(0)));
        std::string status;
        if (const auto* native = json_util::str_at(part, "/state/status")) status = *native;
        else if (const auto current = node.str_attr(attr::STATUS)) {
            status = *current == "pending" ? "pending" : *current == "running" ? "running"
                     : *current == "error" || *current == "cancelled" ? "error" : "completed";
        } else status = "completed";
        put(part, "/id", id);
        put(part, "/type", "tool");
        put(part, "/callID", node.str_attr(attr::CALL_ID).value_or(id));
        put(part, "/tool", str(node, attr::TOOL));
        put(part, "/state/status", status);
        const auto text = [&](ElementKind kind) -> std::string_view {
            auto kids = dom.children_of_kind(node.id, kind);
            return kids.empty() ? std::string_view{} : std::string_view(kids.front()->text);
        };
        const auto input = text(ElementKind::Input);
        if (status == "pending") put(part, "/state/raw", input);
        else {
            auto decoded = json_util::parse_object(input);
            put(part, "/state/input", decoded ? std::move(*decoded) : Value(json::Object{{"raw", input}}));
        }
        if (auto title = node.str_attr(attr::TITLE)) put(part, "/state/title", *title);
        if (status == "completed") put(part, "/state/output", text(ElementKind::Result));
        else if (status == "error") put(part, "/state/error", text(ElementKind::Result));
        if (!node.attr("x-oc")) {
            const auto start = started.value_or(0);
            if (status == "pending") put(part, "/state/input", json::Object{});
            else {
                put(part, "/state/time/start", start);
                if (status != "running") put(part, "/state/time/end", node.int_attr(attr::ENDED).value_or(start));
            }
            if (status == "completed") {
                if (!node.str_attr(attr::TITLE)) put(part, "/state/title", "");
                put(part, "/state/metadata", json::Object{});
            }
        }
        return part;
    }
};

} // namespace

Result<Value> export_value(const Dom& dom) {
    const Node* root = dom.root();
    if (!root) return std::unexpected(AdapterError::Format{"the session DOM is empty"});
    const auto created = root->int_attr(attr::CREATED).value_or(0);
    const std::string session_id = root->str_attr(attr::NATIVE_ID).transform([](std::string_view s) { return std::string(s); })
                                       .value_or(ids::ascending("ses", created));
    auto info = oc(*root);
    if (!info) return std::unexpected(std::move(info.error()));
    put(*info, "/id", session_id);
    if (auto title = root->str_attr(attr::TITLE)) put(*info, "/title", *title);
    if (auto cwd = root->str_attr(attr::CWD)) put(*info, "/directory", *cwd);
    put(*info, "/time/created", created);
    if (!root->attr("x-oc")) {
        put(*info, "/slug", session_id);
        put(*info, "/projectID", "global");
        put(*info, "/version", "loa");
        put(*info, "/time/updated", created);
        if (!root->str_attr(attr::TITLE)) put(*info, "/title", "");
        if (!root->str_attr(attr::CWD)) put(*info, "/directory", "");
    }
    Context context{session_id, str(*root, attr::MODEL), str(*root, attr::CWD)};
    json::Array messages;
    for (const Node* turn : dom.children_of_kind(root->id, ElementKind::Turn)) {
        if (auto result = context.turn(dom, *turn, messages); !result) return std::unexpected(std::move(result.error()));
    }
    return Value(json::Object{{"info", std::move(*info)}, {"messages", std::move(messages)}});
}

Result<> OpenCodeNative::import(const std::filesystem::path& path, loa::core::PatchSink& sink) const {
    errno = 0;
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(io_error(errno ? errno : EIO));
    std::string text((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    if (in.bad()) return std::unexpected(io_error(errno ? errno : EIO));
    auto file = json::parse(text);
    if (!file) return std::unexpected(AdapterError::Format{file.error().message()});
    const auto* info = file->find("info");
    const auto* messages = file->find("messages");
    if (!info || !messages || !messages->is_array())
        return std::unexpected(AdapterError::Format{"invalid OpenCode export"});
    const auto* id = json_util::str_at(*info, "/id");
    if (!id) return std::unexpected(AdapterError::Format{"session has no id"});
    const auto created = json_util::int_at(*info, "/time/created");
    if (!created) return std::unexpected(AdapterError::Format{"session has no time.created"});
    loa::core::DomWriter writer(sink);
    RootInfo root;
    root.session_id = *id;
    root.native = *info;
    auto mapper = Mapper::start(writer.at(*created), std::move(root));
    if (!mapper) return std::unexpected(std::move(mapper.error()));
    auto last = *created;
    for (const auto& message : *messages->as_array()) {
        const auto* message_info = message.find("info");
        if (!message_info) return std::unexpected(AdapterError::Format{"message has no info"});
        const auto* parts = message.find("parts");
        if (parts && !parts->is_array()) return std::unexpected(AdapterError::Format{"message parts is not an array"});
        const auto at = json_util::int_at(*message_info, "/time/created").value_or(last);
        if (auto result = mapper->message_updated(writer.at(at), *message_info); !result)
            return std::unexpected(std::move(result.error()));
        if (parts) for (const auto& part : *parts->as_array()) {
            const auto part_at = json_util::int_at(part, "/time/start")
                                     .value_or(json_util::int_at(part, "/state/time/start").value_or(at));
            if (auto result = mapper->part_updated(writer.at(part_at), part); !result)
                return std::unexpected(std::move(result.error()));
        }
        last = std::max(last, json_util::int_at(*message_info, "/time/completed").value_or(at));
    }
    return mapper->finish(writer.at(last));
}

Result<> OpenCodeNative::export_(const Dom& dom, const std::filesystem::path& out) const {
    auto doc = export_value(dom);
    if (!doc) return std::unexpected(std::move(doc.error()));
    auto text = json::dump_pretty(*doc);
    errno = 0;
    std::ofstream file(out, std::ios::binary | std::ios::trunc);
    if (!file) return std::unexpected(io_error(errno ? errno : EIO));
    file.write(text.data(), static_cast<std::streamsize>(text.size()));
    file.close();
    if (!file) return std::unexpected(io_error(errno ? errno : EIO));
    return {};
}

std::vector<kit::NativeSession> OpenCodeNative::discover(const std::filesystem::path&) const { return {}; }

} // namespace loa::harness::opencode
