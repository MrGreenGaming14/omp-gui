#include "loa/projects/tools.hpp"

#include <algorithm>
#include <asio/awaitable.hpp>
#include <initializer_list>

#include "loa/contracts/schema.hpp"
#include "loa/core/schema.hpp"
#include "loa/projects/read.hpp"

namespace loa::projects {

namespace json = runtime::json;
namespace s = contracts::schema;
using contracts::CallCtx;
using contracts::Json;
using contracts::ToolOutput;
using contracts::ToolResult;

namespace {

std::string type_name(const Json& v) {
    if (v.is_null()) return "null";
    if (const auto* b = v.as_bool()) return std::string("boolean `") + (*b ? "true" : "false") + "`";
    if (const auto* str = v.as_string()) return "string \"" + *str + "\"";
    if (auto i = v.as_i64()) return "integer `" + std::to_string(*i) + "`";
    if (v.is_number()) return "floating point";
    if (v.is_array()) return "sequence";
    return "map";
}

// serde's `deny_unknown_fields` check for a struct with `fields`.
std::expected<void, std::string> known_fields(const Json& json, std::string_view what, std::initializer_list<std::string_view> fields) {
    if (auto ok = contracts::args::object(json, what); !ok) return ok;
    for (const auto& [key, _] : *json.as_object()) {
        if (std::ranges::find(fields, key) != fields.end()) continue;
        std::string expected;
        for (auto f : fields) expected += (expected.empty() ? "`" : ", `") + std::string(f) + "`";
        return std::unexpected("unknown field `" + key + "`, expected one of " + expected);
    }
    return {};
}

std::expected<std::optional<std::vector<std::string>>, std::string> optional_strings(const Json& json, std::string_view key) {
    const auto* v = json.find(key);
    if (!v || v->is_null()) return std::nullopt;
    const auto* items = v->as_array();
    if (!items) return std::unexpected("invalid type: " + type_name(*v) + ", expected a sequence");
    std::vector<std::string> out;
    for (const auto& item : *items) {
        const auto* str = item.as_string();
        if (!str) return std::unexpected("invalid type: " + type_name(item) + ", expected a string");
        out.push_back(*str);
    }
    return out;
}

struct Scope {
    std::shared_ptr<const SessionSource> source;
    std::shared_ptr<const Access> access;

    Result<std::string> project_of(std::string_view session) const {
        auto dom = source->load(session);
        if (!dom) return std::unexpected(dom.error());
        if (!*dom) return std::unexpected(Error::session_not_found(std::string(session)));
        auto project = session_project(**dom);
        if (!project) return std::unexpected(Error::arguments("this session is not in a project"));
        return std::move(*project);
    }
};

Json opt(const std::optional<std::string>& v) { return v ? Json(*v) : Json(nullptr); }

class ProjectSearch final : public contracts::Tool<SearchArgs> {
  public:
    explicit ProjectSearch(Scope scope) : scope_(std::move(scope)) {}
    [[nodiscard]] std::string_view name() const override { return PROJECT_SEARCH; }
    [[nodiscard]] std::string_view description() const override {
        return "Search the other sessions of this project by text or element type (decisions, calls, messages, todos, ...). "
               "Returns hits with session id, node id, a snippet and where the node sits. Read a hit with project_read.";
    }

    [[nodiscard]] contracts::Awaitable<ToolResult> run(const CallCtx& ctx, SearchArgs args) const override {
        auto page = execute(ctx.session_id, std::move(args));
        if (!page) co_return std::unexpected(page.error().to_string());
        json::Array hits;
        for (const auto& h : page->hits) {
            hits.emplace_back(json::Object{{"session", h.session},
                                           {"session_title", opt(h.session_title)},
                                           {"node", h.node.value},
                                           {"element", core::as_str(h.kind)},
                                           {"snippet", h.snippet},
                                           {"path", h.path}});
        }
        const json::Object out{{"total", static_cast<std::uint64_t>(page->total)}, {"hits", std::move(hits)}, {"next_cursor", opt(page->next_cursor)}};
        co_return ToolOutput::text(json::dump(out));
    }

