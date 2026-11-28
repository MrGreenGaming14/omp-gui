#include "support/test_app.hpp"

#include <condition_variable>
#include <deque>
#include <stop_token>
#include <thread>

#include <asio.hpp>
#include <doctest/doctest.h>

#include "loa/app/services.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/trust/keystore.hpp"

namespace loa::app::testing {

namespace fs = std::filesystem;
namespace palette = ui::shell::palette;
namespace composer = ui::session::composer;
using Clock = std::chrono::steady_clock;

struct Inbox {
    std::mutex mutex;
    std::condition_variable changed;
    // What tasks' jobs emitted.
    std::deque<Message> jobs;
    // What subscriptions emitted.
    std::deque<Message> feeds;

    void push(std::deque<Message> Inbox::*queue, Message message) {
        {
            std::lock_guard lock(mutex);
            (this->*queue).push_back(std::move(message));
        }
        changed.notify_all();
    }
    std::deque<Message> take(std::deque<Message> Inbox::*queue) {
        std::lock_guard lock(mutex);
        return std::exchange(this->*queue, {});
    }
};

struct TestApp::Feed {
    std::stop_source stop;
    // `subscription::Every`: the message and when it is due next.
    std::optional<Message> every;
    std::chrono::milliseconds period{0};
    Clock::time_point due{};

    ~Feed() { stop.request_stop(); }
};

namespace {

// Refuses everything that would leave the test: model calls, git, folder dialogs.
class OfflineSummarizer final : public contracts::Summarizer {
  public:
    [[nodiscard]] contracts::Awaitable<std::expected<std::string, std::string>> summarize(std::string,
                                                                                          std::string) const override {
        co_return std::unexpected(std::string("no model calls in tests"));
    }
};

class OfflineSummarizers final : public projects::Summarizers {
  public:
    [[nodiscard]] std::pair<std::shared_ptr<const projects::Summarizer>, std::optional<std::string>> on(
        std::optional<std::string_view>) const override {
        return {std::make_shared<OfflineSummarizer>(), std::nullopt};
    }
};

class OfflineGit final : public GitProbe {
  public:
    [[nodiscard]] std::optional<std::string> branch(const fs::path&) const override { return std::nullopt; }
    [[nodiscard]] contracts::Awaitable<std::optional<std::string>> diff(fs::path) const override { co_return std::nullopt; }
};

class OfflinePicker final : public FolderPicker {
  public:
    [[nodiscard]] contracts::Awaitable<std::expected<std::optional<fs::path>, std::string>> pick(fs::path) const override {
        co_return std::unexpected(std::string("no folder dialog in tests"));
    }
};

loa::runtime::TempDir temp_dir() {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE_MESSAGE(dir.has_value(), "a temp dir");
    return std::move(*dir);
}

FakeHarness default_harness() {
    auto capabilities = contracts::Capabilities::builder().host_tools().mcp_servers().build();
    return FakeHarness("fake").with_capabilities(std::move(capabilities));
}

} // namespace

// ---- FakeUsage -------------------------------------------------------------------------------

contracts::Awaitable<std::expected<std::string, std::string>> FakeUsage::fetch() const {
    std::string report =
        R"({"provider": "anthropic", "metadata": {"orgName": "Personal", "credentialId": "c1"}, "resetCredits": {"availableCount": 2, "redeemableCount": 1},
            "limits": [{"label": "Claude 7 Day", "window": {"id": "7d", "resetsAt": 9000}, "amount": {"usedFraction": 0.6}}]})";
    co_return std::string(R"({"generatedAt": 1, "reports": [)") + report + "]}";
}

contracts::Awaitable<std::expected<void, std::string>> FakeUsage::spend(std::string target) const {
    {
        std::lock_guard lock(mutex_);
        spent_.push_back(std::move(target));
    }
    co_return std::expected<void, std::string>{};
}

std::vector<std::string> FakeUsage::spent() const {
    std::lock_guard lock(mutex_);
    return spent_;
}

// ---- TestApp ---------------------------------------------------------------------------------

TestApp::TestApp() : TestApp(default_harness()) {}

TestApp::TestApp(FakeHarness harness)
    : TestApp(std::make_shared<loa::runtime::Executor>(2), std::move(harness), "testhost", [](settings::app::Config&) {}) {}

TestApp::TestApp(std::shared_ptr<loa::runtime::Executor> executor, FakeHarness harness_, std::string_view name,
                 const Setup& setup)
    : harness(std::move(harness_)), dir(temp_dir()), executor_(std::move(executor)), inbox_(std::make_shared<Inbox>()) {
    open(name, setup);
}

TestApp::~TestApp() {
    stop_feeds();
    app.reset();
}

