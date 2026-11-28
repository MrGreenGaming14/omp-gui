#pragma once

// Carries out a tab's surface work (`Task`) on the `loa::runtime` executor and answers with
// `Message`s on the GUI thread: the iced task runtime of `crates/loa-ui-surface/src/tab/pixel.rs`
// (`frame_stream`, `deliver_input`, `call`) and `document.rs`'s fetch. Inputs go out one at a
// time in order. Dropping the runner stops its work, like the Rust tab's `abort_on_drop`.
// Asio stays in the `.cpp`.

#include <functional>
#include <memory>

#include "loa/ui/surface/tab.hpp"

namespace loa::runtime {
class Executor;
}

namespace loa::ui::surface {

class Runner {
  public:
    // Runs a function on the GUI thread (e.g. `AsioBridge::post`); thread-safe.
    using Post = std::function<void(std::function<void()>)>;
    // Receives the tab's messages on the GUI thread.
    using Deliver = std::function<void(Message)>;

    Runner(SurfacePtr surface, loa::runtime::Executor& executor, Post post, Deliver deliver);
    // Stops streaming and sending; messages still queued for the GUI thread are dropped. Call on
    // the GUI thread.
    ~Runner();
    Runner(const Runner&) = delete;
    Runner& operator=(const Runner&) = delete;

    void run(Task task);

    // The state the runner's coroutines share (defined in the `.cpp`).
    struct Shared;

  private:
    std::shared_ptr<Shared> shared_;
};

} // namespace loa::ui::surface
