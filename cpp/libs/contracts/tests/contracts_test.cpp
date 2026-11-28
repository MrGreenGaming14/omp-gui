#include <doctest/doctest.h>

#include <asio.hpp>

#include <cctype>
#include <fstream>
#include <optional>
#include <sstream>

#include "loa/contracts/harness.hpp"
#include "loa/contracts/host_tools.hpp"
#include "loa/contracts/schema.hpp"
#include "loa/contracts/session.hpp"
#include "loa/contracts/tools.hpp"
#include "loa/core/dom.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::contracts;
namespace core = loa::core;
namespace attr = loa::core::attr;
namespace s = loa::contracts::schema;

namespace {

template <typename T>
T run(Awaitable<T> task) {
    asio::io_context io;
    std::optional<T> out;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> { out = co_await std::move(task); }, asio::detached);
    io.run();
    return std::move(*out);
}

std::string golden(const std::string& name) {
    std::ifstream in(std::string(LOA_GOLDEN_DIR) + "/contracts/" + name, std::ios::binary);
    REQUIRE(in);
    std::ostringstream text;
    text << in.rdbuf();
    return text.str();
}

// ---- tools -----------------------------------------------------------------

struct ShoutArgs {
    std::string word;

    static std::string schema() {
        return s::Root("Args", s::Object()
                                   .description("The `Args` of the `shout` tool in `loa_contracts::tools` tests.")
                                   .property("word", s::string()))
            .dump();
    }
    static std::expected<ShoutArgs, std::string> from_json(const Json& json) {
        if (auto ok = args::object(json, "struct Args"); !ok) return std::unexpected(ok.error());
        auto word = args::string(json, "word");
        if (!word) return std::unexpected(word.error());
        return ShoutArgs{std::move(*word)};
    }
};

class Shout final : public Tool<ShoutArgs> {
  public:
    std::string_view name() const override { return "shout"; }
    std::string_view description() const override { return "uppercases"; }
    Awaitable<ToolResult> run(const CallCtx& ctx, ShoutArgs a) const override {
        if (a.word.empty()) co_return ToolOutput::error("empty");
        for (auto& c : a.word) c = static_cast<char>(std::toupper(static_cast<unsigned char>(c)));
        co_return ToolOutput::text(ctx.session_id + ":" + a.word);
    }
};

std::string search_args_schema() {
    auto object = s::Object()
                      .description("Search arguments.")
                      .property("query", s::describe(s::nullable(s::string()), "Text to find, case insensitive."), false)
                      .property("mode", s::describe(s::ref("TextMode"), "phrase: the whole query as written. all_words: every word, anywhere."), false)
                      .property("node", s::describe(s::range(s::nullable(s::unsigned_integer("uint64")), 1), "Node id from a search hit."), false)
                      .property("offset", s::describe(s::with_default(s::unsigned_integer("uint"), 0), "next_offset of the previous page."), false)
                      .property("max_bytes", s::range(s::nullable(s::unsigned_integer("uint")), 1, 32000), false)
                      .property("x", s::describe(s::number("float"), "Pixels from the left edge"))
                      .property("clicks", s::describe(s::with_default(s::unsigned_integer("uint32"), 1), "2 for a double click"), false)
                      .property("modifiers", s::array(s::ref("ModifierKey")), false)
                      .property("submit", s::with_default(s::boolean(), false), false)
                      .property("network", s::nullable(s::boolean()), false)
                      .property("cpus", s::nullable(s::unsigned_integer("uint32")), false)
                      .property("mode2", s::nullable(s::ref("TextMode")), false)
                      .deny_unknown_fields();
    return s::Root("SearchArgs", std::move(object))
        .define("TextMode", s::one_of({s::describe(s::constant("phrase"), "The whole query as one substring."),
                                       s::describe(s::constant("all_words"), "Every whitespace separated word, anywhere.")},
                                      "How a text query matches."))
        .define("ModifierKey", s::string_enum({"shift", "ctrl"}))
        .dump();
}

