// Port of `crates/loa-ui-session/src/tests/composer.rs` and the composer's workflow tests.

#include <algorithm>

#include "support.hpp"

using namespace composer_test;
using loa::view::session::Attachment;
using loa::view::session::SessionMode;

namespace {

KeyPress named(Key key, Modifiers m = {}) { return KeyPress{key, m, {}}; }
KeyPress chr(std::string text, Modifiers m = {}) { return KeyPress{Key::Character, m, std::move(text)}; }

// The first message a key press produces in the editor, as iced's simulator would report.
std::optional<Message> first(const State& state, const KeyPress& press) {
    const auto b = bind(press, state.key_context());
    if (!b || b->messages.empty()) return std::nullopt;
    return b->messages.front();
}

bool listed(const State& state, PickerId picker, std::string_view label) {
    for (const auto& row : state.menu_rows(picker))
        if (row.label == label || row.detail == std::optional<std::string>(label)) return true;
    for (const auto& a : state.menu_actions(picker))
        if (a == label) return true;
    return false;
}

bool on_chip(const State& state, std::string_view label) {
    for (const auto& c : state.chips())
        if (c.label == label) return true;
    for (const auto& c : state.bar_chips())
        if (c.label == label) return true;
    return state.workspace() && state.workspace()->path == label;
}

} // namespace

TEST_CASE("key bindings follow the editor state") {
    const Modifiers none{}, shift{.shift = true}, cmd{.control = true}, cmd_shift{.shift = true, .control = true};
    const State idle;
    CHECK(first(idle, named(Key::Enter, none)) == Message{msg::Submit{}});
    CHECK(first(idle, named(Key::Enter, shift)) == Message{msg::Edit{action::Edit{edit::Enter{}}}});
    CHECK(first(idle, chr("z", cmd)) == Message{msg::Undo{}});
    CHECK(first(idle, chr("z", cmd_shift)) == Message{msg::Redo{}});
    CHECK(first(idle, chr("y", cmd)) == Message{msg::Redo{}});
    CHECK(first(idle, chr("n", cmd)) == std::nullopt);
    CHECK(first(idle, named(Key::Escape, none)) == std::nullopt);
    // Ctrl+V pastes the clipboard, then asks for an image.
    CHECK(bind(chr("v", cmd), {}) == Binding{Binding::Clipboard::Paste, {msg::PasteImage{}}});

    State popup;
    popup.set_commands({Command{"review", ""}});
    type_text(popup, "/r");
    popup.turn_started();
    const std::pair<Key, Message> table[] = {
        {Key::ArrowDown, msg::PopupStep{1}}, {Key::ArrowUp, msg::PopupStep{-1}}, {Key::Tab, msg::PopupAccept{}},
        {Key::Enter, msg::PopupAccept{}},    {Key::Escape, msg::PopupClose{}},
    };
    for (const auto& [key, expected] : table) {
        const auto b = bind(named(key), popup.key_context());
        REQUIRE(b);
        CHECK(b->messages == std::vector<Message>{expected});
    }
    State busy;
    busy.turn_started();
    CHECK(bind(named(Key::Escape), busy.key_context())->messages == std::vector<Message>{msg::Interrupt{}});
    // An input method composing keeps Enter and Esc.
    State composing;
    act(composing, msg::Composing{true});
    CHECK_FALSE(bind(named(Key::Enter), composing.key_context()));
}

#if !defined(__APPLE__)
TEST_CASE("ctrl edit keys delete and move by word") {
    const KeyContext ctx{};
    const Modifiers ctrl{.control = true};
    const auto msgs = [&](Key k, Modifiers m, KeyContext c) { return bind(named(k, m), c)->messages; };
    CHECK(msgs(Key::Backspace, ctrl, ctx) ==
          std::vector<Message>{msg::Edit{action::Select{Motion::WordLeft}}, msg::Edit{action::Edit{edit::Backspace{}}}});
    CHECK(msgs(Key::Delete, ctrl, ctx) ==
          std::vector<Message>{msg::Edit{action::Select{Motion::WordRight}}, msg::Edit{action::Edit{edit::Delete{}}}});
    CHECK(msgs(Key::ArrowLeft, ctrl, ctx) == std::vector<Message>{msg::Edit{action::Move{Motion::WordLeft}}});
    CHECK(msgs(Key::ArrowRight, ctrl, ctx) == std::vector<Message>{msg::Edit{action::Move{Motion::WordRight}}});
    CHECK(msgs(Key::Home, ctrl, ctx) == std::vector<Message>{msg::Edit{action::Move{Motion::DocumentStart}}});
    CHECK(msgs(Key::End, ctrl, ctx) == std::vector<Message>{msg::Edit{action::Move{Motion::DocumentEnd}}});
    CHECK(msgs(Key::ArrowLeft, Modifiers{.shift = true, .control = true}, ctx) ==
          std::vector<Message>{msg::Edit{action::Select{Motion::WordLeft}}});
    // With a selection, the word delete takes just the selection.
    CHECK(msgs(Key::Backspace, ctrl, KeyContext{.selection = true}) == std::vector<Message>{msg::Edit{action::Edit{edit::Backspace{}}}});
}
#endif

