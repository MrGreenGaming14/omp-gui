#pragma once

// The Asio-to-Qt bridge: work runs on the `loa::runtime` executor, completions come back on the
// GUI thread through Qt's queued event delivery. Asio is forward-declared (via executor.hpp);
// callers that build `Task`s include <asio.hpp> themselves.

#include <exception>
#include <functional>
#include <memory>

#include "loa/runtime/executor.hpp"

class QObject;

namespace loa::ui::app {

class AsioBridge {
  public:
    // Completions are delivered to the thread `gui` lives in (the GUI thread); `nullptr` means
    // the thread constructing the bridge.
    explicit AsioBridge(loa::runtime::Executor& executor, QObject* gui = nullptr);
    // Completions still queued are dropped.
    ~AsioBridge();
    AsioBridge(const AsioBridge&) = delete;
    AsioBridge& operator=(const AsioBridge&) = delete;

    // Runs `fn` on the GUI thread (queued, even when called from it). Thread-safe.
    void post(std::function<void()> fn) const;

    // Runs `fn` on an executor thread. Thread-safe.
    void dispatch(std::function<void()> fn) const;

    // Spawns `task` on the executor; when it finishes `done` runs on the GUI thread with the
    // exception it threw, if any.
    void spawn(loa::runtime::Task task, std::function<void(std::exception_ptr)> done);

    [[nodiscard]] loa::runtime::Executor& executor() const { return executor_; }

  private:
    struct State;
    loa::runtime::Executor& executor_;
    std::shared_ptr<State> state_;
};

} // namespace loa::ui::app