std::string data_args_schema() {
    auto info = s::Object().description("Format, MIME type, page count and page sizes.").property("kind", s::constant("info"));
    auto text = s::Object()
                    .description("Page text.")
                    .property("kind", s::constant("text"))
                    .property("page", s::with_default(s::nullable(s::unsigned_integer("uint")), nullptr), false);
    auto cells = s::Object()
                     .description("Cells of `range` in `sheet`.")
                     .property("kind", s::constant("cells"))
                     .property("sheet", s::with_default(s::nullable(s::string()), nullptr), false)
                     .property("range", s::string());
    auto object = s::Object()
                      .property("uri", s::describe(s::string(), "The surface, e.g. surface://docs/1"))
                      .property("query", s::ref("DataQuery"))
                      .deny_unknown_fields();
    return s::Root("DataArgs", std::move(object))
        .define("DataQuery", s::one_of({info.to_json(), text.to_json(), cells.to_json()},
                                       "Structured data the agent tools ask a document surface for."))
        .dump();
}

// ---- session ---------------------------------------------------------------

core::Attrs base_attrs() {
    return {{"id", core::Value("s1")}, {"harness", core::Value("omp")}, {"node", core::Value("key-laptop")}, {"created", core::Value(100)}};
}

core::NodeId insert(core::Dom& dom, core::NodeId parent, core::ElementKind kind, core::Attrs attrs, std::string text = {}) {
    auto patch = dom.commit(core::Draft::insert(parent, kind, std::move(attrs), std::move(text)), 1);
    REQUIRE(patch);
    return patch->node;
}

} // namespace

TEST_CASE("interface version and adapter error messages match the Rust crate") {
    CHECK(INTERFACE_VERSION == "0.5.0");
    CHECK(AdapterError(AdapterError::Unsupported{"omp", "fork"}).message() == "omp doesn't support fork");
    CHECK(AdapterError(AdapterError::Busy{}).message() == "a turn is already running");
    CHECK(AdapterError(AdapterError::NotInstalled{"claude"}).message() == "the harness executable `claude` wasn't found");
    CHECK(AdapterError(AdapterError::Version{"codex", "need 2"}).message() == "codex version mismatch: need 2");
    CHECK(AdapterError(AdapterError::Closed{}).message() == "the session is closed");
    CHECK(AdapterError(AdapterError::UnknownDecision{core::NodeId{7}}).message() == "unknown decision 7");
    CHECK(AdapterError(AdapterError::Protocol{"eof"}).message() == "harness protocol error: eof");
    CHECK(AdapterError(AdapterError::NotAddressable{"gone"}).message() == "gone");
    CHECK(AdapterError(AdapterError::Rejected{"no"}).message() == "no");
    CHECK(AdapterError(AdapterError::Format{"line 3"}).message() == "malformed native session: line 3");
    CHECK(AdapterError(AdapterError::Commit{core::commit_error::NotWriter{}}).message() == "only the owner node writes this session");
    CHECK(AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "denied")}).message() == "denied");
    DOCTEST_CHECK(DuplicateHostTool{"a"}.message() == "host tool `a` is already registered");
}

TEST_CASE("availability probe reports missing, ready and failing executables") {
    namespace exec = loa::runtime::exec;
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto ok = dir->path() / "ok", bad = dir->path() / "bad";
    REQUIRE(exec::write_executable(ok, "#!/bin/sh\necho \"fake 4.5.6\"\n"));
    REQUIRE(exec::write_executable(bad, "#!/bin/sh\necho 'bad flag' >&2\nexit 3\n"));
    CHECK(Availability::probe("loa-no-such-harness", {}).status == AvailabilityStatus::NotInstalled);
    auto ready = Availability::probe(ok, {"--version"});
    CHECK(ready.version == "4.5.6");
    CHECK(ready.executable == exec::resolve(ok));
    auto failed = Availability::probe(bad, {"--version"});
    CHECK(failed.status == AvailabilityStatus::Failed);
    REQUIRE(failed.detail);
    CHECK(failed.detail->find("bad flag") != std::string::npos);
}

