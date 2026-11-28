#pragma once

// An app over a temporary data directory with offline backends, and a driver that runs the
// tasks its updates return (the Rust `TestApp` of `loa-app/src/testing.rs`, reduced to what the
// glue and workflows tests use).

#include <chrono>
#include <condition_variable>
#include <deque>
#include <filesystem>
#include <fstream>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>

#include <doctest/doctest.h>

#include "loa/app/app.hpp"
#include "loa/app/services.hpp"
#include "loa/harness/account_support/opener.hpp"
#include "loa/models/testing.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"

namespace loa::app::testing {

namespace fs = std::filesystem;

inline std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream text;
    text << in.rdbuf();
    return text.str();
}

inline void write_file(const fs::path& path, std::string_view text) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << text;
}

inline trust::Result<std::pair<trust::NodeIdentity, trust::KeySource>> memory_key(const fs::path&) {
    std::array<std::uint8_t, 32> seed{};
    seed.fill(1);
    return std::pair{trust::NodeIdentity::from_secret_bytes(seed), trust::KeySource{trust::key_source::Memory{"test"}}};
}

// The app over a temporary directory: omp's agent directory is `<dir>/agent`.
struct TestApp {
    runtime::TempDir dir = std::move(*runtime::TempDir::create());
    fs::path data = dir.path() / "loa";
    fs::path work = dir.path() / "work";
    fs::path agent = dir.path() / "agent";
    std::unique_ptr<App> app;

    explicit TestApp(std::vector<AreaBoot> boots) {
        fs::create_directories(work);
        auto loaded = Services::load_in(data, memory_key, work.string(), Host::system());
        REQUIRE(loaded.has_value());
        auto services = std::move(loaded->first);
        auto library = open_library(services.storage.dirs, services.node);
        REQUIRE(library.has_value());
        auto runtime = runtime_parts(services, std::move(*library), {});
        namespace t = loa::models::testing;
        models::Parts models{
            .rpc = std::make_shared<t::FakeOmp>(),
            .omp_account = std::make_shared<t::OfflineOmpAccount>(),
            .files = settings::models::files::OmpFiles{agent},
            .validator = std::make_shared<t::RejectingValidator>(),
            .opener = std::make_shared<t::NoBrowser>(),
            .harnesses = {},
            .environment = std::make_shared<t::NoEnv>(),
        };
        AppParts parts{std::move(runtime), std::move(models), nullptr, dir.path(),
                       std::make_shared<::loa::harness::account_support::SystemUrlOpener>(), std::move(boots)};
        app = App::build(std::move(services), std::move(parts)).first;
    }

    // Runs `task` until nothing it started is left: messages through `update`, jobs on an
    // executor whose messages come back here. Each job emits once (`perform`, `blocking`).
    void run(Task task) {
        static runtime::Executor executor(2);
        struct Inbox {
            std::mutex mutex;
            std::condition_variable ready;
            std::deque<Message> messages;
            long pending = 0;
        };
        auto inbox = std::make_shared<Inbox>();
        auto take = [&](Task t) {
            std::lock_guard lock(inbox->mutex);
            for (auto& m : t.messages) inbox->messages.push_back(std::move(m));
            inbox->pending += static_cast<long>(t.jobs.size());
            return std::move(t.jobs);
        };
        auto start = [&](std::vector<Task::Job> jobs) {
            for (auto& job : jobs)
                job(
                    [inbox](Message m) {
                        std::lock_guard lock(inbox->mutex);
                        inbox->messages.push_back(std::move(m));
                        --inbox->pending;
                        inbox->ready.notify_all();
                    },
                    executor);
        };
        start(take(std::move(task)));
        // Generous: other builds load the machine.
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(60);
        for (;;) {
            std::optional<Message> next;
            {
                std::unique_lock lock(inbox->mutex);
                inbox->ready.wait_until(lock, deadline, [&] { return !inbox->messages.empty() || inbox->pending <= 0; });
                if (inbox->messages.empty()) {
                    REQUIRE_MESSAGE(inbox->pending <= 0, "a job never answered");
                    return;
                }
                next = std::move(inbox->messages.front());
                inbox->messages.pop_front();
            }
            start(take(app->update(std::move(*next))));
        }
    }

    void send(Message message) { run(app->update(std::move(message))); }
    void command(std::string id) { run(app->command(ui::shell::palette::CommandId(std::move(id)))); }
    [[nodiscard]] std::string notice() const { return app->ctx.notice().value_or(""); }
};

} // namespace loa::app::testing