void TestApp::require_area(const void* area) { REQUIRE_MESSAGE(area != nullptr, "the area is registered"); }

void TestApp::stop_feeds() { feeds_.clear(); }

void TestApp::reopen() {
    const std::string name = app->ctx.services.node.label;
    // The process ends: every live session's writer closes and releases its journal, which also
    // ends the feeds still waiting on it (Rust drops them with the app).
    auto library = app->ctx.runtime.library();
    std::vector<std::string> live;
    for (const auto& [id, session] : app->ctx.runtime.sessions().iter()) live.emplace_back(id.as_str());
    stop_feeds();
    app.reset();
    for (const auto& id : live) (void)library->close(id);
    library.reset();
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    inbox_->take(&Inbox::feeds);
    inbox_->take(&Inbox::jobs);
    open(name, [](settings::app::Config&) {});
}

void TestApp::open(std::string_view name, const Setup& setup) {
    auto dirs = Dirs::make(dir.path() / "loa");
    auto config = settings::app::ConfigStore::in_dir(dirs.data);
    auto settings = config.load();
    REQUIRE_MESSAGE(settings.has_value(), "the default config");
    settings->network.discovery.mdns = false;
    settings->network.discovery.tailscale = false;
    setup(*settings);
    REQUIRE_MESSAGE(config.save(*settings).has_value(), "config saved");

    app_runtime::Harnesses harnesses;
    harness::add(harnesses, std::make_shared<FakeHarness>(harness), harness.harness(), true, std::nullopt, std::nullopt);
    std::array<std::uint8_t, 32> seed{};
    seed.fill(static_cast<std::uint8_t>(name.size()));
    auto identity = trust::NodeIdentity::from_secret_bytes(seed);
    trust::KeySource key_source{trust::key_source::Memory{"the key was handed in"}};
    auto node = NodeServices::open(std::move(identity), std::string(name), std::move(key_source), dirs.data);
    REQUIRE_MESSAGE(node.has_value(), "the node");
    // Nothing reaches git, a folder dialog or omp's usage.
    if (!usage) usage = std::make_shared<FakeUsage>();
    Host host{std::make_shared<OfflineGit>(), std::make_shared<OfflinePicker>(), usage};
    auto services = Services::make(std::move(*node), std::move(dirs), std::move(host), dir.path());
    auto library = open_library(services.storage.dirs, services.node);
    REQUIRE_MESSAGE(library.has_value(), "the session library");
    // Nothing reaches omp, a model, a browser or the developer's home.
    auto models = offline_models(std::make_shared<models::testing::FakeOmp>(), dir.path());
    auto runtime = runtime_parts(services, std::move(*library), std::move(harnesses));
    AppParts parts{std::move(runtime), std::move(models), std::make_shared<OfflineSummarizers>(), dir.path() / "home",
                   std::make_shared<models::testing::NoBrowser>()};
    // The startup tasks (harness probes, update checks, project digests) reach outside the
    // test: dropped.
    auto [built, startup] = App::build(std::move(services), std::move(parts));
    (void)startup;
    app = std::move(built);
    list_recents();
}

models::Parts TestApp::offline_models(std::shared_ptr<models::testing::FakeOmp> omp, const fs::path& home) {
    namespace t = models::testing;
    return models::Parts{
        .rpc = std::move(omp),
        .omp_account = std::make_shared<t::OfflineOmpAccount>(),
        .files = settings::models::files::OmpFiles{home / "omp"},
        .validator = std::make_shared<t::RejectingValidator>(),
        .opener = std::make_shared<t::NoBrowser>(),
        .harnesses = {},
        .environment = std::make_shared<t::NoEnv>(),
    };
}

void TestApp::list_recents() {
    auto recents = app_runtime::recents(*app->ctx.runtime.library());
    send(msg::Recents{std::move(recents.summaries), std::move(recents.problems), std::move(recents.empty)});
}

Task TestApp::update(Message message) { return app->update(std::move(message)); }

void TestApp::send(Message message) { run(update(std::move(message))); }

void TestApp::deliver(Message message) { run(update(std::move(message))); }

void TestApp::run(Task task) {
    // A job is done once every copy of the `Emit` it was handed is gone: its coroutine ended.
    std::vector<std::weak_ptr<void>> pending;
    std::deque<Message> next;
    auto start = [&](Task t) {
        for (auto& m : t.messages) next.push_back(std::move(m));
        for (auto& job : t.jobs) {
            auto token = std::make_shared<char>();
            pending.push_back(token);
            job([token, inbox = inbox_](Message m) { inbox->push(&Inbox::jobs, std::move(m)); }, *executor_);
        }
        // Widget and window operations need a view: dropped, as in the Rust tests.
    };
    start(std::move(task));
    const auto deadline = Clock::now() + PATIENCE;
    for (;;) {
        while (!next.empty()) {
            auto message = std::move(next.front());
            next.pop_front();
            start(app->update(std::move(message)));
        }
        std::erase_if(pending, [](const auto& token) { return token.expired(); });
        for (auto& m : inbox_->take(&Inbox::jobs)) next.push_back(std::move(m));
        if (!next.empty()) continue;
        if (pending.empty()) break;
        REQUIRE_MESSAGE(Clock::now() < deadline, "the task finished");
        std::unique_lock lock(inbox_->mutex);
        inbox_->changed.wait_for(lock, std::chrono::milliseconds(2), [&] { return !inbox_->jobs.empty(); });
    }
}

