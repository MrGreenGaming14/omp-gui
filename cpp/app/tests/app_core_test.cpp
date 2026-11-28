// The app core: startup order and on-disk layout, the area registry's order, `settle()`'s
// request routing and the shell routes, over fake areas and offline ports.

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <sstream>

#include <doctest/doctest.h>

#include "loa/app/app.hpp"
#include "loa/app/area.hpp"
#include "loa/app/registry.hpp"
#include "loa/app/routes.hpp"
#include "loa/app/services.hpp"
#include "loa/app/windows.hpp"
#include "loa/app/workspace.hpp"
#include "loa/harness/account_support/opener.hpp"
#include "loa/models/testing.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::app;
namespace fs = std::filesystem;

namespace {

std::string read(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream text;
    text << in.rdbuf();
    return text.str();
}

// The log as one line, so a failed check prints it.
std::string joined(const std::vector<std::string>& log) {
    std::string out;
    for (const auto& each : log) out += (out.empty() ? "" : " ") + each;
    return out;
}

void write(const fs::path& path, std::string_view text) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << text;
}

loa::trust::Result<std::pair<loa::trust::NodeIdentity, loa::trust::KeySource>> memory_key(const fs::path&) {
    std::array<std::uint8_t, 32> seed{};
    seed.fill(1);
    return std::pair{loa::trust::NodeIdentity::from_secret_bytes(seed),
                     loa::trust::KeySource{loa::trust::key_source::Memory{"test"}}};
}

// What the fake areas saw, in order.
using Log = std::shared_ptr<std::vector<std::string>>;

// Asks the app for these requests when it handles the message.
struct Ask {
    std::vector<Request> requests;
};
struct Ping {
    std::string tag;
};

// An area under any id that logs what reaches it.
class FakeArea final : public Area {
  public:
    FakeArea(std::string_view id, Log log) : id_(id), log_(std::move(log)) {}
    [[nodiscard]] std::string_view id() const override { return id_; }
    Task update(AreaMsg message, Ctx& ctx) override {
        if (auto ask = std::move(message).downcast<Ask>()) {
            for (auto& r : ask->requests) ctx.request(std::move(r));
            log_->push_back(std::string(id_) + ":ask");
        } else if (const auto* ping = message.downcast_ref<Ping>()) {
            log_->push_back(std::string(id_) + ":ping:" + ping->tag);
        } else if (message.is<to::tools::Open>()) {
            log_->push_back(std::string(id_) + ":open");
        } else if (message.is<to::projects::Open>()) {
            log_->push_back(std::string(id_) + ":open-project");
        } else {
            log_->push_back(std::string(id_) + ":other");
        }
        return Task::none();
    }
    [[nodiscard]] std::vector<PanelTab> panel_tabs() const override {
        if (id_ == area_id::TOOLS) return {{"tools", "Tools", std::nullopt, false}};
        return {};
    }
    Task project_activated(std::string_view project, const fs::path&, Ctx&) override {
        log_->push_back(std::string(id_) + ":activated:" + std::string(project));
        return Task::none();
    }

  private:
    std::string_view id_;
    Log log_;
};

AreaBoot fake(std::string_view id, Log log) {
    return {id, [id, log](Ctx& ctx) -> std::pair<std::unique_ptr<Area>, Task> {
                log->push_back("boot:" + std::string(id));
                (void)ctx;
                return {std::make_unique<FakeArea>(id, log), Task::none()};
            }};
}

// The app over a temporary data directory with offline backends and `boots`.
struct Fixture {
    loa::runtime::TempDir dir = std::move(*loa::runtime::TempDir::create());
    fs::path data = dir.path() / "loa";
    fs::path work = dir.path() / "work";
    std::unique_ptr<App> app;
    std::vector<std::string> warnings;

