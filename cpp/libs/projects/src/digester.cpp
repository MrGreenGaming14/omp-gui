#include "loa/projects/digester.hpp"

#include <algorithm>
#include <asio/awaitable.hpp>
#include <charconv>

#include "loa/core/schema.hpp"
#include "loa/projects/text.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/text.hpp"

namespace loa::projects {

namespace attr = core::attr;
namespace json = runtime::json;
using K = core::ElementKind;
using core::Node;
using runtime::text::clip;
using runtime::text::clip_marked;
using settings::DigestContent;
using settings::FileNote;

const std::string_view INSTRUCTIONS =
    R"(You write the digest of one coding session, so other sessions in the same project know what it did.
Answer with exactly one JSON object and nothing else, in this shape:
{"summary": "two or three sentences on what the session did",
 "decisions": ["each decision that was made, one short line"],
 "files": [{"path": "a file the session read or changed", "note": "one line on what happened to it"}],
 "open_items": ["unfinished todos and unanswered questions"],
 "pointers": ["labels like T3 of the turns the digest is based on"]}
Every key is required; use [] when there is nothing. Pointers must be labels that appear below.
)";

namespace {

void render_open_items(const core::Dom& dom, const Node& root, const PromptLimits& limits, std::string& out) {
    std::vector<const Node*> items;
    for (const auto& n : dom.descendants(root.id)) {
        bool open = false;
        if (n.kind == K::Todo) {
            auto state = n.str_attr(attr::STATE);
            auto parsed = state ? core::parse<core::TodoState>(*state) : std::nullopt;
            open = !parsed || *parsed == core::TodoState::Pending || *parsed == core::TodoState::InProgress;
        } else if (n.kind == K::Decision) {
            auto status = n.str_attr(attr::STATUS);
            open = status && core::parse<core::DecisionStatus>(*status) == core::DecisionStatus::Waiting;
        }
        if (open) items.push_back(&n);
    }
    std::ranges::stable_sort(items, [](const Node* a, const Node* b) { return (a->kind != K::Todo) < (b->kind != K::Todo); });
    if (!items.empty()) out += "\n## Open\n";
    for (const auto* n : items) {
        std::string_view what, txt;
        if (n->kind == K::Todo) {
            what = "todo";
            txt = n->text;
        } else {
            what = "waiting for an answer";
            txt = n->str_attr(attr::TITLE).value_or(std::string_view(n->text));
        }
        out += "- " + std::string(what) + ": " + std::string(clip(txt, limits.brief_chars)) + "\n";
    }
}

void render_decision(const Node& decision, std::size_t max, std::string& out) {
    const auto kind = decision.str_attr(attr::KIND).value_or("decision");
    std::vector<std::string_view> answer;
    if (const auto* a = decision.attr(attr::ANSWER)) answer = a->str_items();
    const auto what = decision.str_attr(attr::TITLE).value_or(std::string_view(decision.text));
    std::string joined;
    if (answer.empty()) {
        joined = "unanswered";
    } else {
        for (std::size_t i = 0; i < answer.size(); ++i) {
            if (i) joined += ", ";
            joined += answer[i];
        }
    }
    out += "Decision (" + std::string(kind) + "): " + std::string(clip(text::trim(what), max)) + " -> " + joined + "\n";
}

void render_call(const core::Dom& dom, const Node& call, const PromptLimits& limits, std::string& out) {
    out += "Tool " + std::string(call.str_attr(attr::TOOL).value_or("tool"));
    if (auto title = call.str_attr(attr::TITLE)) out += " (" + std::string(clip(*title, 120)) + ")";
    if (auto status = call.str_attr(attr::STATUS); status && core::parse<core::CallStatus>(*status) == core::CallStatus::Error)
        out += " [error]";
    out.push_back('\n');
    for (const auto* child : dom.children(call.id)) {
        std::string_view part;
        if (child->kind == K::Input) part = "input";
        else if (child->kind == K::Result) part = "result";
        else if (child->kind == K::Decision) {
            render_decision(*child, limits.tool_chars, out);
            continue;
        } else continue;
        out += "  " + std::string(part) + ": " + clip_marked(text::trim(child->text), limits.tool_chars) + "\n";
    }
}

std::string render_turn(const core::Dom& dom, const Node& turn, std::size_t label, bool full, const PromptLimits& limits) {
    std::string out = "\n### T" + std::to_string(label) + (full ? "" : " (abbreviated)") + "\n";
    const auto message_chars = full ? limits.message_chars : limits.brief_chars;
    std::vector<std::string> tools;
    for (const auto* node : dom.children(turn.id)) {
        switch (node->kind) {
            case K::Message: {
                auto kind = node->str_attr(attr::KIND);
                if (kind && core::parse<core::MessageKind>(*kind) == core::MessageKind::Thinking) break;
                auto role = node->str_attr(attr::ROLE);
                auto parsed = role ? core::parse<core::Role>(*role) : std::nullopt;
                std::string_view who = "System";
                if (parsed == core::Role::User) who = "User";
                else if (parsed == core::Role::Assistant) who = "Assistant";
                out += std::string(who) + ": " + clip_marked(text::trim(node->text), message_chars) + "\n";
                break;
            }
            case K::Call:
                if (full) {
                    render_call(dom, *node, limits, out);
                } else {
                    const auto tool = node->str_attr(attr::TOOL).value_or("tool");
                    auto title = node->str_attr(attr::TITLE);
                    tools.push_back(title ? std::string(tool) + " (" + std::string(clip(*title, 80)) + ")" : std::string(tool));
                }
                break;
            case K::Decision: render_decision(*node, message_chars, out); break;
            default: break;
        }
    }
    if (!tools.empty()) {
        out += "Tools: ";
        for (std::size_t i = 0; i < tools.size(); ++i) out += (i ? ", " : "") + tools[i];
        out += "\n";
    }
    return out;
}

std::string json_type(const json::Value& v) {
    if (v.is_null()) return "null";
    if (v.is_bool()) return "boolean";
    if (v.is_number()) return "number";
    if (v.is_string()) return "string";
    if (v.is_array()) return "sequence";
    return "map";
}

std::expected<std::string, std::string> need_string(const json::Value& obj, std::string_view key) {
    const auto* v = obj.find(key);
    if (!v) return std::unexpected("missing field `" + std::string(key) + "`");
    if (const auto* s = v->as_string()) return *s;
    return std::unexpected("invalid type: " + json_type(*v) + ", expected a string");
}

std::expected<std::vector<std::string>, std::string> need_strings(const json::Value& obj, std::string_view key) {
    const auto* v = obj.find(key);
    if (!v) return std::unexpected("missing field `" + std::string(key) + "`");
    const auto* items = v->as_array();
    if (!items) return std::unexpected("invalid type: " + json_type(*v) + ", expected a sequence");
    std::vector<std::string> out;
    for (const auto& item : *items) {
        const auto* s = item.as_string();
        if (!s) return std::unexpected("invalid type: " + json_type(item) + ", expected a string");
        out.push_back(*s);
    }
    return out;
}

std::vector<std::string> lines_of(std::vector<std::string> items) {
    std::vector<std::string> out;
    for (auto& s : items) {
        auto t = text::trim(s);
        if (!t.empty()) out.emplace_back(t);
    }
    return out;
}

} // namespace