void TestApp::command(std::string_view id) { run(app->command(palette::CommandId(std::string(id)))); }

void TestApp::palette(std::string_view query) {
    send(msg::Palette{palette::msg::Open{}});
    send(msg::Palette{palette::msg::QueryChanged{std::string(query)}});
    send(msg::Palette{palette::msg::Submit{}});
}

void TestApp::type_text(std::string_view text) {
    // UTF-8 to code points.
    for (std::size_t i = 0; i < text.size();) {
        auto byte = static_cast<unsigned char>(text[i]);
        std::size_t len = byte < 0x80 ? 1 : byte < 0xE0 ? 2 : byte < 0xF0 ? 3 : 4;
        char32_t c = len == 1 ? byte : byte & (0xFF >> (len + 1));
        for (std::size_t k = 1; k < len && i + k < text.size(); ++k) c = (c << 6) | (text[i + k] & 0x3F);
        i += len;
        send(msg::Composer{composer::msg::Edit{composer::action::Edit{composer::edit::Insert{c}}}});
    }
}

void TestApp::prompt(std::string_view text) {
    type_text(text);
    send(msg::Composer{composer::msg::Submit{}});
}

void TestApp::key(palette::KeyPress press) { send(msg::Keyboard{std::move(press)}); }

void TestApp::key(palette::Named named, palette::Modifiers modifiers) {
    key(palette::KeyPress{named, std::nullopt, modifiers, false});
}

void TestApp::pump_feeds() {
    std::map<std::string, Subscription> wanted;
    for (auto& sub : app->subscription()) {
        auto key = std::visit([](const auto& s) { return s.key; }, sub);
        wanted.emplace(std::move(key), std::move(sub));
    }
    std::erase_if(feeds_, [&](const auto& feed) { return !wanted.contains(feed.first); });
    const auto now = Clock::now();
    for (auto& [key, sub] : wanted) {
        if (feeds_.contains(key)) continue;
        auto feed = std::make_unique<Feed>();
        if (auto* every = std::get_if<subscription::Every>(&sub)) {
            feed->every = std::move(every->message);
            feed->period = every->period;
            feed->due = now + every->period;
        } else {
            auto& stream = std::get<subscription::Stream>(sub);
            stream.run([inbox = inbox_](Message m) { inbox->push(&Inbox::feeds, std::move(m)); }, feed->stop.get_token(),
                       *executor_);
        }
        feeds_.emplace(key, std::move(feed));
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(5));
    std::vector<Message> messages;
    for (auto& [key, feed] : feeds_) {
        if (!feed->every || Clock::now() < feed->due) continue;
        feed->due = Clock::now() + feed->period;
        messages.push_back(*feed->every);
    }
    for (auto& m : inbox_->take(&Inbox::feeds)) messages.push_back(std::move(m));
    for (auto& m : messages) deliver(std::move(m));
}

void TestApp::until(std::string_view what, const std::function<bool(const App&)>& done) {
    const auto deadline = Clock::now() + PATIENCE;
    while (!done(*app)) {
        REQUIRE_MESSAGE(Clock::now() < deadline, "timed out waiting until " << std::string(what));
        pump_feeds();
    }
}

std::string TestApp::session(std::string_view text) {
    prompt(text);
    until("the session ran", [](const App& a) {
        const auto* session = a.ctx.runtime.sessions().active();
        return session && !session->is_busy();
    });
    auto id = active();
    REQUIRE_MESSAGE(id.has_value(), "an active session");
    return *id;
}

std::optional<std::string> TestApp::active() const {
    const auto& id = app->ctx.runtime.sessions().active_id();
    if (!id) return std::nullopt;
    return std::string(id->as_str());
}

std::string TestApp::notice() const { return app->ctx.notice().value_or(""); }

std::vector<std::string> TestApp::palette_titles() {
    send(msg::Palette{palette::msg::Open{}});
    std::vector<std::string> titles;
    for (const auto& command : app->palette.commands()) titles.push_back(command.title);
    send(msg::Palette{palette::msg::Close{}});
    return titles;
}

} // namespace loa::app::testing