TEST_CASE("tool argument schemas match the schemars goldens byte for byte") {
    CHECK(ShoutArgs::schema() == golden("args.schema.json"));
    CHECK(search_args_schema() == golden("search_args.schema.json"));
    CHECK(data_args_schema() == golden("data_args.schema.json"));
    CHECK(s::Root("NoArgs", s::Object().deny_unknown_fields()).dump() == golden("no_args.schema.json"));
}

TEST_CASE("tool set serves specs and host tool calls") {
    ToolSet set;
    set.add(std::make_shared<Shout>());
    auto specs = set.specs();
    REQUIRE(specs.size() == 1);
    CHECK(specs[0].name == "shout");
    CHECK(specs[0].description == "uppercases");
    CHECK(specs[0].input_schema.find("\"word\"") != std::string::npos);
    using R = std::expected<std::string, std::string>;
    const HostTools& host = set;
    CHECK(run(host.call("s", "shout", R"({"word":"hi"})")) == R("s:HI"));
    CHECK(run(host.call("s", "shout", R"({"word":""})")) == R(std::unexpect, "empty"));
    CHECK(run(host.call("s", "shout", "{}")) == R(std::unexpect, "invalid arguments for `shout`: missing field `word`"));
    CHECK(run(host.call("s", "shout", " ")) == R(std::unexpect, "invalid arguments for `shout`: missing field `word`"));
    CHECK(run(host.call("s", "nope", "{}")) == R(std::unexpect, "unknown tool `nope`"));
}

TEST_CASE("host tool router routes calls and installs into the config") {
    struct Echo final : HostTools {
        std::string tag;
        explicit Echo(std::string t) : tag(std::move(t)) {}
        Awaitable<std::expected<std::string, std::string>> call(std::string session_id, std::string tool, std::string args) const override {
            co_return tag + ":" + session_id + ":" + tool + ":" + args;
        }
    };
    auto spec = [](std::string name) { return HostToolSpec{std::move(name), "", "{}"}; };
    HostToolRouter router;
    REQUIRE(router.add_all({spec("a"), spec("b")}, std::make_shared<Echo>("one")));
    CHECK(router.add(spec("a"), std::make_shared<Echo>("two")) == std::unexpected(DuplicateHostTool{"a"}));
    CHECK(run(router.call("s1", "b", "{}")) == "one:s1:b:{}");
    CHECK(run(router.call_content("s1", "a", "1")) == std::vector<OutputBlock>{TextBlock{"one:s1:a:1"}});
    CHECK(run(router.call("s1", "nope", "{}")).error() == "unknown host tool `nope`");

    SessionConfig config;
    config.host_tools.push_back(spec("existing"));
    HostToolRouter().install(config);
    CHECK(config.host_tool_handler == nullptr);
    std::move(router).install(config);
    REQUIRE(config.host_tools.size() == 3);
    CHECK(config.host_tools[1].name == "a");
    CHECK(config.host_tools[2].name == "b");
    CHECK(config.host_tool_handler != nullptr);
}