    explicit Fixture(std::vector<AreaBoot> boots, std::optional<std::string> config = std::nullopt) {
        fs::create_directories(work);
        if (config) write(data / CONFIG_FILE, *config);
        auto loaded = Services::load_in(data, memory_key, work.string(), Host::system());
        REQUIRE(loaded.has_value());
        auto [services, w] = std::move(*loaded);
        warnings = std::move(w);
        auto library = open_library(services.storage.dirs, services.node);
        REQUIRE(library.has_value());
        auto runtime = runtime_parts(services, std::move(*library), {});
        namespace t = loa::models::testing;
        loa::models::Parts models{
            .rpc = std::make_shared<t::FakeOmp>(),
            .omp_account = std::make_shared<t::OfflineOmpAccount>(),
            .files = loa::settings::models::files::OmpFiles{dir.path() / "agent"},
            .validator = std::make_shared<t::RejectingValidator>(),
            .opener = std::make_shared<t::NoBrowser>(),
            .harnesses = {},
            .environment = std::make_shared<t::NoEnv>(),
        };
        AppParts parts{std::move(runtime), std::move(models), nullptr, dir.path(),
                       std::make_shared<loa::harness::account_support::SystemUrlOpener>(), std::move(boots)};
        app = App::build(std::move(services), std::move(parts)).first;
    }
};

} // namespace

TEST_CASE("startup creates the data directory with the Rust layout") {
    auto dir = std::move(*loa::runtime::TempDir::create());
    const auto data = dir.path() / "loa";
    auto loaded = Services::load_in(data, memory_key, std::nullopt, Host::system());
    REQUIRE(loaded.has_value());
    const auto& services = loaded->first;
    CHECK(loaded->second.empty());
    CHECK(fs::is_regular_file(data / "grants.db"));
    CHECK(services.storage.config.path() == data / "config.toml");
    CHECK(services.storage.dirs.sessions == data / "sessions");
    CHECK(services.storage.dirs.omp == data / "omp");
    CHECK(services.workspace == fs::temp_directory_path());
    CHECK(services.node.key == services.node.identity.public_key().to_string());
    // The grant store runs the Rust schema (golden from F5).
    CHECK(read(fs::path(LOA_GOLDEN_DIR) / "sqlite/grants.sql") == loa::trust::GrantStore::SCHEMA);
    auto library = open_library(services.storage.dirs, services.node);
    REQUIRE(library.has_value());
    CHECK(fs::is_directory(data / "sessions"));
}

TEST_CASE("a broken key store falls back to a memory key with a warning") {
    auto dir = std::move(*loa::runtime::TempDir::create());
    auto broken = [](const fs::path&) -> loa::trust::Result<std::pair<loa::trust::NodeIdentity, loa::trust::KeySource>> {
        return std::unexpected(loa::trust::TrustError::unavailable("no keychain"));
    };
    auto loaded = Services::load_in(dir.path() / "loa", broken, std::nullopt, Host::system());
    REQUIRE(loaded.has_value());
    REQUIRE(loaded->second.size() == 1);
    CHECK(loaded->second[0].starts_with("Neither the OS keychain nor the key file works ("));
    CHECK(std::holds_alternative<loa::trust::key_source::Memory>(loaded->first.node.key_source));
}

TEST_CASE("the app starts from the central config (golden from F5)") {
    Fixture f({}, read(fs::path(LOA_GOLDEN_DIR) / "settings/config.toml"));
    auto& ctx = f.app->ctx;
    CHECK(ctx.services.storage.config.path() == f.data / "config.toml");
    CHECK(ctx.shell.project_tabs());
    CHECK(ctx.shell.sidebar().pins().contains("s1"));
    CHECK(ctx.shell.sidebar().pinned_projects().contains("loa"));
    // `/src/loa` doesn't exist: the app starts on no project, in the launch folder.
    CHECK_FALSE(ctx.runtime.project().has_value());
    CHECK(ctx.runtime.cwd() == fs::canonical(f.work));
    CHECK(ctx.runtime.access() == std::optional<std::string>("write"));
    const auto config = ctx.read_config("settings");
    REQUIRE(config.has_value());
    CHECK(config->harness.approval_mode == std::optional<std::string>("write"));
}

TEST_CASE("the registry boots areas in the fixed order") {
    auto log = std::make_shared<std::vector<std::string>>();
    Fixture f({fake(area_id::WORKFLOWS, log), fake(area_id::HARNESS, log), fake(area_id::PEERS, log),
               fake(area_id::SESSIONS, log), fake("bogus", log)});
    const std::vector<std::string_view> expected{area_id::HARNESS, area_id::SESSIONS, area_id::PEERS, area_id::WORKFLOWS};
    CHECK(f.app->areas.ids() == expected);
    CHECK(joined(*log) == "boot:harness boot:sessions boot:peers boot:workflows");
    REQUIRE(f.app->ctx.notice().has_value());
    CHECK(f.app->ctx.notice()->find("No area bogus") != std::string::npos);
    CHECK(AREA_ORDER == std::array<std::string_view, 12>{"harness", "sessions", "decisions", "tools", "surfaces", "models",
                                                         "projects", "peers", "updater", "sandbox", "agents", "workflows"});
}

