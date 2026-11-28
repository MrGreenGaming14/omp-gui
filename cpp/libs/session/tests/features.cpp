// Session features over the journal-backed library, with a scripted harness.
// Port of `crates/loa-session/tests/features.rs`.

#include <doctest/doctest.h>

#include <asio.hpp>

#include <algorithm>
#include <fstream>
#include <mutex>
#include <optional>
#include <sstream>
#include <thread>

#include "loa/bus/bus.hpp"
#include "loa/bus/store.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/session/hooks.hpp"
#include "loa/session/library.hpp"
#include "loa/session/plan.hpp"
#include "loa/session/session.hpp"

namespace f = loa::session;
namespace c = loa::contracts;
namespace core = loa::core;
namespace bus = loa::bus;
namespace attr = loa::core::attr;
namespace fs = std::filesystem;
using core::Draft;
using core::NodeId;
using core::Value;
using K = core::ElementKind;

namespace {

template <typename T>
T run(c::Awaitable<T> task) {
    asio::io_context io;
    std::optional<T> out;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> { out = co_await std::move(task); }, asio::detached);
    io.run();
    REQUIRE(out.has_value());
    return std::move(*out);
}

template <typename T>
c::Awaitable<T> ready(T value) {
    co_return value;
}

struct Nobody final : bus::Authorize {
    std::expected<void, bus::Denied> authorize(const bus::SessionId&, const bus::NodeKey&, const Draft&,
                                               const core::Dom&) const override {
        return std::unexpected(bus::Denied{"local only"});
    }
};

std::unique_ptr<f::Library> library(const fs::path& dir) {
    auto store = bus::Store::make(dir);
    REQUIRE(store.has_value());
    bus::NodeKey key;
    key.bytes.fill(1);
    return std::make_unique<f::Library>(std::move(*store), std::make_shared<bus::Bus>(), key,
                                        std::make_shared<Nobody>());
}

core::Attrs attrs(std::initializer_list<std::pair<std::string_view, Value>> pairs) {
    core::Attrs out;
    for (const auto& [k, v] : pairs) out.insert_or_assign(std::string(k), v);
    return out;
}

c::Result<core::Patch> commit(core::PatchSink& sink, Draft draft, std::optional<std::int64_t> ts = {}) {
    auto r = ts ? sink.commit_at(std::move(draft), *ts) : sink.commit(std::move(draft));
    if (!r) return std::unexpected(c::AdapterError(c::AdapterError::Commit{r.error()}));
    return std::move(*r);
}

struct Seen {
    std::mutex mutex;
    std::vector<std::string> prompts;
    std::vector<core::SessionMode> modes;
    std::vector<std::optional<std::string>> contexts;
    std::vector<core::Dom> forked;
};

// Records prompts and writes each as a turn with an echo reply.
class Echo final : public c::HarnessSession {
  public:
    Echo(std::shared_ptr<core::PatchSink> sink, NodeId root, std::shared_ptr<Seen> seen)
        : sink_(std::move(sink)), root_(root), seen_(std::move(seen)) {}

    c::Awaitable<c::Result<>> send(c::Prompt prompt) const override { co_return send_now(prompt); }
    c::Awaitable<c::Result<>> interrupt() const override { co_return c::Result<>{}; }
    c::Awaitable<c::Result<>> answer(NodeId decision, c::Answer) const override {
        co_return std::unexpected(c::AdapterError(c::AdapterError::UnknownDecision{decision}));
    }
    c::Awaitable<c::Result<>> set_mode(core::SessionMode mode) const override {
        {
            std::scoped_lock lock(seen_->mutex);
            seen_->modes.push_back(mode);
        }
        co_return std::unexpected(c::AdapterError(c::AdapterError::Unsupported{"echo", "set_mode"}));
    }
    std::string_view harness() const override { return "echo"; }
    c::SessionState state() const override { return c::SessionState::Idle; }
    c::Awaitable<c::Result<>> wait_idle() const override { co_return c::Result<>{}; }
    c::Awaitable<c::Result<>> shutdown() const override { co_return c::Result<>{}; }

