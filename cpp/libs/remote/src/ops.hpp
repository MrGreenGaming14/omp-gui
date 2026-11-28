#pragma once

// Internal: the SSH operations the service runs on a pooled session.

#include <chrono>
#include <cstddef>
#include <memory>
#include <string>

#include "loa/remote/cancel.hpp"
#include "loa/remote/error.hpp"
#include "loa/remote/exec.hpp"
#include "loa/remote/pty.hpp"
#include "session.hpp"

namespace loa::remote::detail {

// Runs `command` on a new session channel of `link`.
[[nodiscard]] Result<ExecOutput> run(Link& link, const std::string& host, const std::string& command, std::size_t max_output,
                                     std::chrono::milliseconds timeout, const CancelToken& cancel);

// Opens a PTY running the login shell on `session`.
[[nodiscard]] Result<Shell> open_shell(std::shared_ptr<Session> session, std::uint16_t cols, std::uint16_t rows);

} // namespace loa::remote::detail
