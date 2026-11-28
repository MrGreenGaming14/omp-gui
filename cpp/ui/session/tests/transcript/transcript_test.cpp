// Transcript state: rows, incremental parsing, the window and follow-bottom.
// Port of `crates/loa-ui-session/src/tests/transcript.rs` (state assertions).

#include <algorithm>
#include <cmath>

#include "loa/ui/session/transcript/rows.hpp"
#include "support.hpp"

using namespace transcript_test;
namespace intent = loa::view::session::transcript::intent;

namespace {

std::vector<ItemKind> kinds(const State& s) {
    std::vector<ItemKind> out;
    for (const auto& i : s.items()) out.push_back(i.kind);
    return out;
}

bool positions_match(const State& s) {
    for (std::size_t i = 0; i < s.items().size(); ++i)
        if (s.position(s.items()[i].key) != i) return false;
    return true;
}

bool contains(Range r, std::size_t i) { return i >= r.begin && i < r.end; }

} // namespace

TEST_CASE("streaming feeds each chunk to the parser once") {
    S s;
    const auto t = s.turn();
    const auto reply = s.msg(t, "assistant", "");
    // The reply is parsed when it is first drawn, then fed each chunk.
    s.draw();
    std::string full;
    const std::string_view chunks[] = {"## Part\n\n", "Some **bold** and `code` ", "- one\n- two\n\n", "```rust\nfn main() {}\n```\n\n"};
    for (int i = 0; i < 200; ++i) {
        const auto chunk = chunks[i % 4];
        full += chunk;
        s.commit(Draft::append(reply, std::string(chunk)));
        if (i == 100) {
            // A structural change elsewhere in the turn rebuilds its rows; the reply keeps its parse.
            s.commit(Draft::insert_at(t, 0, K::Ext, {{"harness", "omp"}}, "x"));
        }
    }
    const auto* streamed = s.chat.streamed(reply);
    REQUIRE(streamed != nullptr);
    CHECK_MESSAGE(streamed->fed() == full.size(), "re-parsing per token would feed ~N\u00b2/2 bytes");
    const auto whole = md::parse(hard_breaks(full));
    CHECK(md::debug(streamed->blocks()) == md::debug(whole));
    CHECK(streamed->blocks() == whole);
    s.commit(Draft::set_text(reply, "# Final"));
    CHECK_MESSAGE(s.chat.streamed(reply) == nullptr, "the old parse is dropped");
    s.draw();
    streamed = s.chat.streamed(reply);
    REQUIRE(streamed != nullptr);
    CHECK(streamed->fed() == std::string_view("# Final").size());
    REQUIRE(streamed->blocks().size() == 1);
    CHECK(std::holds_alternative<md::Heading>(streamed->blocks()[0].v));
}

