// Port of the tests in `crates/loa-ui-settings/src/projects/{mod,create}.rs`. Clicks the iced
// simulator made go through the Qt views' `message` signal; editor actions become the editor's
// whole text after the edit.

#include <doctest/doctest.h>

#include <QApplication>
#include <QLineEdit>

#include "loa/ui/kit/testkit.hpp"
#include "loa/ui/settings/projects/view.hpp"

// Tests live in a named namespace so the `local` helper hides the screen's `local` message namespace.
namespace projects_tests {

using namespace loa::ui::settings::projects;
namespace in = loa::view::settings::projects::intent;
namespace oh = loa::view::settings::projects::omp_history;
using loa::view::settings::projects::Digest;
using loa::view::settings::projects::OmpHistoryRow;

void app() {
    if (QApplication::instance()) return;
    static int argc = 1;
    static char name[] = "loa_ui_settings_tests";
    static char* argv[] = {name, nullptr};
    static QApplication a(argc, argv);
}

ProjectOverview overview() {
    auto row = [](std::string id, std::optional<std::string> title, std::size_t turns, bool excluded, DigestState state,
                  std::optional<std::string> digest, std::optional<std::string> model) {
        SessionRow r;
        r.id = std::move(id);
        r.title = std::move(title);
        r.harness = "omp";
        r.turns = turns;
        r.excluded = excluded;
        r.digest_state = state;
        if (digest) r.digest = Digest{*digest, {}};
        r.digest_model = std::move(model);
        return r;
    };
    ProjectOverview o;
    o.cwd = "/home/u/loa";
    o.settings.instructions = "Use rustls.";
    o.ideas = std::string();
    o.sessions = {row("a", "Set up iroh", 3, false, DigestState::Fresh, "Picked port 31415.", "smol"),
                  row("b", std::nullopt, 1, true, DigestState::Missing, std::nullopt, std::nullopt)};
    return o;
}

State loaded() {
    State s("loa");
    s.set_overview(overview(), 3 * 3'600'000);
    return s;
}

std::optional<Intent> local(State& s, Local m) { return s.update(Message{std::move(m)}); }

// Renders `s`, clicks `label` and feeds the messages the view sent back to `s`.
std::vector<Intent> click(State& s, std::string_view label) {
    app();
    ProjectView view;
    view.render(s);
    std::vector<Message> sent;
    QObject::connect(&view, &ProjectView::message, [&](const Message& m) { sent.push_back(m); });
    CHECK_MESSAGE(loa::ui::kit::testkit::click_label(view, label), "no ", label);
    std::vector<Intent> out;
    for (auto& m : sent)
        if (auto e = s.update(std::move(m))) out.push_back(std::move(*e));
    return out;
}

OmpHistory two_rows() {
    return oh::Ready{{OmpHistoryRow{"native-new", "Recent terminal conversation", 2'000},
                      OmpHistoryRow{"native-old", "Older terminal conversation", 1'000}}};
}


TEST_CASE("projects: terminal history rows open only while visible and not importing") {
    auto s = loaded();
    CHECK(s.omp_history() == nullptr);
    s.set_omp_history(two_rows());
    REQUIRE(s.omp_history() != nullptr);
    CHECK(*s.omp_history() == two_rows());
    CHECK(click(s, "Recent terminal conversation") == std::vector<Intent>{in::OpenOmpHistory{"native-new"}});
    CHECK(click(s, "Older terminal conversation") == std::vector<Intent>{in::OpenOmpHistory{"native-old"}});
    const Message open{Intent{in::OpenOmpHistory{"native-new"}}};
    CHECK_FALSE(s.update(Message{Intent{in::OpenOmpHistory{"missing"}}}).has_value());
    s.set_omp_history_busy(true);
    CHECK_FALSE(s.update(open).has_value());
    s.set_omp_history_busy(false);
    s.set_omp_history(oh::Loading{});
    CHECK_FALSE(s.update(open).has_value());
    s.set_omp_history(oh::Failed{"Could not read history"});
    CHECK_FALSE(s.update(open).has_value());
    s.set_omp_history(oh::Ready{});
    CHECK_FALSE(s.update(open).has_value());
    s.set_omp_history(std::nullopt);
    CHECK_FALSE(s.update(open).has_value());
}

TEST_CASE("projects: edits and toggles reduce to events") {
    namespace l = local;
    SUBCASE("save without edits does nothing") {
        auto s = loaded();
        CHECK_FALSE(local(s, l::Save{}).has_value());
        CHECK_FALSE(s.is_dirty());
    }
    SUBCASE("pins and the digest model save once") {
        auto s = loaded();
        std::vector<Intent> events;
        for (Local m : std::vector<Local>{l::PinPath{" docs/ARCH.md "}, l::PinNote{"the contract"}, l::AddPin{}, l::PinPath{"docs/ARCH.md"},
                                          l::AddPin{}, l::DigestModel{"anthropic/claude-haiku"}, l::Save{}, l::Save{}})
            if (auto e = local(s, std::move(m))) events.push_back(std::move(*e));
        ProjectSettings settings;
        settings.instructions = "Use rustls.";
        settings.pinned = {PinnedFile{"docs/ARCH.md", "the contract"}};
        settings.digest_model = "anthropic/claude-haiku";
        CHECK(events == std::vector<Intent>{in::SaveSettings{"loa", settings, std::nullopt}});
        CHECK(s.saving());
        CHECK(s.is_dirty());
    }
    SUBCASE("revert drops edits") {
        auto s = loaded();
        for (Local m : std::vector<Local>{l::PinPath{"src/main.rs"}, l::AddPin{}, l::RemovePin{5}, l::ShareIdeas{true}, l::Revert{}})
            CHECK_FALSE(local(s, std::move(m)).has_value());
        CHECK(s.draft().pinned.empty());
        CHECK_FALSE(s.ideas_in_context());
        CHECK_FALSE(s.is_dirty());
    }
    SUBCASE("context toggles report only changes") {
        auto s = loaded();
        std::vector<Intent> events;
        for (Local m : std::vector<Local>{l::SetIncluded{"a", false}, l::SetIncluded{"a", false}, l::SetIncluded{"b", true}, l::SetIncluded{"zz", true}})
            if (auto e = local(s, std::move(m))) events.push_back(std::move(*e));
        CHECK(events == std::vector<Intent>{in::SetExcluded{"a", true}, in::SetExcluded{"b", false}});
        for (const auto& r : s.overview()->sessions) CHECK(r.excluded == (r.id == "a"));
    }
    SUBCASE("emitted intents hold only for this project") {
        auto s = loaded();
        CHECK(s.update(Message{Intent{in::OpenSession{"a"}}}) == std::optional<Intent>(in::OpenSession{"a"}));
        CHECK_FALSE(s.update(Message{Intent{in::OpenSession{"zz"}}}).has_value());
        CHECK_FALSE(s.update(Message{Intent{in::NewChat{"other", std::nullopt}}}).has_value());
    }
}

TEST_CASE("projects: ideas keep their line endings and are sent only when edited") {
    const std::string notes = "# Ideas\r\n\r\n- try iroh relays\r\n";
    State s("loa");
    auto o = overview();
    o.ideas = notes;
    s.set_overview(o, 0);
    CHECK_FALSE(s.is_dirty());

    CHECK(click(s, "Use ideas as agent context").empty());
    CHECK(s.is_dirty());
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    auto save = std::get<in::SaveSettings>(*saved);
    CHECK_FALSE(save.ideas.has_value());
    CHECK(save.settings.ideas_in_context);
    s.saved(std::pair{save.settings, std::optional<std::string>()});
    CHECK_FALSE(s.is_dirty());

    const auto edited = notes + "- add a sidebar";
    local(s, local::Ideas{edited});
    saved = local(s, local::Save{});
    REQUIRE(saved);
    save = std::get<in::SaveSettings>(*saved);
    CHECK(save.ideas == std::optional(edited));
    s.saved(std::pair{save.settings, save.ideas});
    CHECK_FALSE(s.is_dirty());
    CHECK(*s.overview()->ideas == edited);
}

TEST_CASE("projects: notes changed outside keep their edit until reloaded") {
    auto s = loaded();
    local(s, local::Ideas{"typed in Loa"});
    auto o = overview();
    o.ideas = std::string("edited outside");
    s.set_overview(o, 0);
    CHECK(s.ideas() == "typed in Loa");
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    auto save = std::get<in::SaveSettings>(*saved);
    CHECK(save.ideas.has_value());
    s.ideas_changed_outside(save.settings);
    CHECK_FALSE(s.saving());
    CHECK(s.ideas_edited());
    CHECK(s.ideas() == "typed in Loa");
    CHECK(s.draft() == s.overview()->settings);

    CHECK(click(s, "Reload") == std::vector<Intent>{in::ReloadIdeas{}});
    CHECK_FALSE(s.ideas_changed());
    CHECK(s.ideas_reloading());
    local(s, local::Ideas{"typed while reloading typed in Loa"});
    CHECK(s.ideas() == "typed in Loa");
    {
        // Save is on the page but waits for the reload.
        ProjectView view;
        view.render(s);
        CHECK_FALSE(loa::ui::kit::testkit::click_label(view, "Save"));
    }
    CHECK_FALSE(local(s, local::Save{}).has_value());
    CHECK_FALSE(local(s, local::ReloadIdeas{}).has_value());

    s.set_overview(o, 0);
    s.revert_ideas();
    CHECK(s.ideas() == "edited outside");
    CHECK_FALSE(s.is_dirty());
    CHECK_FALSE(s.ideas_reloading());
    local(s, local::Ideas{"typed while reloading edited outside"});
    CHECK(s.ideas() == "typed while reloading edited outside");
}

TEST_CASE("projects: unreadable notes disable only the notes") {
    State s("loa");
    auto o = overview();
    o.ideas = std::unexpected(std::string("loa.ideas.md: invalid utf-8 sequence"));
    s.set_overview(o, 0);
    local(s, local::Ideas{"typed anyway"});
    CHECK_FALSE(s.is_dirty());
    local(s, local::ShareIdeas{true});
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    const auto& save = std::get<in::SaveSettings>(*saved);
    CHECK_FALSE(save.ideas.has_value());
    CHECK(save.settings.ideas_in_context);
}

TEST_CASE("projects: refreshes keep unsaved edits and saves settle them") {
    auto s = loaded();
    local(s, local::PinPath{"src/net.rs"});
    local(s, local::AddPin{});
    auto o = overview();
    o.ideas = std::string("edited outside");
    s.set_overview(o, 0);
    CHECK(s.draft().pinned.size() == 1);
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    const auto save = std::get<in::SaveSettings>(*saved);
    CHECK_FALSE(save.ideas.has_value());
    s.saved(std::pair{save.settings, std::optional<std::string>()});
    CHECK_FALSE(s.is_dirty());
    CHECK_FALSE(s.saving());
}

TEST_CASE("projects: rows open sessions and new chat starts in the project") {
    auto s = loaded();
    auto events = click(s, "omp  \u00b7  3 turns  \u00b7  3h ago");
    const auto chat = click(s, "New chat");
    events.insert(events.end(), chat.begin(), chat.end());
    CHECK(events == std::vector<Intent>{in::OpenSession{"a"}, in::NewChat{"loa", std::string("/home/u/loa")}});
}

TEST_CASE("projects: the folder changes from the header and deleting asks first") {
    auto s = loaded();
    auto events = click(s, "Change folder\u2026");
    CHECK_FALSE(local(s, local::Delete{}).has_value());
    CHECK(s.confirm_delete());
    CHECK(click(s, "Cancel").empty());
    CHECK_FALSE(s.confirm_delete());
    s.ask_delete();
    const auto deleted = click(s, "Delete");
    events.insert(events.end(), deleted.begin(), deleted.end());
    CHECK(events == std::vector<Intent>{in::ChangeFolder{"loa", std::string("/home/u/loa")}, in::Delete{"loa"}});
    State empty("ghost");
    ProjectOverview none;
    none.ideas = std::string();
    empty.set_overview(none, 0);
    CHECK(click(empty, "Set folder\u2026") == std::vector<Intent>{in::ChangeFolder{"ghost", std::nullopt}});
}

TEST_CASE("projects: deleting a terminal conversation asks first and never opens it") {
    auto s = loaded();
    s.set_omp_history(two_rows());
    CHECK(click(s, "Delete").empty());
    CHECK(s.confirm_delete_history() == std::optional<std::string>("native-new"));
    CHECK(click(s, "Cancel").empty());
    CHECK_FALSE(s.confirm_delete_history().has_value());
    click(s, "Delete");
    CHECK(click(s, "Delete") == std::vector<Intent>{in::DeleteOmpHistory{"native-new"}});
    CHECK_FALSE(s.confirm_delete_history().has_value());
    CHECK_FALSE(s.update(Message{Intent{in::DeleteOmpHistory{"native-old"}}}).has_value());

    local(s, local::DeleteOmpHistory{"native-old"});
    s.set_omp_history_busy(true);
    CHECK_FALSE(local(s, local::ConfirmDeleteOmpHistory{}).has_value());
    s.set_omp_history_busy(false);
    local(s, local::DeleteOmpHistory{"native-old"});
    s.set_omp_history(oh::Loading{});
    CHECK_FALSE(local(s, local::ConfirmDeleteOmpHistory{}).has_value());

    const OmpHistoryRow old{"native-old", "Older terminal conversation", 1'000};
    s.set_omp_history(oh::Ready{{old}});
    local(s, local::DeleteOmpHistory{"native-old"});
    s.set_omp_history(oh::Loading{});
    s.set_omp_history(oh::Ready{{old}});
    CHECK(s.confirm_delete_history() == std::optional<std::string>("native-old"));
    s.set_omp_history(oh::Ready{});
    CHECK_FALSE(s.confirm_delete_history().has_value());
    local(s, local::DeleteOmpHistory{"native-old"});
    s.set_omp_history(std::nullopt);
    CHECK_FALSE(s.confirm_delete_history().has_value());
}

TEST_CASE("projects create: needs a name and a folder and one try at a time") {
    namespace c = create;
    namespace ci = loa::view::settings::projects::create_intent;
    c::State s("");
    auto send = [&](c::Local m) { return s.update(c::Message{std::move(m)}); };
    CHECK_FALSE(send(c::local::Create{}).has_value());
    send(c::local::Name{"  Launch  "});
    CHECK_FALSE(send(c::local::Create{}).has_value());
    CHECK(send(c::local::ChooseFolder{}) == std::optional<CreateIntent>(ci::ChooseFolder{""}));

    s.set_folder("/work/launch");
    send(c::local::Instructions{"Use rustls.\n"});
    auto created = send(c::local::Create{});
    REQUIRE(created);
    REQUIRE(std::holds_alternative<ci::Create>(*created));
    const auto& create = std::get<ci::Create>(*created);
    CHECK(create.name == "Launch");
    CHECK(create.settings.folder == std::optional<std::string>("/work/launch"));
    CHECK(create.settings.instructions == "Use rustls.");
    CHECK_FALSE(send(c::local::Create{}).has_value());

    s.failed("a project named Launch already exists");
    CHECK(s.error() == std::optional<std::string_view>("a project named Launch already exists"));
    CHECK(send(c::local::Create{}).has_value());
    CHECK(s.update(c::Message{CreateIntent{ci::Cancel{}}}) == std::optional<CreateIntent>(ci::Cancel{}));

    app();
    c::State fresh("/work");
    CreateView view;
    view.render(fresh);
    view.show();
    auto* name = view.findChild<QLineEdit*>(QString::fromUtf8(c::NAME_ID.data(), static_cast<qsizetype>(c::NAME_ID.size())));
    REQUIRE(name != nullptr);
    CHECK(c::NAME_ID == "loa-new-project-name");
}

} // namespace projects_tests
