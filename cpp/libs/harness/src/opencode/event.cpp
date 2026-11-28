#include "loa/harness/opencode/event.hpp"

#include <array>
#include <utility>

namespace loa::harness::opencode {
namespace {
const json::Value* member(const json::Value& v, std::string_view key) { return v.find(key); }
std::string text(const json::Value& v, std::string_view key) {
    const auto* f = member(v, key);
    return f && f->as_string() ? *f->as_string() : std::string{};
}
bool has_text(const json::Value& v, std::string_view key) {
    const auto* f = member(v, key);
    return f && f->as_string();
}
json::Value field(const json::Value& v, std::string_view key) {
    const auto* f = member(v, key);
    return f ? *f : json::Value{};
}
std::vector<std::string> strings(const json::Value& v, std::string_view key) {
    std::vector<std::string> result;
    const auto* f = member(v, key);
    if (f && f->as_array()) for (const auto& item : *f->as_array()) if (item.as_string()) result.push_back(*item.as_string());
    return result;
}
Ref ref(const json::Value& v) {
    return {text(v, "sessionID"), text(v, "messageID"), text(v, "partID"), text(v, "requestID")};
}
std::optional<ToolRef> tool(const json::Value& v) {
    const auto* t = member(v, "tool");
    if (!t || !t->is_object() || !has_text(*t, "callID")) return std::nullopt;
    return ToolRef{text(*t, "callID")};
}
std::optional<PermissionRequest> permission(const json::Value& v) {
    if (!has_text(v, "id") || !has_text(v, "sessionID") || !has_text(v, "permission")) return std::nullopt;
    return PermissionRequest{text(v, "id"), text(v, "sessionID"), text(v, "permission"),
                             strings(v, "patterns"), strings(v, "always"), field(v, "metadata"), tool(v)};
}
std::optional<QuestionRequest> question(const json::Value& v) {
    const auto* questions = member(v, "questions");
    if (!has_text(v, "id") || !has_text(v, "sessionID") || !questions || !questions->as_array()) return std::nullopt;
    QuestionRequest result{text(v, "id"), text(v, "sessionID"), {}, tool(v)};
    for (const auto& q : *questions->as_array()) {
        if (!has_text(q, "question")) return std::nullopt;
        QuestionInfo info{text(q, "question"), text(q, "header")};
        if (const auto* choices = member(q, "options"); choices && choices->as_array()) {
            for (const auto& choice : *choices->as_array()) {
                if (!has_text(choice, "label")) return std::nullopt;
                info.options.push_back({text(choice, "label")});
            }
        }
        if (const auto* multiple = member(q, "multiple"); multiple && multiple->as_bool()) info.multiple = *multiple->as_bool();
        if (const auto* custom = member(q, "custom"); custom && custom->as_bool()) info.custom = *custom->as_bool();
        result.questions.push_back(std::move(info));
    }
    return result;
}
std::optional<Known> known(std::string_view type, const json::Value& p) {
    using K = Known::Kind;
    constexpr std::array<std::pair<std::string_view, K>, 16> types{{
        {"session.created", K::SessionCreated}, {"session.updated", K::SessionUpdated},
        {"session.status", K::SessionStatus}, {"session.idle", K::SessionIdle},
        {"session.error", K::SessionError}, {"message.updated", K::MessageUpdated},
        {"message.removed", K::MessageRemoved}, {"message.part.updated", K::PartUpdated},
        {"message.part.delta", K::PartDelta}, {"message.part.removed", K::PartRemoved},
        {"todo.updated", K::TodoUpdated}, {"permission.asked", K::PermissionAsked},
        {"permission.replied", K::PermissionReplied}, {"question.asked", K::QuestionAsked},
        {"question.replied", K::QuestionReplied}, {"question.rejected", K::QuestionRejected}
    }};
    for (auto [name, kind] : types) {
        if (type != name) continue;
        if (!p.is_object()) return std::nullopt;
        Known result{kind};
        result.at = ref(p);
        switch (kind) {
        case K::SessionCreated: case K::SessionUpdated:
            if (!member(p, "info")) return std::nullopt;
            result.info = field(p, "info"); break;
        case K::SessionStatus:
            if (!member(p, "status")) return std::nullopt;
            result.status = field(p, "status"); break;
        case K::SessionError: result.error = field(p, "error"); break;
        case K::MessageUpdated:
            if (!member(p, "info")) return std::nullopt;
            result.info = field(p, "info"); break;
        case K::PartUpdated:
            if (!member(p, "part")) return std::nullopt;
            result.part = field(p, "part"); break;
        case K::PartDelta:
            if (!has_text(p, "field") || !has_text(p, "delta")) return std::nullopt;
            result.field = text(p, "field"); result.delta = text(p, "delta"); break;
        case K::TodoUpdated: {
            const auto* todos = member(p, "todos");
            auto parsed = todos ? parse_todos(*todos) : std::nullopt;
            if (!parsed) return std::nullopt;
            result.todos = std::move(*parsed);
            break;
        }
        case K::PermissionAsked:
            result.permission = permission(p);
            if (!result.permission) return std::nullopt;
            break;
        case K::PermissionReplied:
            if (!has_text(p, "reply")) return std::nullopt;
            result.reply = text(p, "reply"); break;
        case K::QuestionAsked:
            result.question = question(p);
            if (!result.question) return std::nullopt;
            break;
        case K::QuestionReplied: {
            const auto* answers = member(p, "answers");
            if (!answers || !answers->as_array()) return std::nullopt;
            for (const auto& answer : *answers->as_array()) {
                if (!answer.as_array()) return std::nullopt;
                std::vector<std::string> row;
                for (const auto& item : *answer.as_array()) {
                    if (!item.as_string()) return std::nullopt;
                    row.push_back(*item.as_string());
                }
                result.answers.push_back(std::move(row));
            }
            break;
        }
        default: break;
        }
        return result;
    }
    return std::nullopt;
}
} // namespace

std::optional<std::vector<Todo>> parse_todos(const json::Value& value) {
    if (!value.as_array()) return std::nullopt;
    std::vector<Todo> out;
    for (const auto& todo : *value.as_array()) {
        if (!has_text(todo, "content") || !has_text(todo, "status")) return std::nullopt;
        out.push_back({text(todo, "content"), text(todo, "status"), text(todo, "priority")});
    }
    return out;
}

std::optional<Event> parse_event(const json::Value& value) {
    if (!has_text(value, "type")) return std::nullopt;
    Event result{text(value, "type"), field(value, "properties"), std::nullopt};
    result.known = known(result.type, result.properties);
    return result;
}

std::optional<std::string_view> Event::session_id() const noexcept {
    const std::string* id = nullptr;
    if (!known) {
        if (const auto* v = properties.find("sessionID")) id = v->as_string();
    } else {
        switch (known->kind) {
        case Known::Kind::SessionCreated: case Known::Kind::SessionUpdated:
            if (const auto* v = known->info.find("id")) id = v->as_string(); break;
        case Known::Kind::PermissionAsked: id = &known->permission->session_id; break;
        case Known::Kind::QuestionAsked: id = &known->question->session_id; break;
        default: id = &known->at.session; break;
        }
    }
    if (id && !id->empty()) return *id;
    return std::nullopt;
}

void SseDecoder::feed(std::string_view bytes, std::vector<std::string>& out) {
    pending_.append(bytes);
    std::size_t start = 0;
    for (auto end = pending_.find('\n', start); end != std::string::npos; end = pending_.find('\n', start)) {
        std::string_view line(pending_.data() + start, end - start);
        start = end + 1;
        if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
        if (line.empty()) {
            if (data_) { out.push_back(std::move(*data_)); data_.reset(); }
        } else if (line == "data" || line.starts_with("data:")) {
            auto value = line == "data" ? std::string_view{} : line.substr(5);
            if (value.starts_with(' ')) value.remove_prefix(1);
            if (data_) { data_->push_back('\n'); data_->append(value); }
            else data_ = std::string(value);
        }
    }
    pending_.erase(0, start);
}

} // namespace loa::harness::opencode
