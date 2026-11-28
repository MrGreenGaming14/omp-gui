// The transcript view renders rows from the state and reports clicks (offscreen).
// Port of the view assertions of `crates/loa-ui-session/src/tests/transcript.rs`.

#include <QApplication>
#include <QLabel>
#include <QPushButton>
#include <QScrollArea>
#include <QScrollBar>

#include <set>

#include "loa/ui/kit/testkit.hpp"
#include "loa/ui/session/transcript/view.hpp"
#include "loa/ui/session/transcript/rows.hpp"
#include "support.hpp"

using namespace transcript_test;

namespace {

QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_session_tests";
    static char* argv[] = {name, nullptr};
    if (auto* a = qobject_cast<QApplication*>(QCoreApplication::instance())) return *a;
    static auto* a = new QApplication(argc, argv);
    return *a;
}

void settle() {
    for (int i = 0; i < 3; ++i) {
        QApplication::processEvents();
        QCoreApplication::sendPostedEvents(nullptr, QEvent::DeferredDelete);
    }
}

// A view over `s`; `live` feeds every message back to the state and re-renders, as the app does.
struct V {
    S& s;
    TranscriptView view;
    std::vector<Message> seen;
    bool live = false;

    explicit V(S& s, bool live = false) : s(s), live(live) {
        app();
        view.set_slot([](Slot slot) { return new QLabel(QString::fromStdString("card for " + loa::core::to_string(slot.node))); });
        view.resize(800, 700);
        QObject::connect(&view, &TranscriptView::message, [this](const Message& m) {
            seen.push_back(m);
            if (!this->live) return;
            auto [task, _] = this->s.chat.update(m);
            render();
            view.apply(task);
        });
        view.show();
        render();
        settle();
    }
    void render() { view.render(s.chat, s.dom); }
    void refresh() {
        render();
        settle();
    }
    bool shows(std::string_view label) {
        const auto want = QString::fromUtf8(label.data(), static_cast<qsizetype>(label.size()));
        for (auto* w : view.findChildren<QWidget*>()) {
            if (!w->isVisibleTo(&view)) continue;
            if (const auto* l = qobject_cast<QLabel*>(w); l && l->text() == want) return true;
            if (const auto* b = qobject_cast<QPushButton*>(w); b && b->text() == want) return true;
            if (w->accessibleName() == want && !qobject_cast<QPushButton*>(w)) return true;
        }
        return false;
    }
    // The messages a click on `label` sends, without the size and scroll reports.
    std::vector<Message> click(std::string_view label) {
        seen.clear();
        if (!loa::ui::kit::testkit::click_label(view, label)) return {};
        std::vector<Message> out;
        for (auto& m : seen)
            if (!std::holds_alternative<msg::Measured>(m) && !std::holds_alternative<msg::Scrolled>(m)) out.push_back(m);
        return out;
    }
};

} // namespace

TEST_CASE("transcript view: rows collapse, expand and report clicks") {
    S s;
    const auto t = s.turn();
    const auto prompt = s.msg(t, "user", "fix the build");
    const auto a = s.call(t, "read", {1'000, 4'000}, "fn a() {}");
    s.call(t, "read", {2'000, 9'000}, "fn b() {}");
    s.call(t, "read", {3'000, 13'000}, "fn c() {}");
    const auto thought = s.add(t, K::Message, {{"role", "assistant"}, {"kind", "thinking"}, {"status", "done"}}, "weighing");
    const auto bash = s.add(t, K::Call, {{"tool", "bash"}, {"title", "cargo test"}, {"status", "error"}});
    s.add(bash, K::Result, {{"is_error", true}}, "exit 101");
    const auto decision = s.add(bash, K::Decision, {{"kind", "approval"}}, "rm -rf target");
    s.add(s.session, K::Ext, {{"harness", "omp"}, {"type", std::string(COMMANDS_EXT)}}, "{}");
    s.add(s.session, K::Todo, {{"state", "in_progress"}}, "Write the parser");
    s.add(s.session, K::Compaction, {{"covers", core::Value::List{core::Value(2), core::Value(3)}}}, "sum");
    V v(s);
    for (const std::string label : {std::string("Read 3 files"), std::string("12s"), std::string("Thought"), std::string("Ran cargo test"),
                                    std::string("1 failed"), std::string("\u25d0"), std::string("Compacted 2 turns"),
                                    "card for " + loa::core::to_string(decision)})
        CHECK_MESSAGE(v.shows(label), label);
    for (const auto hidden : {"fn b() {}", "weighing", "rm -rf target", "exit 101", "available_commands_update"})
        CHECK_MESSAGE(!v.shows(hidden), hidden);
    CHECK(v.click("Read 3 files") == std::vector<Message>{msg::Toggle{a}});
    CHECK(v.click("Rewind") == std::vector<Message>{msg::Rewind{prompt}});
    CHECK(v.click("Copy message") == std::vector<Message>{msg::Copy{"fix the build"}});
    for (const auto key : {a, thought, bash}) s.send(msg::Toggle{key});
    v.refresh();
    for (const auto label : {"fn b() {}", "weighing", "Error", "exit 101"}) CHECK_MESSAGE(v.shows(label), label);
}