TEST_CASE("submit, queue and stop") {
    State state;
    state.set_author("node-a");
    CHECK_MESSAGE(act(state, msg::Submit{}) == std::nullopt, "blank");
    type_text(state, "  fix the bug\nplease\n");
    const auto prompt = submitted(act(state, msg::Submit{}));
    CHECK(prompt.text == "fix the bug\nplease");
    CHECK(prompt.by == "node-a");
    CHECK(state.text().empty());
    state.turn_started();
    type_text(state, "second");
    CHECK(act(state, msg::Submit{}) == Intent{intent::QueueChanged{{"second"}}});
    type_text(state, "third");
    act(state, msg::Submit{});
    const auto first_id = state.queue().front().id;
    CHECK(act(state, msg::Queue{first_id, " "}) == Intent{intent::QueueChanged{{" ", "third"}}});
    CHECK_MESSAGE(submitted(state.turn_ended()).text == "third", "blank queued messages are skipped");
    CHECK(state.turn_ended() == std::nullopt);
    // Stop moves queued messages back into the editor, ahead of what is typed.
    state.turn_started();
    type_text(state, "queued");
    act(state, msg::Submit{});
    type_text(state, "draft");
    CHECK(act(state, msg::Interrupt{}) == Intent{intent::Stop{}});
    CHECK(state.text() == "queued\n\ndraft");
    CHECK(state.queue().empty());
}

TEST_CASE("queued messages dispatch in order and can be removed") {
    State state;
    type_text(state, "first");
    submitted(act(state, msg::Submit{}));
    state.turn_started();
    for (const auto* text : {"a", "b", "c"}) {
        type_text(state, text);
        act(state, msg::Submit{});
    }
    REQUIRE(state.queue().size() == 3);
    CHECK(act(state, msg::Queue{state.queue()[1].id, std::nullopt}) == Intent{intent::QueueChanged{{"a", "c"}}});
    CHECK(submitted(state.turn_ended()).text == "a");
    state.turn_started();
    CHECK(submitted(state.turn_ended()).text == "c");
    CHECK(state.turn_ended() == std::nullopt);
}

TEST_CASE("undo groups words and deletions") {
    State state;
    type_text(state, "hello world");
    act(state, msg::Undo{});
    CHECK(state.text() == "hello ");
    act(state, msg::Redo{});
    CHECK(state.text() == "hello world");
    for (int i = 0; i < 3; ++i) act(state, msg::Edit{action::Edit{edit::Backspace{}}});
    act(state, msg::Undo{});
    CHECK_MESSAGE(state.text() == "hello world", "a run of deletions undoes at once");
    act(state, msg::Edit{action::Move{Motion::Home}});
    type_text(state, "x");
    act(state, msg::Undo{});
    CHECK_MESSAGE(state.text() == "hello world", "moving the cursor ends a group");
    act(state, msg::Undo{});
    CHECK(state.text() == "hello ");
}

