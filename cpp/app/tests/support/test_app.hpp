#pragma once

// The app driven headless for tests (port of `loa-app/src/testing/mod.rs`): `TestApp` runs the
// tasks `App::update` returns and the app's subscriptions on its own executor, feeding every
// message back through `update` on the test thread, as the Qt host does on the GUI thread.
// `FakeHarness` is the scripted harness sessions run on.

#include <chrono>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/app/app.hpp"
#include "loa/app/usage.hpp"
#include "loa/harness/testing/scripted.hpp"
#include "loa/models/testing.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/settings/app/schema.hpp"

namespace loa::app::testing {

// The scripted harness of `TestApp` sessions.
using FakeHarness = loa::harness::testing::ScriptedAdapter;

// How long `TestApp::run` and `TestApp::until` wait; generous, for a loaded machine.
inline constexpr std::chrono::seconds PATIENCE{30};

// One account with a usable saved reset; records the resets spent.
class FakeUsage final : public UsageSource {
  public:
    [[nodiscard]] contracts::Awaitable<std::expected<std::string, std::string>> fetch() const override;
    [[nodiscard]] contracts::Awaitable<std::expected<void, std::string>> spend(std::string target) const override;
    [[nodiscard]] std::vector<std::string> spent() const;

  private:
    mutable std::mutex mutex_;
    mutable std::vector<std::string> spent_;
};

// Messages handed back by jobs and subscriptions, from any thread.
struct Inbox;

class TestApp {
  public:
    using Setup = std::function<void(settings::app::Config&)>;

    // An app on harness `fake`, taking host tools and MCP servers, in a fresh home, on its own
    // executor.
    TestApp();
    explicit TestApp(FakeHarness harness);
    // An app on `executor` whose central config `setup` edits first; `name` is the node's label.
    TestApp(std::shared_ptr<loa::runtime::Executor> executor, FakeHarness harness, std::string_view name,
            const Setup& setup);
    ~TestApp();
    TestApp(const TestApp&) = delete;
    TestApp& operator=(const TestApp&) = delete;

    // The app started again over the same home, as after a restart.
    void reopen();

    // Lists the stored sessions in Recents, as the journal scan `App::build` starts does once
    // it finishes.
    void list_recents();

    // The area of type `A`.
    template <typename A>
    [[nodiscard]] A& area() const {
        auto* a = app->areas.get<A>();
        require_area(a);
        return *a;
    }

    // Updates the app with `message` and runs the task it returns.
    void send(Message message);
    // Updates the app with `message` and returns its task unrun, so the test acts before what it
    // does (a harness start, say) completes.
    [[nodiscard]] Task update(Message message);
    // Runs `task` as the Qt host would, feeding every message it produces back through `update`,
    // until no job of it (or of what it led to) is left.
    void run(Task task);
    // Runs palette command `id` as picking it in the palette does.
    void command(std::string_view id);
    // Types `query` in the palette and runs the first match.
    void palette(std::string_view query);
    // Types `text` into the composer without sending it.
    void type_text(std::string_view text);
    // Types `text` into the composer and submits it.
    void prompt(std::string_view text);
    // A key press no widget captured.
    void key(ui::shell::palette::KeyPress press);
    void key(ui::shell::palette::Named named, ui::shell::palette::Modifiers modifiers = {});
    // Delivers what the app's subscriptions produced so far: new subscriptions start, dropped
    // ones stop.
    void pump_feeds();
    // Pumps feeds and tasks until `done` holds; fails the test after `PATIENCE`.
    void until(std::string_view what, const std::function<bool(const App&)>& done);
    // Starts a session with `text` and waits for its turn to end.
    std::string session(std::string_view text);
    [[nodiscard]] std::optional<std::string> active() const;
    [[nodiscard]] std::string notice() const;
    // The titles of the palette's rows.
    [[nodiscard]] std::vector<std::string> palette_titles();
    // Points the model screens at `omp` instead of an omp that knows nothing.
    [[nodiscard]] static models::Parts offline_models(std::shared_ptr<models::testing::FakeOmp> omp,
                                                      const std::filesystem::path& home);

    std::unique_ptr<App> app;
    FakeHarness harness;
    std::shared_ptr<FakeUsage> usage;
    loa::runtime::TempDir dir;

  private:
    static void require_area(const void* area);
    void open(std::string_view name, const Setup& setup);
    void stop_feeds();
    // Hands `message` to the app and runs what it asks.
    void deliver(Message message);

    std::shared_ptr<loa::runtime::Executor> executor_;
    std::shared_ptr<Inbox> inbox_;
    struct Feed;
    std::map<std::string, std::unique_ptr<Feed>> feeds_;
};

} // namespace loa::app::testing
