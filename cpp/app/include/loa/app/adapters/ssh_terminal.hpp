#pragma once

// The terminal's remote PTY port over `loa::remote`'s SSH shells (port of
// `loa-app/src/adapters/ssh_terminal.rs`).

#include <memory>
#include <string>

#include "loa/view/terminal.hpp"

namespace loa::remote {
class RemoteService;
} // namespace loa::remote

namespace loa::app::adapters {

// `RemotePty` over the service's SSH shells.
class SshPty final : public view::terminal::port::RemotePty {
  public:
    explicit SshPty(std::shared_ptr<remote::RemoteService> service) : service_(std::move(service)) {}

    [[nodiscard]] contracts::Awaitable<view::terminal::port::Result<view::terminal::port::PtyChannel>> open_pty(
        std::string host, view::terminal::port::GridSize size) const override;

  private:
    std::shared_ptr<remote::RemoteService> service_;
};

} // namespace loa::app::adapters