Result<DigestPrompt> build_prompt(const core::Dom& dom, const PromptLimits& limits) {
    const auto* root = dom.root();
    if (!root) return std::unexpected(Error::nothing_to_digest());
    const auto turns = dom.children_of_kind(root->id, K::Turn);
    if (turns.empty()) return std::unexpected(Error::nothing_to_digest());
    std::string out = "\n# Session\n";
    if (auto title = root->str_attr(attr::TITLE)) out += "Title: " + std::string(clip(*title, limits.brief_chars)) + "\n";
    render_open_items(dom, *root, limits, out);
    if (auto compactions = dom.children_of_kind(root->id, K::Compaction); !compactions.empty())
        out += "\n## Earlier summary\n" + clip_marked(compactions.back()->text, limits.message_chars) + "\n";
    // Newest first until the budget runs out, then emitted oldest first.
    std::vector<std::string> sections;
    std::size_t used = 0;
    for (std::size_t age = 0; age < turns.size(); ++age) {
        const std::size_t i = turns.size() - 1 - age;
        auto section = render_turn(dom, *turns[i], i + 1, age < limits.full_turns, limits);
        const auto room = limits.max_chars > used ? limits.max_chars - used : 0;
        if (section.size() > room) {
            if (sections.empty()) sections.push_back(clip_marked(section, room));
            break;
        }
        used += section.size();
        sections.push_back(std::move(section));
    }
    out += "\n## Turns\n";
    const auto omitted = turns.size() - sections.size();
    if (omitted > 0) out += "[" + std::to_string(omitted) + " earlier turns omitted]\n";
    for (auto it = sections.rbegin(); it != sections.rend(); ++it) out += *it;
    DigestPrompt prompt{std::move(out), {}, dom.seq()};
    for (const auto* t : turns) prompt.labels.push_back(t->id);
    return prompt;
}