TEST_CASE("rows group calls, hide the command list and report clicks") {
    S s;
    const auto t = s.turn();
    const auto prompt = s.msg(t, "user", "fix the build");
    const auto a = s.call(t, "read", {1'000, 4'000}, "fn a() {}");
    s.call(t, "read", {2'000, 9'000}, "fn b() {}");
    s.call(t, "read", {3'000, 13'000}, "fn c() {}");
    s.add(t, K::Message, {{"role", "assistant"}, {"kind", "thinking"}, {"status", "done"}}, "weighing");
    const auto bash = s.add(t, K::Call, {{"tool", "bash"}, {"title", "cargo test"}, {"status", "error"}});
    s.add(bash, K::Result, {{"is_error", true}}, "exit 101");
    s.add(bash, K::Decision, {{"kind", "approval"}}, "rm -rf target");
    s.add(s.session, K::Ext, {{"harness", "omp"}, {"type", std::string(COMMANDS_EXT)}}, "{}");
    s.add(s.session, K::Todo, {{"state", "in_progress"}}, "Write the parser");
    s.add(s.session, K::Compaction, {{"covers", core::Value::List{core::Value(2), core::Value(3)}}}, "sum");
    using enum ItemKind;
    CHECK_MESSAGE((kinds(s.chat) == std::vector{User, Calls, Thinking, Calls, Decision, Todo, Compaction}), "the command list is no row");
    const auto group = summarize(s.dom, s.chat.items()[1].calls);
    CHECK(group.label == "Read 3 files");
    CHECK(group.duration_ms == 12'000);
    const auto failed = summarize(s.dom, s.chat.items()[3].calls);
    CHECK(failed.label == "Ran cargo test");
    CHECK(failed.failed == 1);
    CHECK(failed.status == Status::Error);
    CHECK(s.chat.items()[1].key == a);
    CHECK(s.send(msg::Rewind{prompt}) == Event{intent::Rewind{prompt}});
    CHECK_FALSE(s.chat.expanded(a));
    s.send(msg::Toggle{a});
    CHECK(s.chat.expanded(a));
    s.send(msg::Toggle{a});
    CHECK_FALSE(s.chat.expanded(a));
    const auto [task, event] = s.chat.update(msg::Copy{"fix the build"});
    CHECK_FALSE(event);
    CHECK(task == Task::of(op::WriteClipboard{"fix the build"}));
}

TEST_CASE("tool verbs read like Claude Code") {
    const std::tuple<std::string_view, std::size_t, std::string_view> cases[] = {
        {"Read", 3, "Read 3 files"},       {"edit", 1, "Edited 1 file"}, {"grep", 2, "Searched 2 patterns"},
        {"web_search", 2, "Searched the web for 2 queries"}, {"mystery", 1, "mystery"}, {"mystery", 2, "mystery 2 calls"}};
    for (const auto& [tool, n, label] : cases) {
        S s;
        const auto t = s.turn();
        for (std::size_t i = 0; i < n; ++i) s.call(t, tool, {0, 1}, "");
        CHECK_MESSAGE(s.chat.items().size() == 1, tool);
        CHECK(summarize(s.dom, s.chat.items()[0].calls).label == label);
    }
}

TEST_CASE("incremental rows match a fresh load after mixed patches") {
    S s;
    const auto t1 = s.turn();
    s.msg(t1, "user", "one");
    const auto r1 = s.call(t1, "read", {0, 1}, "a");
    s.call(t1, "read", {0, 1}, "b");
    const auto t2 = s.turn();
    s.msg(t2, "user", "two");
    const auto reply = s.msg(t2, "assistant", "hi");
    s.add(s.session, K::Digest, {{"source_seq", 1}}, "digest");
    s.commit(Draft::insert_at(t1, 1, K::Call, {{"tool", "read"}}));
    s.add(t2, K::Call, {{"tool", "bash"}});
    s.add(t2, K::Call, {{"tool", "bash"}});
    const auto t3 = s.turn();
    s.msg(t3, "user", "three");
    s.commit(Draft::set(r1, "tool", "edit"));
    s.commit(Draft::set(reply, "kind", "thinking"));
    s.commit(Draft::remove(t2));
    s.add(t3, K::Todo, {}, "later");
    const auto fresh = State::from_dom(s.dom);
    CHECK(s.chat.items() == fresh.items());
    CHECK(positions_match(s.chat));
    CHECK(std::abs(s.chat.heights().total() - fresh.heights().total()) < 1e-3);
}

TEST_CASE("a huge transcript builds only the rows around the viewport") {
    auto s = long_session(10'000);
    auto window = s.chat.window();
    CHECK_MESSAGE(window.end - window.begin < 100, "built ", window.end - window.begin, " rows");
    CHECK(contains(window, 9'999));
    CHECK_FALSE(contains(window, 0));
    const auto content = static_cast<float>(s.chat.heights().total());
    s.scroll(content - 700.0F, 700.0F);
    s.scroll(static_cast<float>(s.chat.heights().prefix(5'000)), 700.0F);
    CHECK_FALSE(s.chat.is_following());
    window = s.chat.window();
    CHECK(contains(window, 5'000));
    CHECK(window.end - window.begin < 100);
    CHECK_FALSE(contains(window, 9'999));
    const auto last = s.chat.items()[9'999].key;
    s.commit(Draft::append(last, " more"));
    CHECK_MESSAGE(s.chat.window() == window, "a token far below leaves the window alone");
}

TEST_CASE("a row growing above the viewport keeps the reading position") {
    auto s = long_session(200);
    const auto content = static_cast<float>(s.chat.heights().total());
    s.scroll(content - 500.0F, 500.0F);
    const auto reading = static_cast<float>(s.chat.heights().prefix(150));
    s.scroll(reading, 500.0F);
    const auto first_visible = s.chat.heights().find(reading);
    const auto key = s.chat.items()[3].key;
    const auto old = *s.chat.heights().get(3);
    const auto [task, _] = s.chat.update(msg::Measured{key, old + 120.0F});
    CHECK(task == Task::of(op::ScrollBy{120.0F}));
    CHECK(s.chat.metrics().offset == reading + 120.0F);
    CHECK(s.chat.heights().find(s.chat.metrics().offset) == first_visible);
    // The measurement survives the turn's rows being rebuilt.
    const auto turn = s.chat.items()[3].top;
    s.commit(Draft::insert_at(turn, 0, K::Ext, {{"harness", "omp"}}));
    CHECK(s.chat.heights().get(*s.chat.position(key)) == old + 120.0F);
}

TEST_CASE("follow-bottom stays pinned while streaming and stops when the user scrolls up") {
    auto s = long_session(300);
    const auto content = static_cast<float>(s.chat.heights().total());
    s.scroll(content - 600.0F, 600.0F);
    CHECK(s.chat.is_following());
    // Streaming grows the content under a still viewport: the view keeps following and snaps.
    const auto t = s.turn();
    const auto reply = s.msg(t, "assistant", "");
    for (int i = 0; i < 20; ++i) s.commit(Draft::append(reply, "token\n"));
    const auto [snap, _] = s.chat.update(msg::Scrolled{{content - 600.0F, 600.0F, content + 400.0F}});
    CHECK_MESSAGE(s.chat.is_following(), "content growing under a still viewport keeps following");
    CHECK(snap == Task::of(op::SnapToEnd{}));
    CHECK(s.chat.window().end == s.chat.items().size());
    s.scroll(1'000.0F, 600.0F);
    CHECK_FALSE(s.chat.is_following());
    const auto window = s.chat.window();
    const auto t2 = s.turn();
    s.msg(t2, "user", "new prompt");
    CHECK(s.chat.window() == window);
    const auto [task, __] = s.chat.update(msg::JumpToLatest{});
    CHECK(task == Task::of(op::SnapToEnd{}));
    CHECK(s.chat.is_following());
    CHECK(s.chat.window().end == s.chat.items().size());
}

TEST_CASE("another session starts following and its first scroll report keeps it") {
    auto s = long_session(300);
    const auto content = static_cast<float>(s.chat.heights().total());
    const auto at = [&](float offset) { return msg::Scrolled{{offset, 600.0F, content}}; };
    s.send(at(content - 600.0F));
    s.send(at(1'000.0F));
    CHECK_FALSE(s.chat.is_following());
    // Reloading the same session (a resync) keeps the reading position.
    s.chat.load(s.dom);
    CHECK_FALSE(s.chat.is_following());
    // Another session follows its newest rows, even when the scroll area then reports a smaller
    // offset left over from the first one.
    s.commit(Draft::set(s.session, "id", "s2"));
    s.chat.load(s.dom);
    CHECK(s.chat.is_following());
    s.send(msg::Scrolled{{200.0F, 600.0F, content}});
    CHECK_MESSAGE(s.chat.is_following(), "the old offset is not a scroll up");
}

TEST_CASE("historical omp subagent_progress exts are suppressed while other exts remain") {
    S s{S::Empty{}};
    std::vector<core::Patch> patches;
    const auto commit = [&](Draft d) {
        auto patch = s.dom.commit(std::move(d), 0);
        REQUIRE(patch.has_value());
        s.chat.apply(*patch, s.dom);
        patches.push_back(*patch);
        return patch->node;
    };
    s.session = commit(Draft::insert(NodeId::DOCUMENT, K::Session, {{"id", "s1"}, {"harness", "omp"}, {"node", "n1"}, {"created", 0}}));
    const auto session = s.session;
    const auto t = commit(Draft::insert(session, K::Turn, {{"status", "done"}}));
    commit(Draft::insert(t, K::Message, {{"role", "user"}, {"status", "done"}}, "run agents"));
    const auto in_turn = commit(Draft::insert(t, K::Ext, {{"harness", "omp"}, {"type", "subagent_progress"}},
                                              R"({"agent":"explore","progress":{"step":"old"}})"));
    const auto notice = commit(Draft::insert(t, K::Ext, {{"harness", "omp"}, {"type", "notice"}}, R"({"message":"notice text"})"));
    const auto other = commit(Draft::insert(t, K::Ext, {{"harness", "claude-code"}, {"type", "subagent_progress"}}, R"({"step":"other-harness"})"));
    const auto untyped = commit(Draft::insert(t, K::Ext, {{"harness", "omp"}}, R"({"raw":"kept"})"));
    const auto job = commit(Draft::insert(
        t, K::Job, {{"kind", "subagent"}, {"status", "done"}, {"id", "sub-1"}, {"agent", "explore"}, {"title", "Survey"}}));
    commit(Draft::append(job, "{\"id\":\"sub-1\",\"currentTool\":\"read\"}\n"));
    commit(Draft::insert(t, K::Message, {{"role", "assistant"}, {"status", "done"}}, "All done."));
    commit(Draft::insert(session, K::Ext, {{"harness", "omp"}, {"type", std::string(COMMANDS_EXT)}}, "{}"));
    const auto at_root = commit(Draft::insert(session, K::Ext, {{"harness", "omp"}, {"type", "subagent_progress"}}, R"({"progress":{"step":"orphan"}})"));

    using enum ItemKind;
    CHECK((kinds(s.chat) == std::vector{User, Ext, Ext, Ext, Job, Assistant}));
    std::vector<NodeId> keys;
    for (const auto& i : s.chat.items()) keys.push_back(i.key);
    const auto has = [&](NodeId id) { return std::find(keys.begin(), keys.end(), id) != keys.end(); };
    CHECK_FALSE(has(in_turn));
    CHECK_FALSE(has(at_root));
    CHECK(has(notice));
    CHECK(has(other));
    CHECK(has(untyped));
    CHECK(has(job));

    // Replaying the exact journal patches onto a fresh DOM and transcript matches `from_dom`.
    core::Dom replayed;
    State chat;
    for (const auto& p : patches) {
        REQUIRE(replayed.apply(p).has_value());
        chat.apply(p, replayed);
    }
    CHECK(replayed == s.dom);
    CHECK(replayed.contains(in_turn));
    CHECK(replayed.contains(at_root));
    CHECK(chat.items() == s.chat.items());
    const auto snapshot = State::from_dom(replayed);
    CHECK(snapshot.items() == s.chat.items());
    CHECK(positions_match(s.chat));
    CHECK(std::abs(snapshot.heights().total() - s.chat.heights().total()) < 1e-3);
}

TEST_CASE("item kind names and the commands ext name are the Rust ones") {
    CHECK(COMMANDS_EXT == "available_commands_update");
    CHECK(name(ItemKind::User) == "User");
    CHECK(name(ItemKind::Compaction) == "Compaction");
    CHECK(name(ItemKind::Ext) == "Ext");
}

TEST_CASE("leading and trailing bound long output") {
    std::string many;
    for (int i = 0; i < 100; ++i) many += "line " + std::to_string(i) + "\n";
    const auto [head, left] = leading(many);
    CHECK(std::count(head.begin(), head.end(), '\n') == static_cast<long>(MAX_LINES - 1));
    CHECK(head.size() + left == many.size());
    const auto [tail, start] = trailing(many);
    CHECK(tail.starts_with("line 60\n"));
    CHECK(start + tail.size() == many.size());
    const std::string big(MAX_BYTES + 10, 'x');
    CHECK(leading(big).second == 10);
}