TEST_CASE("settle runs what areas ask for") {
    auto log = std::make_shared<std::vector<std::string>>();
    Fixture f({fake(area_id::PROJECTS, log), fake(area_id::TOOLS, log)});
    auto& app = *f.app;
    log->clear();
    const auto folder = f.dir.path() / "project";
    fs::create_directories(folder);

    std::vector<Request> requests;
    requests.emplace_back(request::OpenTab{"tools"});
    requests.emplace_back(request::Forward{area_message(area_id::TOOLS, Ping{"forwarded"})});
    requests.emplace_back(request::ActivateProject{"p", folder});
    (void)app.update(area_message(area_id::PROJECTS, Ask{std::move(requests)}));

    CHECK(app.ctx.shell.active_tab() == std::optional<std::string_view>("tools"));
    // Every area hears of the project, in `AREA_ORDER` (tools before projects).
    CHECK(joined(*log) == "projects:ask tools:ping:forwarded tools:activated:p projects:activated:p");
    CHECK(app.ctx.runtime.project() == std::optional<std::string>("p"));
    CHECK(app.ctx.runtime.cwd() == folder);
    CHECK(app.workspace.open_projects == std::vector<std::string>{"p"});
    CHECK(app.ctx.read_config("settings")->active_project->name == "p");

    // A revealed tab is listed without taking the open panel from the tab it shows.
    std::vector<Request> reveal;
    reveal.emplace_back(request::RevealTab{"files"});
    (void)app.update(area_message(area_id::TOOLS, Ask{std::move(reveal)}));
    CHECK(app.ctx.shell.active_tab() == std::optional<std::string_view>("tools"));

    std::vector<Request> forget;
    forget.emplace_back(request::ForgetProject{"p"});
    (void)app.update(area_message(area_id::PROJECTS, Ask{std::move(forget)}));
    CHECK_FALSE(app.ctx.runtime.project().has_value());
    CHECK(app.workspace.open_projects.empty());
    CHECK_FALSE(app.ctx.read_config("settings")->active_project.has_value());

    // An unknown tab is a notice.
    std::vector<Request> missing;
    missing.emplace_back(request::OpenTab{"nowhere"});
    (void)app.update(area_message(area_id::TOOLS, Ask{std::move(missing)}));
    CHECK(app.ctx.notice() == std::optional<std::string>("There is no nowhere tab"));
}

TEST_CASE("shell intents route to the areas that own them") {
    auto log = std::make_shared<std::vector<std::string>>();
    Fixture f({fake(area_id::PROJECTS, log), fake(area_id::TOOLS, log)});
    auto& app = *f.app;
    log->clear();
    namespace nav = loa::view::shell::sidebar_intent;
    namespace layout = loa::ui::shell::layout;
    auto sidebar = [](loa::view::shell::SidebarIntent i) {
        return msg::Shell{layout::msg::Emit{loa::view::shell::intent::Sidebar{std::move(i)}}};
    };
    (void)app.update(sidebar(nav::OpenTools{}));
    (void)app.update(sidebar(nav::OpenProject{"p"}));
    CHECK(joined(*log) == "tools:open projects:open-project");

    (void)app.update(sidebar(nav::DeleteSession{"peer:abc:s1"}));
    CHECK(app.ctx.notice()->starts_with("A peer's session is deleted on its own node"));

    (void)app.update(sidebar(nav::OpenSettings{}));
    REQUIRE(app.windows.overlay.has_value());
    CHECK(std::get<overlay::Settings>(*app.windows.overlay).tab == SettingsTab::General);
    // Esc closes the overlay, then a screen.
    (void)app.escape();
    CHECK_FALSE(app.windows.overlay.has_value());
    app.ctx.show_screen(area_id::TOOLS);
    (void)app.escape();
    CHECK_FALSE(app.ctx.screen().has_value());

    // Messages for an area that isn't registered are a notice.
    (void)app.update(area_message(area_id::MODELS, to::models::Close{}));
    CHECK(app.ctx.notice()->starts_with("No area models"));
}