  private:
    c::Result<> send_now(const c::Prompt& prompt) const {
        {
            std::scoped_lock lock(seen_->mutex);
            seen_->prompts.push_back(prompt.text);
        }
        auto turn = commit(*sink_, Draft::insert(root_, K::Turn, attrs({{attr::STATUS, Value("done")}})));
        if (!turn) return std::unexpected(turn.error());
        if (auto r = commit(*sink_, Draft::insert(turn->node, K::Message, attrs({{attr::ROLE, Value("user")}}),
                                                  prompt.text));
            !r)
            return std::unexpected(r.error());
        if (auto r = commit(*sink_, Draft::insert(turn->node, K::Message, attrs({{attr::ROLE, Value("assistant")}}),
                                                  "echo " + prompt.text));
            !r)
            return std::unexpected(r.error());
        return {};
    }

    std::shared_ptr<core::PatchSink> sink_;
    NodeId root_;
    std::shared_ptr<Seen> seen_;
};

class EchoAdapter final : public c::Adapter {
  public:
    explicit EchoAdapter(bool native_fork = false) : native_fork_(native_fork) {}

    std::shared_ptr<Seen> seen = std::make_shared<Seen>();

    std::string_view harness() const override { return "echo"; }
    c::Capabilities capabilities() const override {
        c::Capabilities caps;
        if (native_fork_) caps.features.push_back(c::Feature::Fork);
        caps.import = true;
        caps.export_ = true;
        return caps;
    }
    c::Awaitable<c::Result<c::SessionPtr>> start(c::SessionConfig config,
                                                 std::shared_ptr<core::PatchSink> sink) const override {
        {
            std::scoped_lock lock(seen->mutex);
            seen->contexts.push_back(config.context);
        }
        auto root = commit(*sink, Draft::insert(NodeId::DOCUMENT, K::Session,
                                                attrs({{attr::ID, Value(config.session_id)},
                                                       {attr::HARNESS, Value("echo")},
                                                       {attr::NODE, Value("me")},
                                                       {attr::CREATED, Value(std::int64_t{1})}})));
        if (!root) co_return std::unexpected(root.error());
        co_return session(sink, root->node);
    }
    c::Awaitable<c::Result<c::SessionPtr>> resume(c::SessionConfig, const core::Dom& dom,
                                                  std::shared_ptr<core::PatchSink> sink) const override {
        co_return session(sink, dom.root()->id);
    }
    c::Awaitable<c::Result<c::SessionPtr>> fork(c::SessionConfig, const core::Dom& dom,
                                                std::shared_ptr<core::PatchSink> sink) const override {
        {
            std::scoped_lock lock(seen->mutex);
            seen->forked.push_back(dom);
        }
        co_return session(sink, dom.root()->id);
    }
    c::Result<> import(const fs::path& native, core::PatchSink& sink) const override {
        std::ifstream in(native, std::ios::binary);
        if (!in)
            return std::unexpected(c::AdapterError(c::AdapterError::Io{loa::Error(loa::ErrorCode::io, "missing")}));
        std::stringstream text;
        text << in.rdbuf();
        auto root = commit(sink, Draft::insert(NodeId::DOCUMENT, K::Session,
                                               attrs({{attr::ID, Value("native-7")},
                                                      {attr::HARNESS, Value("echo")},
                                                      {attr::NODE, Value("")},
                                                      {attr::CREATED, Value(std::int64_t{5})}})),
                           5);
        if (!root) return std::unexpected(root.error());
        std::string line;
        while (std::getline(text, line)) {
            auto turn = commit(sink, Draft::insert(root->node, K::Turn, attrs({{attr::STATUS, Value("done")}})), 6);
            if (!turn) return std::unexpected(turn.error());
            if (auto r = commit(sink, Draft::insert(turn->node, K::Message, attrs({{attr::ROLE, Value("user")}}), line),
                                6);
                !r)
                return std::unexpected(r.error());
        }
        return {};
    }
    c::Result<> export_(const core::Dom& dom, const fs::path& out) const override {
        std::string text;
        for (const auto& node : dom.iter()) {
            if (node.kind != K::Message) continue;
            if (!text.empty()) text += '\n';
            text += node.text;
        }
        std::ofstream(out, std::ios::binary | std::ios::trunc) << text;
        return {};
    }