TEST_CASE("session summary: apply matches from_dom patch by patch") {
    core::Dom dom;
    std::optional<SessionSummary> summary;
    auto step = [&](core::Draft draft, std::int64_t ts) {
        auto patch = dom.commit(std::move(draft), ts);
        REQUIRE(patch);
        if (summary) {
            const auto before = *summary;
            const bool changed = summary->apply(dom, *patch);
            CHECK(changed == (*summary != before));
        } else {
            summary = SessionSummary::from_dom(dom, false);
        }
        CHECK(summary == SessionSummary::from_dom(dom, false));
        return patch->node;
    };
    using core::Draft;
    using core::ElementKind;
    using core::Value;
    auto root = step(Draft::insert(core::NodeId::DOCUMENT, ElementKind::Session, base_attrs()), 1);
    auto turn = step(Draft::insert(root, ElementKind::Turn, {{"status", Value("running")}, {"started", Value(200)}}), 2);
    auto prompt = step(Draft::insert(turn, ElementKind::Message, {{"role", Value("user")}}), 3);
    step(Draft::append(prompt, "Ship the release"), 4);
    auto reply = step(Draft::insert(turn, ElementKind::Message, {{"role", Value("assistant")}}, "On it"), 5);
    step(Draft::append(reply, ", reading the changelog"), 6);
    auto decision = step(Draft::insert(turn, ElementKind::Decision, {{"kind", Value("approval")}, {"status", Value("waiting")}}), 7);
    step(Draft::set(decision, std::string(attr::STATUS), Value("answered")), 8);
    step(Draft::set(turn, std::string(attr::ENDED), Value(900)), 9);
    step(Draft::set(turn, std::string(attr::STATUS), Value("done")), 10);
    step(Draft::set(root, std::string(attr::TITLE), Value("Release")), 11);
    step(Draft::remove(decision), 12);
    // Clearing the latest end falls back to `created`; the turn's start never counts.
    step(Draft::clear(turn, std::string(attr::ENDED)), 13);

    REQUIRE(summary);
    CHECK(summary->title == "Release");
    CHECK_FALSE(summary->waiting);
    CHECK(summary->updated == 100);
}

TEST_CASE("session summary: untitled session takes the first prompt line and a waiting decision") {
    core::Dom dom;
    using core::ElementKind;
    using core::Value;
    auto root = insert(dom, core::NodeId::DOCUMENT, ElementKind::Session, base_attrs());
    auto turn = insert(dom, root, ElementKind::Turn, {{"status", Value("waiting")}, {"started", Value(500)}});
    insert(dom, turn, ElementKind::Message, {{"role", Value("user")}, {"status", Value("done")}}, "\n  Fix the flaky deploy test  \nmore detail");
    auto call = insert(dom, turn, ElementKind::Call,
                       {{"tool", Value("bash")}, {"call_id", Value("c1")}, {"status", Value("running")}, {"started", Value(900)}});
    insert(dom, call, ElementKind::Decision, {{"kind", Value("approval")}, {"status", Value("waiting")}});

    auto summary = SessionSummary::from_dom(dom, true);
    REQUIRE(summary);
    CHECK(summary->title == "Fix the flaky deploy test");
    CHECK(summary->waiting);
    CHECK(summary->pinned);
    // Started work does not count: only `created` and `ended` move the session.
    CHECK(summary->updated == 100);
    CHECK(summary->node == "key-laptop");
}

TEST_CASE("session summary: answered decision is not waiting and the title attribute wins") {
    core::Dom dom;
    using core::ElementKind;
    using core::Value;
    auto attrs = base_attrs();
    attrs.insert_or_assign("title", Value("Release notes"));
    attrs.insert_or_assign("project", Value("loa"));
    auto root = insert(dom, core::NodeId::DOCUMENT, ElementKind::Session, attrs);
    auto turn = insert(dom, root, ElementKind::Turn, {{"status", Value("running")}});
    insert(dom, turn, ElementKind::Message, {{"role", Value("user")}, {"status", Value("done")}}, "write the notes");
    insert(dom, turn, ElementKind::Decision, {{"kind", Value("question")}, {"status", Value("answered")}});

    auto summary = SessionSummary::from_dom(dom, false);
    REQUIRE(summary);
    CHECK(summary->title == "Release notes");
    CHECK(summary->project == "loa");
    CHECK_FALSE(summary->waiting);
    CHECK(summary->updated == 100);
}

TEST_CASE("session summary: a long prompt title is cut on a char boundary") {
    std::string prompt;
    for (int i = 0; i < 80; ++i) prompt += "é";
    auto title = derived_title(prompt);
    REQUIRE(title);
    std::string want;
    for (int i = 0; i < 60; ++i) want += "é";
    CHECK(*title == want + "…");
    CHECK(derived_title(" \n\t\n") == std::nullopt);
}