TEST_CASE("transcript view: a huge transcript builds only the rows around the viewport") {
    auto s = long_session(10'000);
    V v(s, true);
    CHECK(v.view.list()->rows() < 100);
    CHECK(v.shows("message 9999"));
    CHECK_FALSE(v.shows("message 0"));
    // Following: the scroll area sits at the bottom.
    auto* bar = v.view.scroll_area()->verticalScrollBar();
    CHECK(bar->value() == bar->maximum());
}

TEST_CASE("transcript view: follow-bottom pins while streaming, scrolling up shows the jump button") {
    auto s = long_session(300);
    V v(s, true);
    auto* bar = v.view.scroll_area()->verticalScrollBar();
    const auto t = s.turn();
    const auto reply = s.msg(t, "assistant", "");
    for (int i = 0; i < 30; ++i) {
        s.commit(Draft::append(reply, "streamed line " + std::to_string(i) + "\n\n"));
        v.refresh();
        CHECK(bar->value() == bar->maximum());
    }
    CHECK(s.chat.is_following());
    CHECK_FALSE(v.shows("Jump to latest \u2193"));
    bar->setValue(bar->maximum() / 2);
    settle();
    CHECK_FALSE(s.chat.is_following());
    CHECK(v.shows("Jump to latest \u2193"));
    const auto before = bar->value();
    s.commit(Draft::append(reply, "more\n"));
    v.refresh();
    CHECK(bar->value() == before);
    CHECK(v.click("Jump to latest \u2193") == std::vector<Message>{msg::JumpToLatest{}});
    settle();
    CHECK(s.chat.is_following());
    CHECK(bar->value() == bar->maximum());
}

TEST_CASE("transcript view: rows keep their widgets while the window slides") {
    auto s = long_session(300);
    V v(s, true);
    auto* bar = v.view.scroll_area()->verticalScrollBar();
    bar->setValue(0);
    settle();
    CHECK_FALSE(s.chat.is_following());
    const auto before = s.chat.window();
    std::map<NodeId, QWidget*> widgets;
    for (auto i = before.begin; i < before.end; ++i) widgets[s.chat.items()[i].key] = v.view.list()->row(s.chat.items()[i].key);
    bar->setValue(1'500);
    settle();
    const auto after = s.chat.window();
    CHECK(after.begin > before.begin);
    std::size_t kept = 0;
    for (auto i = after.begin; i < after.end; ++i) {
        const auto key = s.chat.items()[i].key;
        if (const auto it = widgets.find(key); it != widgets.end()) {
            CHECK(v.view.list()->row(key) == it->second);
            ++kept;
        }
    }
    CHECK(kept > 0);
}

TEST_CASE("transcript view: copy buttons copy the author's code and message") {
    const std::pair<std::string_view, std::string_view> cases[] = {
        {"Example:\n\n```text\n    first  \n\n\tsecond\n```\n", "    first  \n\n\tsecond\n"},
        {" ```text\n a\n\n b\n ```", "a\n\nb\n"},
        {"```sh\nprintf x |", "printf x |"},
    };
    for (const auto& [source, expected] : cases) {
        S s;
        const auto t = s.turn();
        s.msg(t, "assistant", source);
        V v(s);
        CHECK(v.click("Copy code block") == std::vector<Message>{msg::Copy{std::string(expected)}});
        CHECK(v.click("Copy message") == std::vector<Message>{msg::Copy{std::string(source)}});
    }
}

TEST_CASE("transcript view: exts show their harness and type, the command list shows nothing") {
    S s;
    const auto t = s.turn();
    s.add(t, K::Ext, {{"harness", "omp"}, {"type", "notice"}}, R"({"message":"notice text"})");
    s.add(t, K::Ext, {{"harness", "claude-code"}, {"type", "subagent_progress"}}, "{}");
    s.add(t, K::Ext, {{"harness", "omp"}, {"type", "subagent_progress"}}, "{}");
    s.add(t, K::Job, {{"kind", "subagent"}, {"status", "done"}, {"agent", "explore"}, {"title", "Survey"}});
    s.add(s.session, K::Ext, {{"harness", "omp"}, {"type", std::string(COMMANDS_EXT)}}, "{}");
    V v(s);
    CHECK(s.chat.items().size() == 3);
    CHECK(v.shows("Survey"));
    CHECK(v.shows("notice"));
    CHECK(v.shows("claude-code"));
    CHECK_FALSE(v.shows(COMMANDS_EXT));
}

TEST_CASE("transcript view: a new session's first reply renders frame after frame") {
    S s;
    V v(s, true);
    const auto t = s.turn();
    s.msg(t, "user", "Reply with pong");
    v.refresh();
    s.msg(t, "assistant", "po");
    v.refresh();
    s.call(t, "read", {1, 2}, "file");
    s.msg(t, "assistant", "pong");
    v.refresh();
    const auto t2 = s.turn();
    s.msg(t2, "user", "Now ping");
    s.msg(t2, "assistant", "ping");
    v.refresh();
    std::size_t users = 0;
    for (const auto& i : s.chat.items()) users += i.kind == ItemKind::User ? 1 : 0;
    CHECK(users == 2);
    CHECK(v.shows("ping"));
}