    c::SessionPtr session(std::shared_ptr<core::PatchSink> sink, NodeId root) const {
        return std::make_unique<Echo>(std::move(sink), root, seen);
    }

  private:
    bool native_fork_;
};

c::SessionConfig config(std::string id) {
    c::SessionConfig out;
    out.session_id = std::move(id);
    out.node = "me";
    return out;
}

c::Prompt prompt(std::string text) {
    c::Prompt out;
    out.text = std::move(text);
    out.by = "me";
    return out;
}

// A session `s1` with two turns.
std::vector<NodeId> two_turns(const f::Library& lib, const EchoAdapter& adapter) {
    auto live = lib.create("s1");
    REQUIRE(live.has_value());
    auto session = run(adapter.start(config("s1"), live->writer));
    REQUIRE(session.has_value());
    REQUIRE(run((*session)->send(prompt("first"))).has_value());
    REQUIRE(run((*session)->send(prompt("second"))).has_value());
    auto seqs = lib.turn_seqs("s1");
    REQUIRE(seqs.has_value());
    std::vector<NodeId> out;
    for (auto [node, _] : *seqs) out.push_back(node);
    return out;
}

core::Node root_of(const f::Library& lib, std::string_view id) {
    auto dom = lib.load(id);
    REQUIRE(dom.has_value());
    REQUIRE(dom->root() != nullptr);
    return *dom->root();
}

core::Dom dom_of(const f::Live& live) {
    return live.writer->with_dom([](const core::Dom& dom) { return dom; });
}

std::string read_text(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream text;
    text << in.rdbuf();
    return text.str();
}

void write_text(const fs::path& path, std::string_view text) {
    std::ofstream(path, std::ios::binary | std::ios::trunc) << text;
}

bool contains(std::string_view hay, std::string_view needle) { return hay.find(needle) != std::string_view::npos; }

// Answers from a script and records the prompts.
struct Scripted final : c::Summarizer {
    mutable std::mutex mutex;
    mutable std::vector<std::string> answers;
    mutable std::vector<std::string> asked;

    c::Awaitable<std::expected<std::string, std::string>> summarize(std::string, std::string input) const override {
        std::string answer;
        {
            std::scoped_lock lock(mutex);
            asked.push_back(std::move(input));
            answer = answers.front();
            answers.erase(answers.begin());
        }
        co_return answer;
    }
};

} // namespace