  private:
    Result<SearchPage> execute(const std::string& caller, SearchArgs args) const {
        auto project = scope_.project_of(caller);
        if (!project) return std::unexpected(project.error());
        std::optional<core::ElementKind> kind;
        if (args.element) {
            kind = core::parse<core::ElementKind>(*args.element);
            if (!kind) return std::unexpected(Error::arguments("unknown element `" + *args.element + "`"));
        }
        // The caller's own session is left out unless `sessions` names it.
        const bool listed = args.sessions && std::ranges::find(*args.sessions, caller) != args.sessions->end();
        SearchQuery query;
        query.text = std::move(args.query);
        query.mode = args.mode;
        query.kind = kind;
        for (auto& [name, value] : args.attributes) query.attrs.push_back(AttrFilter{name, value});
        if (!listed) query.skip_session = caller;
        query.sessions = std::move(args.sessions);
        query.include_excluded = args.include_excluded;
        query.limit = args.limit.value_or(0);
        query.cursor = std::move(args.cursor);
        return search(*scope_.source, *scope_.access, *project, query);
    }

    Scope scope_;
};

class ProjectRead final : public contracts::Tool<ReadArgs> {
  public:
    explicit ProjectRead(Scope scope) : scope_(std::move(scope)) {}
    [[nodiscard]] std::string_view name() const override { return PROJECT_READ; }
    [[nodiscard]] std::string_view description() const override {
        return "Read the full record behind a project_search hit or a digest pointer: the element and everything inside it, in pages.";
    }

    [[nodiscard]] contracts::Awaitable<ToolResult> run(const CallCtx& ctx, ReadArgs args) const override {
        ReadRequest request{std::move(args.session), args.node ? std::optional(core::NodeId{*args.node}) : std::nullopt,
                            args.offset, args.max_bytes.value_or(0)};
        auto project = scope_.project_of(ctx.session_id);
        if (!project) co_return std::unexpected(project.error().to_string());
        auto page = read_subtree(*scope_.source, *scope_.access, *project, request);
        if (!page) co_return std::unexpected(page.error().to_string());
        const auto end = page->offset + page->text.size();
        std::string out = "session " + page->session + " node " + core::to_string(page->node) + " (bytes " +
                          std::to_string(page->offset) + ".." + std::to_string(end) + " of " + std::to_string(page->total_bytes) +
                          ")\n" + page->text;
        if (page->next_offset) out += "\n[more: call project_read with offset=" + std::to_string(*page->next_offset) + "]\n";
        co_return ToolOutput::text(std::move(out));
    }