TEST_CASE("popups complete commands and attach mentioned files") {
    State state;
    const auto command = [](std::string name) { return Command{name, name + " the session"}; };
    state.set_commands({command("review"), command("rewind"), command("compact")});
    type_text(state, "/re");
    CHECK(state.popup_kind() == PopupKind::Command);
    CHECK(state.popup_items() == std::pair{std::vector<std::string>{"review", "rewind"}, std::size_t{0}});
    act(state, msg::PopupStep{1});
    act(state, msg::PopupAccept{});
    CHECK(state.text() == "/rewind ");
    type_text(state, "see @ma");
    CHECK_MESSAGE(state.popup_kind() == std::nullopt, "no files yet");
    state.set_files({"src/main.rs", "docs/a b.png"});
    CHECK(state.popup_items() == std::pair{std::vector<std::string>{"src/main.rs"}, std::size_t{0}});
    act(state, msg::PopupClose{});
    type_text(state, "i");
    CHECK_MESSAGE(state.popup_kind() == std::nullopt, "Esc keeps the word's popup closed");
    type_text(state, " @do");
    CHECK_MESSAGE(type_text(state, "c").empty(), "the open popup asks for files once");
    act(state, msg::PopupAccept{0});
    type_text(state, "and @src/main.rs, @nope");
    const auto prompt = submitted(act(state, msg::Submit{}));
    CHECK(prompt.text == "/rewind see @mai @\"docs/a b.png\" and @src/main.rs, @nope");
    const std::vector<Attachment> expected{{"docs/a b.png", "image/png"}, {"src/main.rs", std::nullopt}};
    CHECK(prompt.attachments == expected);
    State fresh;
    CHECK(act(fresh, msg::MentionPressed{}) == Intent{intent::FilesRequested{""}});
}

TEST_CASE("pickers emit changes once and skip unavailable options") {
    State state;
    const auto harness = [](std::string id, bool available) { return HarnessOption{id, id, available, std::nullopt}; };
    state.set_harnesses({harness("omp", true), harness("gone", false), harness("claude", true)}, "omp");
    const auto model = [](std::string id) { return ModelOption{id, "Claude " + id, "anthropic", std::nullopt}; };
    state.set_models({model("opus"), model("sonnet")}, "opus");
    state.set_model_actions({PickerAction{"all", "All models\u2026"}});
    state.set_efforts({"low", "high"}, "low");
    state.set_access({AccessOption{"ask", "Ask", "asks first"}, AccessOption{"full", "Full", "never asks"}}, "ask");
    const auto ws = [](std::string p) { return Workspace{p, "/home/" + p, std::nullopt}; };
    state.set_workspace(ws("a"));
    state.set_folders({ws("a"), ws("b")});
    const std::tuple<PickerId, std::string, std::optional<Intent>> cases[] = {
        {PickerId::Harness, "gone", std::nullopt},
        {PickerId::Harness, "omp", std::nullopt},
        {PickerId::Harness, "claude", intent::HarnessChanged{"claude"}},
        {PickerId::Model, "sonnet", intent::ModelChanged{model("sonnet")}},
        {PickerId::Model, "all", intent::ModelAction{"all"}},
        {PickerId::Effort, "high", intent::EffortChanged{"high"}},
        {PickerId::Access, "full", intent::AccessChanged{"full"}},
        {PickerId::Mode, "plan", intent::ModeChanged{SessionMode::Plan}},
        {PickerId::Folder, "/home/b", intent::FolderPicked{"/home/b"}},
    };
    for (const auto& [picker, key, expected] : cases) {
        act(state, msg::Toggle{picker});
        const auto message = state.pick_key(picker, key);
        REQUIRE_MESSAGE(message, key, " listed");
        CHECK_MESSAGE(act(state, *message) == expected, key);
    }
    CHECK(state.harness()->id == "claude");
    CHECK(state.efforts().second == "high");
    CHECK(state.models().second->id == "sonnet");
    CHECK(act(state, msg::Pick{PickerId::Folder, 2}) == Intent{intent::BrowseFolders{}});
    CHECK(act(state, msg::Pick{PickerId::Folder, 3}) == Intent{intent::OpenWorkspace{}});
    act(state, msg::Toggle{PickerId::Model});
    CHECK(state.open_picker() == PickerId::Model);
    for (const auto* label : {"Claude opus", "anthropic", "All models\u2026"}) CHECK_MESSAGE(listed(state, PickerId::Model, label), label);
    for (const auto* label : {"sonnet", "Plan", "High", "Full", "a"}) CHECK_MESSAGE(on_chip(state, label), label);
}