TEST_CASE("sessions persist, fork, rewind, import and export") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir.has_value());
    EchoAdapter adapter(true);
    auto lib = library(dir->path() / "store");
    auto turns = two_turns(*lib, adapter);
    auto dom = lib->load("s1").value();
    CHECK(lib->close("s1").value());
    lib = library(dir->path() / "store");
    CHECK_MESSAGE(lib->load("s1").value() == dom, "a closed session reloads from its journal");
    CHECK(dom_of(lib->open("s1").value()) == dom);

    // Native fork: the journal fork labels the root, nothing more is written.
    auto source = lib->through_turn("s1", turns[0]).value();
    auto branch = run(f::fork(adapter, *lib, source, config("b1")));
    REQUIRE(branch.has_value());
    auto forked = lib->load("b1").value();
    const auto* root = forked.root();
    CHECK(root->str_attr(attr::ID) == "b1");
    CHECK(root->str_attr(attr::PARENT_SESSION) == "s1");
    CHECK(root->int_attr(attr::FORKED_AT) == static_cast<std::int64_t>(source.seq()));
    CHECK(forked.seq() == source.seq() + 3);
    CHECK(adapter.seen->forked.at(0) == forked);
    CHECK(run((*branch)->send(prompt("third"))).has_value());

    // Fallback rewind: a fresh session seeded with the history before the turn.
    EchoAdapter fallback;
    auto target = lib->before_turn("s1", turns[1]).value();
    CHECK((target.get(turns[1]) == nullptr && target.get(turns[0]) != nullptr));
    REQUIRE(run(f::rewind(fallback, *lib, dom, target, config("r1"))).has_value());
    const auto context = fallback.seen->contexts.at(0).value();
    CHECK_MESSAGE((contains(context, "user: first") && !contains(context, "second")), context);
    CHECK(root_of(*lib, "r1").int_attr(attr::FORKED_AT) == static_cast<std::int64_t>(target.seq()));
    CHECK_MESSAGE(lib->load("s1").value() == dom, "the old timeline is untouched");

    // Import, export and XML.
    const auto native = dir->path() / "native.txt";
    write_text(native, "alpha\nbeta");
    REQUIRE(lib->import(adapter, native, "imp-1", "my-node").has_value());
    auto imported = root_of(*lib, "imp-1");
    CHECK(imported.str_attr(attr::ID) == "imp-1");
    CHECK(imported.str_attr(attr::NATIVE_ID) == "native-7");
    CHECK(imported.str_attr(attr::NODE) == "my-node");
    const auto out = dir->path() / "out.txt";
    REQUIRE(lib->export_(adapter, "imp-1", out).has_value());
    CHECK(read_text(out) == "alpha\nbeta");
    REQUIRE(lib->export_xml("imp-1", out).has_value());
    CHECK(contains(read_text(out), "<session"));
    CHECK_FALSE(lib->import(adapter, dir->path() / "missing", "imp-2", "me").has_value());
    CHECK_MESSAGE(!lib->load("imp-2").has_value(), "a failed import leaves nothing behind");

    // A subagent file imports as a linked child, reused until it changes.
    const auto child_file = dir->path() / "child.txt";
    write_text(child_file, "sub task");
    auto live = lib->live("s1").value();
    auto job = live.writer->commit(Draft::insert(NodeId{1}, K::Job, attrs({{attr::KIND, Value("subagent")}})));
    REQUIRE(job.has_value());
    auto id = lib->import_child(adapter, "s1", job->node, child_file, "me").value();
    CHECK(root_of(*lib, id).int_attr(attr::FORKED_AT) == static_cast<std::int64_t>(job->seq));
    const auto seq = lib->load(id).value().seq();
    CHECK(lib->import_child(adapter, "s1", job->node, child_file, "me").value() == id);
    CHECK(lib->load(id).value().seq() == seq);
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
    write_text(child_file, "sub task\nmore work");
    REQUIRE(lib->import_child(adapter, "s1", job->node, child_file, "me").has_value());
    auto child = lib->load(id).value();
    CHECK(std::ranges::count_if(child.iter(), [](const core::Node& n) { return n.kind == K::Turn; }) == 2);
    REQUIRE(lib->remove("s1").has_value());
    CHECK((!lib->load("s1").has_value() && !lib->live("s1").has_value()));
}