  private:
    Scope scope_;
};

} // namespace

std::string SearchArgs::schema() {
    auto attributes = json::Object{{"type", "object"}, {"additionalProperties", s::nullable(s::string())}};
    auto object =
        s::Object()
            .description("Search arguments.")
            .property("query", s::describe(s::nullable(s::string()), "Text to find, case insensitive."), false)
            .property("mode", s::describe(s::ref("TextMode"), "phrase: the whole query as written. all_words: every word, anywhere."), false)
            .property("element",
                      s::describe(s::nullable(s::string()),
                                  "Only elements of this type (message, call, decision, todo, ...). The query then matches anywhere\n"
                                  "inside the element, e.g. element=call with a file path finds every call that touched it."),
                      false)
            .property("attributes",
                      s::describe(s::with_default(std::move(attributes), json::Object{}),
                                  "Attributes the element must have. A string matches part of the value; null only requires the\n"
                                  "attribute, e.g. {\"answer\": null} on decisions."),
                      false)
            .property("sessions",
                      s::describe(s::nullable(s::array(s::string())), "Only these session ids. List this chat's own id to search it too."),
                      false)
            .property("include_excluded",
                      s::describe(s::with_default(s::boolean(), false), "Also search sessions excluded from project context."), false)
            .property("limit", s::range(s::nullable(s::unsigned_integer("uint")), 1, 100), false)
            .property("cursor", s::describe(s::nullable(s::string()), "next_cursor of the previous page."), false)
            .deny_unknown_fields();
    auto mode = s::one_of({s::describe(s::constant("phrase"), "The whole query as one substring."),
                           s::describe(s::constant("all_words"), "Every whitespace separated word, anywhere.")},
                          "How a text query matches.");
    return s::Root("SearchArgs", std::move(object)).define("TextMode", std::move(mode)).dump();
}

std::expected<SearchArgs, std::string> SearchArgs::from_json(const Json& json) {
    if (auto ok = known_fields(json, "struct SearchArgs",
                               {"query", "mode", "element", "attributes", "sessions", "include_excluded", "limit", "cursor"});
        !ok)
        return std::unexpected(ok.error());
    SearchArgs a;
    auto query = contracts::args::optional_string(json, "query");
    if (!query) return std::unexpected(query.error());
    a.query = std::move(*query);
    if (const auto* mode = json.find("mode")) {
        const auto* str = mode->as_string();
        if (!str) return std::unexpected("invalid type: " + type_name(*mode) + ", expected enum TextMode");
        if (*str == "phrase") a.mode = TextMode::Phrase;
        else if (*str == "all_words") a.mode = TextMode::AllWords;
        else return std::unexpected("unknown variant `" + *str + "`, expected `phrase` or `all_words`");
    }
    auto element = contracts::args::optional_string(json, "element");
    if (!element) return std::unexpected(element.error());
    a.element = std::move(*element);
    if (const auto* attrs = json.find("attributes")) {
        const auto* obj = attrs->as_object();
        if (!obj) return std::unexpected("invalid type: " + type_name(*attrs) + ", expected a map");
        for (const auto& [key, value] : *obj) {
            if (value.is_null()) {
                a.attributes.insert_or_assign(key, std::nullopt);
            } else if (const auto* str = value.as_string()) {
                a.attributes.insert_or_assign(key, *str);
            } else {
                return std::unexpected("invalid type: " + type_name(value) + ", expected a string");
            }
        }
    }
    auto sessions = optional_strings(json, "sessions");
    if (!sessions) return std::unexpected(sessions.error());
    a.sessions = std::move(*sessions);
    auto include = contracts::args::optional_bool(json, "include_excluded");
    if (!include) return std::unexpected(include.error());
    a.include_excluded = include->value_or(false);
    auto limit = contracts::args::optional_u64(json, "limit");
    if (!limit) return std::unexpected(limit.error());
    if (*limit) a.limit = static_cast<std::size_t>(**limit);
    auto cursor = contracts::args::optional_string(json, "cursor");
    if (!cursor) return std::unexpected(cursor.error());
    a.cursor = std::move(*cursor);
    return a;
}

std::string ReadArgs::schema() {
    auto object =
        s::Object()
            .description("Read arguments.")
            .property("session", s::describe(s::string(), "Session id from a search hit or digest."))
            .property("node",
                      s::describe(s::range(s::nullable(s::unsigned_integer("uint64")), 1),
                                  "Node id from a search hit or digest pointer. Omit for the whole session."),
                      false)
            .property("offset", s::describe(s::with_default(s::unsigned_integer("uint"), 0), "next_offset of the previous page."), false)
            .property("max_bytes", s::range(s::nullable(s::unsigned_integer("uint")), 1, 32000), false)
            .deny_unknown_fields();
    return s::Root("ReadArgs", std::move(object)).dump();
}

std::expected<ReadArgs, std::string> ReadArgs::from_json(const Json& json) {
    if (auto ok = known_fields(json, "struct ReadArgs", {"session", "node", "offset", "max_bytes"}); !ok)
        return std::unexpected(ok.error());
    ReadArgs a;
    auto session = contracts::args::string(json, "session");
    if (!session) return std::unexpected(session.error());
    a.session = std::move(*session);
    auto node = contracts::args::optional_u64(json, "node");
    if (!node) return std::unexpected(node.error());
    a.node = *node;
    if (const auto* offset = json.find("offset")) {
        auto n = offset->as_u64();
        if (!n) return std::unexpected("invalid type: " + type_name(*offset) + ", expected usize");
        a.offset = static_cast<std::size_t>(*n);
    }
    auto max = contracts::args::optional_u64(json, "max_bytes");
    if (!max) return std::unexpected(max.error());
    if (*max) a.max_bytes = static_cast<std::size_t>(**max);
    return a;
}

std::shared_ptr<contracts::ToolSet> tool_set(std::shared_ptr<const SessionSource> source, std::shared_ptr<const Access> access) {
    const Scope scope{std::move(source), std::move(access)};
    auto set = std::make_shared<contracts::ToolSet>();
    set->add(std::make_shared<ProjectSearch>(scope));
    set->add(std::make_shared<ProjectRead>(scope));
    return set;
}

} // namespace loa::projects