Result<DigestContent> parse_answer(std::string_view answer_text, const DigestPrompt& prompt) {
    const auto bad = [](std::string why) { return std::unexpected(Error::invalid_answer(std::move(why))); };
    const auto start = answer_text.find('{');
    const auto end = answer_text.rfind('}');
    if (start == std::string_view::npos || end == std::string_view::npos || start >= end)
        return bad("no JSON object in the answer");
    auto parsed = json::parse(answer_text.substr(start, end - start + 1));
    if (!parsed) return bad(parsed.error().message());
    const auto& v = *parsed;
    if (!v.is_object()) return bad("invalid type: " + json_type(v) + ", expected struct Answer");
    auto summary = need_string(v, "summary");
    if (!summary) return bad(summary.error());
    auto decisions = need_strings(v, "decisions");
    if (!decisions) return bad(decisions.error());
    const auto* files = v.find("files");
    if (!files) return bad("missing field `files`");
    if (!files->is_array()) return bad("invalid type: " + json_type(*files) + ", expected a sequence");
    std::vector<FileNote> notes;
    for (const auto& f : *files->as_array()) {
        if (!f.is_object()) return bad("invalid type: " + json_type(f) + ", expected struct FileNote");
        auto path = need_string(f, "path");
        if (!path) return bad(path.error());
        std::string note;
        if (const auto* n = f.find("note")) {
            const auto* s = n->as_string();
            if (!s) return bad("invalid type: " + json_type(*n) + ", expected a string");
            note = *s;
        }
        notes.push_back(FileNote{std::move(*path), std::move(note)});
    }
    auto open_items = need_strings(v, "open_items");
    if (!open_items) return bad(open_items.error());
    auto pointers = need_strings(v, "pointers");
    if (!pointers) return bad(pointers.error());

    const auto trimmed = text::trim(*summary);
    if (trimmed.empty()) return bad("empty summary");
    DigestContent content;
    content.summary = std::string(trimmed);
    content.decisions = lines_of(std::move(*decisions));
    for (auto& f : notes) {
        const auto path = text::trim(f.path);
        if (path.empty()) return bad("a file entry has an empty path");
        content.files.push_back(FileNote{std::string(path), std::string(text::trim(f.note))});
    }
    content.open_items = lines_of(std::move(*open_items));
    for (const auto& raw : *pointers) {
        const auto label = text::trim(raw);
        std::optional<core::NodeId> id;
        if (!label.empty() && (label[0] == 'T' || label[0] == 't'))
            if (auto n = text::parse_usize(label.substr(1)); n && *n >= 1 && *n <= prompt.labels.size()) id = prompt.labels[*n - 1];
        if (!id) return bad("pointer `" + std::string(label) + "` names no turn");
        if (std::ranges::find(content.pointers, *id) == content.pointers.end()) content.pointers.push_back(*id);
    }
    return content;
}

contracts::Awaitable<Result<BuiltDigest>> Digester::digest(const core::Dom& dom, std::optional<std::string> model) const {
    auto prompt = build_prompt(dom, limits_);
    if (!prompt) co_return std::unexpected(prompt.error());
    auto [summarizer, recorded] = summarizers_->on(model ? std::optional<std::string_view>(*model) : std::nullopt);
    auto answer = co_await summarizer->summarize(std::string(INSTRUCTIONS), prompt->text);
    if (!answer) co_return std::unexpected(Error::summarizer(answer.error()));
    auto content = parse_answer(*answer, *prompt);
    if (!content) co_return std::unexpected(content.error());
    co_return BuiltDigest{std::move(*content), prompt->source_seq, std::move(recorded)};
}

} // namespace loa::projects