TEST_CASE("plan mode denies tool approvals until the plan is approved") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir.has_value());
    EchoAdapter adapter;
    auto lib = library(dir->path());
    auto live = lib->create("p1").value();
    auto session = run(adapter.start(config("p1"), live.writer)).value();
    auto gate = f::plan::PlanGate::make();
    CHECK_MESSAGE(!gate->permit("p1").has_value(), "fails closed");
    CHECK_FALSE(gate->enforceable("p1"));
    gate->enforce_through_approvals("p1");
    CHECK(gate->enforceable("p1"));
    auto permit = gate->permit("p1").value();
    auto step = f::plan::begin(dom_of(live), *live.writer).value();
    CHECK(gate->active("p1"));
    CHECK(f::plan::open_step(dom_of(live)) == step);

    REQUIRE(run(session->send(prompt(f::plan::wrap_prompt("add a cache")))).has_value());
    const auto turn = lib->turn_seqs("p1").value().at(0).first;
    const auto approval = live.writer
                              ->commit(Draft::insert(turn, K::Decision,
                                                     attrs({{attr::KIND, Value("approval")},
                                                            {attr::STATUS, Value("waiting")}}),
                                                     "rm"))
                              .value()
                              .node;
    auto deny = gate->verdict("p1", *dom_of(live).get(approval));
    REQUIRE(deny.has_value());
    CHECK(deny->values == std::vector<std::string>{"deny"});
    CHECK(deny->reason == std::string(f::plan::DENY_REASON));
    CHECK(deny->by == "plan-mode");

    auto dom = dom_of(live);
    const std::string text(f::plan::plan_text(dom, turn).value());
    const auto root = dom.root()->id;
    auto review = f::submit_plan(*live.writer, root, step, text).value();
    dom = dom_of(live);
    CHECK((f::plan::is_app_plan(dom, review) && !f::plan::is_app_plan(dom, approval)));
    auto rejected = run(f::plan::answer_plan(*session, dom, *live.writer, review, "me",
                                             f::plan::verdict::Reject{"smaller"}, std::move(permit)));
    REQUIRE(rejected.has_value());
    auto* revise = std::get_if<f::plan::outcome::Revise>(&*rejected);
    REQUIRE_MESSAGE(revise != nullptr, "reject keeps plan mode");
    CHECK((contains(revise->prompt, "smaller") && revise->prompt.ends_with(f::plan::PLAN_STEP) && gate->active("p1")));
    review = f::submit_plan(*live.writer, root, revise->step, "1. small cache").value();
    dom = dom_of(live);
    auto approved = run(f::plan::answer_plan(*session, dom, *live.writer, review, "me",
                                             f::plan::verdict::Edit{"1. tiny cache"}, std::move(revise->permit)));
    REQUIRE(approved.has_value());
    auto* execute = std::get_if<f::plan::outcome::Execute>(&*approved);
    REQUIRE_MESSAGE(execute != nullptr, "edit approves");
    CHECK((contains(execute->prompt, "1. tiny cache") && !gate->active("p1")));
    dom = dom_of(live);
    CHECK(dom.root()->str_attr(attr::MODE) == "normal");
    CHECK(*dom.get(review)->attr(attr::ANSWER) == Value(Value::List{Value("edit")}));
    CHECK(adapter.seen->modes == std::vector<core::SessionMode>{core::SessionMode::Normal});
    CHECK(gate->verdict("p1", *dom_of(live).get(approval)) == std::nullopt);
}

TEST_CASE("compaction and the advisor use the small model") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir.has_value());
    EchoAdapter adapter;
    auto lib = library(dir->path() / "store");
    auto turns = two_turns(*lib, adapter);
    auto live = lib->live("s1").value();
    auto running = run(adapter.resume(config("s1"), dom_of(live), live.writer)).value();
    auto model = std::make_shared<Scripted>();
    model->answers = {"They said first and second.", "You ignored the rule.", "OK."};
    auto source = dom_of(live);
    f::Compaction request{*running, source, *live.writer, config("s1-c"), "smol", "keep it short"};
    auto next = run(f::compact(adapter, *lib, *model, std::move(request)));
    REQUIRE(next.has_value());
    REQUIRE(next->has_value());
    const auto asked = model->asked.at(0);
    CHECK((contains(asked, "user: first") && contains(asked, "keep it short")));
    CHECK(contains(adapter.seen->contexts.at(1).value(), "They said first and second."));
    auto dom = lib->load("s1").value();
    const core::Node* compaction = nullptr;
    for (const auto& n : dom.iter())
        if (n.kind == K::Compaction) compaction = &n;
    REQUIRE(compaction != nullptr);
    Value::List covers;
    for (auto t : turns) covers.emplace_back(static_cast<std::int64_t>(t.value));
    CHECK(*compaction->attr(attr::COVERS) == Value(covers));
    CHECK(root_of(*lib, "s1-c").str_attr(attr::PARENT_SESSION) == "s1");
    const auto history = f::translated_history(dom).value();
    CHECK_MESSAGE((history.starts_with("Summary of earlier turns:\nThey said") && !contains(history, "user: first")),
                  history);

    f::hooks::Advisor advisor(model);
    auto note = run(advisor.review(dom, turns[0], "Use tabs.")).value();
    REQUIRE(note.has_value());
    CHECK((contains(model->asked.at(1), "Use tabs.") && contains(model->asked.at(1), "user: first")));
    auto message = f::hooks::record_note(*live.writer, turns[0], *note).value();
    dom = dom_of(live);
    CHECK(dom.get(message)->str_attr(attr::BY) == f::hooks::ADVISOR);
    CHECK(contains(f::turn_transcript(dom, turns[0]), "advisor: You ignored"));
    CHECK_MESSAGE(run(advisor.review(dom, turns[0], std::nullopt)).value() == std::nullopt, "OK means no note");
}