TEST_CASE("picking and managing workflows") {
    const std::vector<WorkflowOption> options{
        {"DEEP", "Deep", loa::view::session::composer::WorkflowKind::HandWritten, true},
        {"REVIEW", "Review", loa::view::session::composer::WorkflowKind::Managed, false},
    };
    State state;
    state.set_workflows(options, true);
    CHECK(state.workflow()->role == "DEEP");
    CHECK(act(state, msg::Pick{PickerId::Workflow, 0}) == std::nullopt);
    CHECK(act(state, msg::Pick{PickerId::Workflow, 1}) == Intent{intent::WorkflowPicked{"REVIEW"}});
    CHECK(act(state, msg::Pick{PickerId::Workflow, 2}) == Intent{intent::OpenRules{}});
    CHECK(act(state, msg::OpenRules{}) == Intent{intent::OpenRules{}});

    State off;
    off.set_workflows(options, false);
    act(off, msg::Toggle{PickerId::Workflow});
    CHECK(off.open_picker() == std::nullopt);
    CHECK(act(off, msg::Pick{PickerId::Workflow, 1}) == std::nullopt);
    CHECK(act(off, msg::OpenRules{}) == std::nullopt);
    CHECK(on_chip(off, "Deep"));
}

TEST_CASE("pasted images become numbered tokens and are sent while their token stays") {
    State state;
    type_text(state, "compare");
    (void)state.attach_image(pasted("/tmp/one.png", std::pair{1414u, 281u}));
    (void)state.attach_image(pasted("/tmp/two.png", std::pair{578u, 200u}));
    CHECK(state.text() == "compare [Image #1] [Image #2] ");
    CHECK_MESSAGE(shown_images(state) == 2, "both cards show");

    // The × on a card takes its token out of the text too; undo brings both back.
    act(state, msg::RemoveImage{1});
    CHECK(state.text() == "compare [Image #2] ");
    CHECK(shown_images(state) == 1);
    act(state, msg::Undo{});
    CHECK(state.text() == "compare [Image #1] [Image #2] ");
    CHECK_MESSAGE(shown_images(state) == 2, "the card is back");
    act(state, msg::RemoveImage{1});
    const auto prompt = submitted(act(state, msg::Submit{}));
    CHECK(prompt.text == "compare [Image #2]");
    CHECK(prompt.attachments == std::vector<Attachment>{{"/tmp/two.png", "image/png"}});

    // The next message numbers its images from 1 again.
    (void)state.attach_image(pasted("/tmp/three.png", std::nullopt));
    CHECK(state.text() == "[Image #1] ");
}

TEST_CASE("a cursor just before a mention opens no popup") {
    State state;
    type_text(state, "see @");
    act(state, msg::Edit{action::Move{Motion::Left}});
    CHECK(state.popup_kind() == std::nullopt);
}

TEST_CASE("leaving a running session brings its queue back and a failed send returns the text") {
    State state;
    type_text(state, "first");
    submitted(act(state, msg::Submit{}));
    state.turn_started();
    type_text(state, "queued");
    const auto queued = act(state, msg::Submit{});
    CHECK((queued && std::holds_alternative<intent::QueueChanged>(*queued)));

    // Another, idle session is shown: nothing stays queued for the old one.
    state.follow_session(false);
    CHECK_FALSE(state.is_busy());
    CHECK(state.text() == "queued");
    // Showing a running session makes the composer queue again.
    state.follow_session(true);
    CHECK(state.is_busy());
    state.follow_session(false);

    // A send that could not start a session gives its text back.
    const auto prompt = submitted(act(state, msg::Submit{}));
    CHECK_MESSAGE(state.is_busy(), "starting");
    state.follow_session(false);
    CHECK_MESSAGE(state.is_busy(), "a send on its way is left alone");
    (void)state.give_back(prompt.text);
    CHECK_FALSE(state.is_busy());
    CHECK(state.text() == "queued");
}

namespace {
std::optional<long long> tick(const State& state) {
    const auto t = state.tick();
    return t ? std::optional(static_cast<long long>(t->count())) : std::nullopt;
}
} // namespace

TEST_CASE("a magic keyword starts the shimmer and removing it stops it") {
    State state;
    const WindowId window = 7;
    state.set_window(window);
    CHECK(tick(state) == std::nullopt);
    type_text(state, "please workflowz");
    CHECK(state.keyword());
    CHECK_MESSAGE(tick(state) == 32, "every step of the 1.8 s cycle");
    act(state, msg::Shimmer{900});
    CHECK_MESSAGE(state.shimmer() == 28, "half way through the cycle");
    act(state, msg::WindowFocused{window, false});
    CHECK_MESSAGE(tick(state) == std::nullopt, "an unfocused window stops the shimmer");
    act(state, msg::WindowFocused{window, true});
    for (std::size_t i = 0; i < std::string_view("workflowz").size(); ++i) act(state, msg::Edit{action::Edit{edit::Backspace{}}});
    CHECK_FALSE(state.keyword());
    CHECK(tick(state) == std::nullopt);
}

