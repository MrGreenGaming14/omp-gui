#pragma once

// Runs the PTY operations of a terminal `Task` on the `loa::runtime` executor: opening channels through the
// state's backend, the read loop and the write loop. Messages come back through `post`, which the app points
// at the GUI thread. Clipboard operations are the view's (`TerminalView::apply`). Asio stays out of this header.

#include <functional>
#include <memory>

#include "loa/ui/terminal/state.hpp"

namespace loa::ui::terminal {

class Runner {
  public:
    using Post = std::function<void(Message)>;

    Runner(const asio::any_io_executor& executor, Post post);
    ~Runner();
    Runner(const Runner&) = delete;
    Runner& operator=(const Runner&) = delete;

    // Starts the `Open` and `Drive` operations of `task`; `backend` opens the PTYs.
    void run(const Task& task, std::shared_ptr<const PtyBackend> backend) const;

  private:
    struct Impl;
    std::shared_ptr<Impl> impl_;
};

} // namespace loa::ui::terminal
