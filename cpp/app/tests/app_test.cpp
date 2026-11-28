#include <algorithm>
#include <filesystem>
#include <fstream>
#include <set>

#include <doctest/doctest.h>

#include <atomic>
#include <chrono>
#include <stdexcept>
#include <thread>

#include <asio.hpp>

#include <QApplication>
#include <QElapsedTimer>
#include <QThread>

#include "loa/runtime/executor.hpp"
#include "loa/ui/app/application.hpp"
#include "loa/ui/app/asio_bridge.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/ui/app/local_fs.hpp"
#include "loa/ui/app/smoke.hpp"

using namespace loa::ui::app;

namespace {

QApplication& qt_app() {
    static int argc = 1;
    static char name[] = "loa_app_tests";
    static char* argv[] = {name, nullptr};
    if (qEnvironmentVariableIsEmpty("QT_QPA_PLATFORM")) qputenv("QT_QPA_PLATFORM", "offscreen");
    // Leaked: destroying a QApplication during static destruction crashes in Qt's own teardown.
    static auto* app = new QApplication(argc, argv);
    return *app;
}

// Spins the GUI event loop until `done` or a 5 s timeout.
template <typename F> bool pump_until(F&& done) {
    QElapsedTimer t;
    t.start();
    while (!done() && t.elapsed() < 5000) {
        QApplication::processEvents(QEventLoop::AllEvents, 10);
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    return done();
}

} // namespace

TEST_CASE("the bridge delivers task completions on the GUI thread") {
    auto& app = qt_app();
    loa::runtime::Executor executor(2);
    AsioBridge bridge(executor, &app);

    std::atomic<std::thread::id> worker{};
    bool done = false;
    bool on_gui = false;
    bool ok = false;
    bridge.spawn(
        [](std::atomic<std::thread::id>& w) -> loa::runtime::Task {
            w = std::this_thread::get_id();
            co_return;
        }(worker),
        [&](std::exception_ptr e) {
            on_gui = QThread::currentThread() == app.thread();
            ok = !e;
            done = true;
        });
    REQUIRE(pump_until([&] { return done; }));
    CHECK(on_gui);
    CHECK(ok);
    CHECK(worker.load() != std::this_thread::get_id());

    // Failures arrive as the exception, still on the GUI thread.
    std::exception_ptr error;
    bool failed = false;
    bridge.spawn([]() -> loa::runtime::Task {
        throw std::runtime_error("boom");
        co_return;
    }(),
                 [&](std::exception_ptr e) {
                     error = e;
                     failed = QThread::currentThread() == app.thread();
                 });
    REQUIRE(pump_until([&] { return error != nullptr; }));
    CHECK(failed);
    CHECK_THROWS_WITH_AS(std::rethrow_exception(error), "boom", std::runtime_error);

    // `dispatch` runs off the GUI thread, `post` brings results back.
    bool posted = false;
    bridge.dispatch([&] {
        const bool off_gui = QThread::currentThread() != app.thread();
        bridge.post([&, off_gui] { posted = off_gui && QThread::currentThread() == app.thread(); });
    });
    REQUIRE(pump_until([&] { return posted; }));
    executor.stop();
}

TEST_CASE("a destroyed bridge drops late completions") {
    auto& app = qt_app();
    loa::runtime::Executor executor(1);
    bool called = false;
    {
        AsioBridge bridge(executor, &app);
        bridge.post([&] { called = true; });
    }
    QApplication::processEvents();
    CHECK_FALSE(called);
    executor.stop();
}

TEST_CASE("command line options") {
    auto parse = [](std::initializer_list<std::string_view> a) { return parse_args(std::vector<std::string_view>(a)); };
    CHECK(parse({"loa-qt", "--smoke", "kit"}).smoke == std::optional<std::string>("kit"));
    CHECK(parse({"loa-qt", "--smoke"}).error == "--smoke needs a screen name");
    CHECK(parse({"loa-qt", "--bogus"}).error == "unknown option --bogus");
    CHECK(parse({"loa-qt", "--help"}).help);
    CHECK_FALSE(parse({"loa-qt"}).smoke);
}

// Every registered screen must pass; later UI issues add screens, so the list is not pinned.
TEST_CASE("smoke screens: every registered screen passes, unknown screens are rejected") {
    qt_app();
    auto& smoke = Smoke::registry();
    const auto names = smoke.names();
    for (const char* required : {"agents", "composer", "kit", "models", "panel", "projects", "shell", "terminal", "workflows"})
        CHECK(std::ranges::find(names, required) != names.end());
    for (const auto& name : names) {
        CAPTURE(name);
        CHECK(smoke.run(name) == 0);
    }
    CHECK(smoke.run("nope") == 2);
}

TEST_CASE("LocalFs lists through gitignore: negation and ** apply, .git is hidden") {
    namespace fs = std::filesystem;
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto root = dir->path();
    fs::create_directories(root / ".git");
    fs::create_directories(root / "src");
    for (const auto* f : {"keep.log", "drop.log", "a.txt", ".env", "src/x.tmp", "src/y.rs"}) std::ofstream(root / f) << "x";
    std::ofstream(root / ".gitignore") << "*.log\n!keep.log\n**/*.tmp\n";
    LocalFs local;
    auto names = [&](const fs::path& d) {
        auto listed = local.list(d);
        REQUIRE(listed);
        std::set<std::string> out;
        for (const auto& e : *listed) out.insert(e.name + (e.is_dir ? "/" : ""));
        return out;
    };
    CHECK(names(root) == std::set<std::string>{".env", ".gitignore", "a.txt", "keep.log", "src/"});
    CHECK(names(root / "src") == std::set<std::string>{"y.rs"});
    CHECK_FALSE(local.list(root / "missing"));
}