TEST_CASE("the working indicator shows while a turn runs and ticks only then") {
    State state;
    const WindowId window = 1, detached = 2;
    state.set_window(window);
    const auto reading = Activity::working("Reading the config\u2026", 1000);
    state.set_activity(reading);
    CHECK_MESSAGE(state.indicator() == std::nullopt, "an idle composer shows none");
    CHECK(tick(state) == std::nullopt);

    type_text(state, "hi");
    submitted(act(state, msg::Submit{}));
    state.set_activity(Activity::idle());
    CHECK(state.indicator() == "Starting session\u2026");
    CHECK_MESSAGE(state.trailer() == std::nullopt, "nothing runs to interrupt yet");
    state.turn_started();
    CHECK_MESSAGE(state.indicator() == "Working\u2026", "the turn runs before the session's DOM has it");
    CHECK(state.trailer() == "esc to interrupt");
    state.set_activity(reading);
    CHECK(state.indicator() == "Reading the config\u2026");
    CHECK_MESSAGE(tick(state) == 90, "slower than a keyword's shimmer");
    act(state, msg::Shimmer{66'000});
    CHECK_MESSAGE(state.trailer() == "1m 05s \u00b7 esc to interrupt", "the elapsed time counts from the turn's start");

    act(state, msg::WindowFocused{detached, false});
    CHECK_MESSAGE(tick(state) == 90, "another window's focus changes nothing");
    act(state, msg::WindowFocused{window, false});
    CHECK_MESSAGE(tick(state) == 1000, "unfocused, only the elapsed time ticks");
    const auto phase = state.shimmer();
    act(state, msg::Shimmer{67'300});
    CHECK_MESSAGE(state.shimmer() == phase, "the animation rests");
    CHECK(state.trailer() == "1m 06s \u00b7 esc to interrupt");
    act(state, msg::WindowFocused{window, true});
    state.set_activity(Activity::waiting());
    CHECK_MESSAGE(state.indicator() == std::nullopt, "a decision card asks the user instead");
    CHECK(tick(state) == std::nullopt);
    state.set_activity(reading);
    (void)state.turn_ended();
    CHECK(state.indicator() == std::nullopt);
    CHECK(tick(state) == std::nullopt);
}

TEST_CASE("the project chip picks a project or none") {
    State state;
    state.set_projects({"alpha", "beta"}, std::nullopt);
    act(state, msg::Toggle{PickerId::Project});
    CHECK(state.open_picker() == PickerId::Project);
    const auto beta = state.pick_key(PickerId::Project, "beta");
    REQUIRE(beta);
    CHECK(act(state, *beta) == Intent{intent::ProjectPicked{"beta"}});
    CHECK_MESSAGE(state.open_picker() == std::nullopt, "picking closes the menu");
    CHECK_MESSAGE(act(state, *beta) == std::nullopt, "the same project again changes nothing");
    CHECK(act(state, msg::Pick{PickerId::Project, 0}) == Intent{intent::ProjectPicked{std::nullopt}});
}

TEST_CASE("the effort slider applies its level when let go") {
    State state;
    state.set_efforts({"low", "medium", "high"}, "low");
    act(state, msg::Toggle{PickerId::Effort});
    CHECK_MESSAGE(act(state, msg::EffortSlid{2}) == std::nullopt, "dragging only previews");
    CHECK(state.effort_title() == "High");
    CHECK(act(state, msg::EffortReleased{}) == Intent{intent::EffortChanged{"high"}});
    CHECK_MESSAGE(state.open_picker() == PickerId::Effort, "the card stays open");
    act(state, msg::EffortSlid{2});
    CHECK_MESSAGE(act(state, msg::EffortReleased{}) == std::nullopt, "the same level again changes nothing");
}

TEST_CASE("a taken draft empties the composer and comes back whole") {
    State state;
    type_text(state, "see");
    (void)state.mention_path("src/a.rs");
    (void)state.attach_image(pasted("one.png", std::nullopt));
    act(state, msg::Edit{action::Move{Motion::Left}});
    const auto cursor = state.cursor();

    auto draft = state.take_draft();
    CHECK_FALSE(draft.is_empty());
    CHECK(state.text().empty());
    CHECK(state.images().empty());
    act(state, msg::Undo{});
    CHECK_MESSAGE(state.text().empty(), "undo can't bring the taken text back");
    CHECK(state.take_draft().is_empty());

    type_text(state, "other");
    state.restore_draft(std::move(draft));
    CHECK(state.text() == "see @src/a.rs [Image #1] ");
    CHECK(state.cursor() == cursor);
    CHECK_MESSAGE(shown_images(state) == 1, "the card is back");
    act(state, msg::Undo{});
    CHECK_MESSAGE(state.text() == "see @src/a.rs [Image #1] ", "undo can't reach the text typed before the restore");

    act(state, msg::Edit{action::Move{Motion::DocumentEnd}});
    (void)state.attach_image(pasted("two.png", std::nullopt));
    const auto prompt = submitted(act(state, msg::Submit{}));
    CHECK_MESSAGE(prompt.text == "see @src/a.rs [Image #1] [Image #2]", "the image counter is back too");
    CHECK_MESSAGE((attached(prompt) == std::vector<std::string>{"src/a.rs", "one.png", "two.png"}), "the mention and both images are sent");
}

TEST_CASE("a draft taken while messages wait leaves them their images and mentions") {
    State state;
    type_text(state, "first");
    submitted(act(state, msg::Submit{}));
    (void)state.mention_path("src/a.rs");
    (void)state.attach_image(pasted("one.png", std::nullopt));
    const auto queued_event = act(state, msg::Submit{});
    CHECK_MESSAGE((queued_event && std::holds_alternative<intent::QueueChanged>(*queued_event)), "queued while the send starts");
    type_text(state, "draft");

    auto draft = state.take_draft();
    State other;
    (void)other.attach_image(pasted("two.png", std::nullopt));
    state.restore_draft(other.take_draft());
    CHECK_MESSAGE(state.text() == "[Image #2] ", "a restored image is numbered on from the queued one");
    state.turn_started();
    const auto queued = submitted(state.turn_ended());
    CHECK(queued.text == "@src/a.rs [Image #1]");
    CHECK_MESSAGE((attached(queued) == std::vector<std::string>{"src/a.rs", "one.png"}), "the queued message is sent with its mention and image");
    CHECK(state.turn_ended() == std::nullopt);
    CHECK(submitted(act(state, msg::Submit{})).attachments == std::vector<Attachment>{{"two.png", "image/png"}});
    state.restore_draft(std::move(draft));
    CHECK(state.text() == "draft");
}

namespace {
// A composer holding `text` with an image `path` pasted after it, and a mention of `mention`.
State drafted(std::string_view text, std::string path, std::string_view mention) {
    State state;
    type_text(state, text);
    (void)state.attach_image(pasted(std::move(path), std::nullopt));
    (void)state.mention_path(mention);
    return state;
}
} // namespace

TEST_CASE("a draft put ahead of another keeps both texts and images apart") {
    auto editor = drafted("now", "b.png", "b.rs");
    const auto cursor = editor.cursor();
    editor.prepend_draft(drafted("before", "a.png", "a.rs").take_draft());
    CHECK(editor.text() == "before [Image #2] @a.rs \n\nnow [Image #1] @b.rs ");
    CHECK_MESSAGE(editor.cursor().position.line == cursor.position.line + 2, "the cursor stays in the editor's own text");
    act(editor, msg::Undo{});
    CHECK_MESSAGE(editor.text() == "before [Image #2] @a.rs \n\nnow [Image #1] @b.rs ", "undo starts from the merged text");
    const auto prompt = submitted(act(editor, msg::Submit{}));
    CHECK_MESSAGE((attached(prompt) == std::vector<std::string>{"a.rs", "b.rs", "b.png", "a.png"}), "every mention and image is sent once");

    auto stored = drafted("stored", "d.png", "d.rs").take_draft();
    stored.prepend(drafted("carried", "c.png", "c.rs").take_draft());
    State shown;
    shown.restore_draft(std::move(stored));
    CHECK(shown.text() == "carried [Image #2] @c.rs \n\nstored [Image #1] @d.rs ");
    (void)shown.attach_image(pasted("e.png", std::nullopt));
    CHECK_MESSAGE(shown.text().find("[Image #3]") != std::string::npos, "the counter counts both drafts' images");
    CHECK(attached(submitted(act(shown, msg::Submit{}))) == std::vector<std::string>{"c.rs", "d.rs", "d.png", "c.png", "e.png"});
}