TEST_CASE("detached tabs open a window and reattach to the panel") {
    auto log = std::make_shared<std::vector<std::string>>();
    Fixture f({fake(area_id::TOOLS, log)});
    auto& app = *f.app;
    auto open = app.open_main();
    REQUIRE(app.windows.main.has_value());
    REQUIRE(open.ops.size() == 1);
    CHECK(std::get<ui_op::OpenWindow>(open.ops[0]).id == *app.windows.main);

    auto task = app.update(msg::Window{window_msg::Detach{"files"}});
    REQUIRE(app.windows.detached.size() == 1);
    const auto id = app.windows.detached[0].first;
    CHECK(std::get<ui_op::OpenWindow>(task.ops.at(0)).id == id);
    CHECK(app.ctx.shell.is_detached("files"));
    CHECK(app.title(id) == "Files — Loa");
    // Detaching again focuses the window.
    auto again = app.update(msg::Window{window_msg::Detach{"files"}});
    CHECK(std::holds_alternative<ui_op::FocusWindow>(again.ops.at(0)));

    (void)app.update(msg::Window{window_msg::Reattach{id}});
    CHECK(app.windows.detached.empty());
    CHECK_FALSE(app.ctx.shell.is_detached("files"));
    CHECK(app.ctx.shell.active_tab() == std::optional<std::string_view>("files"));

    // Closing the main window quits.
    auto quit = app.update(msg::Window{window_msg::Closed{*app.windows.main}});
    CHECK(std::holds_alternative<ui_op::Exit>(quit.ops.at(0)));
}

namespace {

// A typed area, as the areas of later issues declare them.
class Tools final : public TypedArea<Tools> {
  public:
    static constexpr std::string_view ID = area_id::TOOLS;
    using Message = std::variant<to::tools::Open, Ping>;

    static std::span<const AreaCommand<Tools>> COMMANDS() {
        static const std::array<AreaCommand<Tools>, 2> commands{
            AreaCommand<Tools>{"tools.open", "Open tools", "Tools", std::nullopt, Listed<Tools>::always(),
                               [](Tools& t, std::string_view, Ctx&) {
                                   t.seen.emplace_back("open");
                                   return Task::none();
                               }}
                .with_slash("tools", "Show the tools"),
            AreaCommand<Tools>{"tools.server.", "", "Tools", std::nullopt,
                               Listed<Tools>::each_([](const Tools&, const Ctx&) {
                                   return std::vector<std::pair<std::string, std::string>>{{"a", "Server a"}};
                               }),
                               [](Tools& t, std::string_view arg, Ctx&) {
                                   t.seen.push_back("server " + std::string(arg));
                                   return Task::none();
                               }},
        };
        return commands;
    }

    Task update_typed(Message message, Ctx&) {
        seen.emplace_back(std::holds_alternative<Ping>(message) ? "ping" : "open-message");
        return Task::none();
    }

    std::vector<std::string> seen;
};

} // namespace

TEST_CASE("typed areas take their own messages, commands and slash commands") {
    Fixture f({{area_id::TOOLS, [](Ctx&) -> std::pair<std::unique_ptr<Area>, Task> {
                    return {std::make_unique<Tools>(), Task::none()};
                }}});
    auto& app = *f.app;
    auto* tools = app.areas.get<Tools>();
    REQUIRE(tools != nullptr);

    (void)app.update(Tools::message(Ping{"x"}));
    (void)app.update(area_message(area_id::TOOLS, to::tools::Open{}));
    (void)app.update(area_message(area_id::TOOLS, to::projects::New{}));
    CHECK(tools->seen == std::vector<std::string>{"ping", "open-message"});
    CHECK(app.ctx.notice()->starts_with("The tools area can't handle"));

    const auto rows = tools->commands(app.ctx);
    REQUIRE(rows.size() == 2);
    CHECK(rows[0].id.as_str() == "tools.open");
    CHECK(rows[1].id.as_str() == "tools.server.a");
    CHECK(rows[1].title == "Server a");

    (void)app.command(loa::ui::shell::palette::CommandId("tools.server.a"));
    (void)app.command(loa::ui::shell::palette::CommandId("tools.open"));
    CHECK(tools->seen.back() == "open");
    CHECK(tools->seen.at(tools->seen.size() - 2) == "server a");
    (void)app.command(loa::ui::shell::palette::CommandId("tools.nothing"));
    CHECK(app.ctx.notice() == std::optional<std::string>("Unknown command tools.nothing"));

    // The `/` command list reaches the composer.
    REQUIRE(app.ctx.slash.size() == 1);
    CHECK(app.ctx.slash[0].name == "tools");
    CHECK(tools->slash("  /tools now ", app.ctx).has_value());
    CHECK_FALSE(tools->slash("/other", app.ctx).has_value());
}