TEST_CASE("rules feed the context and each turn") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir.has_value());
    const auto home = dir->path() / "home";
    const auto cwd = dir->path() / "project";
    const std::pair<fs::path, std::string_view> files[] = {
        {home / ".omp/agent/rules/style.md", "Use tabs."},
        {cwd / ".omp/rules/style.md", "---\nalwaysApply: true\n---\nUse four spaces."},
        {cwd / ".omp/rules/db.md", "---\ndescription: \"Database migrations\"\nglobs: migrations/**\n---\nNever drop columns."},
    };
    for (const auto& [path, text] : files) {
        fs::create_directories(path.parent_path());
        write_text(path, text);
    }
    auto rules = f::hooks::discover_rules(cwd, home);
    REQUIRE(rules.size() == 2);
    CHECK(rules[0].name == "db");
    CHECK(rules[1].name == "style");
    CHECK(rules[0].description == "Database migrations");
    CHECK_FALSE(rules[0].always);
    CHECK(rules[0].body == "Never drop columns.");
    CHECK(rules[1].always);
    auto text = f::hooks::rules_context(rules).value();
    CHECK_MESSAGE((contains(text, "Use four spaces.") && !contains(text, "Use tabs.")), "project overrides global: ", text);
    CHECK_MESSAGE((contains(text, "db: Database migrations") && !contains(text, "Never drop")), text);
    CHECK_FALSE(f::hooks::rules_context({}).has_value());
    auto cfg = config("h1");
    auto turn_hooks = f::hooks::TurnHooks::start(cfg, text);
    CHECK(contains(cfg.context.value(), "Use four spaces."));
    CHECK(turn_hooks.before_turn(text, "go") == "go");
    turn_hooks.advise("Mind the rule.");
    CHECK(turn_hooks.pending_note() == "Mind the rule.");
    const auto sent = turn_hooks.before_turn("Use tabs only.", "next");
    CHECK_MESSAGE((sent.starts_with("[The project rules changed]\nUse tabs only.") &&
                   contains(sent, "[Advisor note on your last turn]\nMind the rule.") && sent.ends_with("next")),
                  sent);
    CHECK(turn_hooks.before_turn("Use tabs only.", "again") == "again");
    CHECK(turn_hooks.before_turn(std::nullopt, "x") == "[The project rules were removed]\n\nx");
}

TEST_CASE("translated history and transcript text") {
    core::MemorySink sink;
    auto root = sink.commit(Draft::insert(NodeId::DOCUMENT, K::Session,
                                          attrs({{attr::ID, Value("t1")},
                                                 {attr::HARNESS, Value("echo")},
                                                 {attr::NODE, Value("me")},
                                                 {attr::CREATED, Value(std::int64_t{1})}})))
                    .value()
                    .node;
    auto turn = sink.commit(Draft::insert(root, K::Turn, attrs({{attr::STATUS, Value("done")}}))).value().node;
    REQUIRE(sink.commit(Draft::insert(turn, K::Message, attrs({{attr::ROLE, Value("user")}}), "hi")).has_value());
    REQUIRE(sink.commit(Draft::insert(turn, K::Message,
                                      attrs({{attr::ROLE, Value("assistant")}, {attr::KIND, Value("thinking")}}),
                                      "hmm"))
                .has_value());
    REQUIRE(sink.commit(Draft::insert(turn, K::Message, attrs({{attr::ROLE, Value("assistant")}}), "hello"))
                .has_value());
    CHECK(f::turn_transcript(sink.dom(), turn) == "user: hi\nassistant: hello\n");
    CHECK(f::translated_history(sink.dom()).value() == "user: hi\nassistant: hello\n\n");
    CHECK(f::translated_history(core::Dom()).error().kind() == f::Error::Kind::NoSession);
    CHECK(f::compaction_prompt("H\n", "  ").find("Also follow") == std::string::npos);
}