TEST_CASE("area messages keep their type") {
    auto m = AreaMsg::make(Ping{"a"});
    auto copy = m;
    CHECK(copy.is<Ping>());
    CHECK_FALSE(copy.is<Ask>());
    CHECK(copy.downcast_ref<Ping>()->tag == "a");
    CHECK_FALSE(std::move(copy).downcast<Ask>().has_value());
    CHECK(std::move(m).downcast<Ping>()->tag == "a");
}

TEST_CASE("slash commands split into name and arguments") {
    CHECK(split_slash(" /model  opus fast ") == std::optional(std::pair<std::string_view, std::string_view>{"model", "opus fast"}));
    CHECK(split_slash("/compact") == std::optional(std::pair<std::string_view, std::string_view>{"compact", ""}));
    CHECK_FALSE(split_slash("hello /there").has_value());
}

TEST_CASE("peer access refuses every request until grants are set") {
    PeerAccess access;
    loa::core::Dom dom;
    auto denied = access.authorize(loa::bus::SessionId("s"), loa::bus::NodeKey{}, loa::core::Draft{}, dom);
    REQUIRE_FALSE(denied.has_value());
    CHECK(denied.error().reason == "this node has issued no grants");
}

TEST_CASE("starts in the temp folder unless a folder is given") {
    const auto temp = fs::temp_directory_path();
    auto given = std::move(*loa::runtime::TempDir::create());
    CHECK(starting_folder(std::nullopt, temp) == temp);
    CHECK(starting_folder("/no/such/folder", temp) == temp);
    CHECK(starting_folder(given.path().string(), temp) == fs::canonical(given.path()));
}

TEST_CASE("scan lists project files before global ones") {
    auto project = std::move(*loa::runtime::TempDir::create());
    auto home = std::move(*loa::runtime::TempDir::create());
    write(project.path() / ".omp/agents/reviewer.md", "---\nname: reviewer\n---\n");
    write(home.path() / ".omp/agent/agents/explore.md", "explore");
    write(home.path() / ".omp/agent/skills/git-workflow/SKILL.md", "skill");
    write(project.path() / "AGENTS.md", "rules");

    auto agents = scan(ConfigsTab::Agents, project.path(), home.path());
    REQUIRE(agents.size() == 2);
    CHECK(agents[0].name == "reviewer");
    CHECK(agents[0].scope == "This project");
    CHECK(agents[1].name == "explore");
    CHECK(agents[1].scope == "All projects");
    auto skills = scan(ConfigsTab::Skills, project.path(), home.path());
    REQUIRE(skills.size() == 1);
    CHECK(skills[0].name == "git-workflow");
    auto rules = scan(ConfigsTab::Rules, project.path(), home.path());
    REQUIRE(rules.size() == 1);
    CHECK(rules[0].name == "AGENTS.md");
}

TEST_CASE("reads the branch and shortens home") {
    auto repo = std::move(*loa::runtime::TempDir::create());
    write(repo.path() / ".git/HEAD", "ref: refs/heads/tai/ui-v3\n");
    fs::create_directories(repo.path() / "crates/app");
    GitCli git;
    CHECK(git.branch(repo.path() / "crates/app") == std::optional<std::string>("tai/ui-v3"));
    write(repo.path() / ".git/HEAD", "0123456789abcdef\n");
    CHECK(git.branch(repo.path()) == std::optional<std::string>("0123456"));
    auto other = std::move(*loa::runtime::TempDir::create());
    CHECK_FALSE(git.branch(other.path()).has_value());
    if (auto home = loa::runtime::fs::home()) CHECK(chip(git, *home / "Projects/omp-gui").path == "~/Projects/omp-gui");
}

TEST_CASE("the top bar status names the latest unfinished turn") {
    CHECK_FALSE(turn_status(loa::core::Dom{}).has_value());
}
