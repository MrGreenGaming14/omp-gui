#pragma once

// Opens login URLs in the user's browser through the platform opener (port of
// `account_support/opener.rs`).

#include <expected>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/account.hpp"

namespace loa::harness::account_support {

// `UrlOpener` over the platform's own opener: `xdg-open` on Linux and the BSDs, `open` on
// macOS, `rundll32 url.dll,FileProtocolHandler` on Windows (no shell, so `&` in the URL is
// not interpreted). Returns as soon as the opener started; it is reaped on a background thread.
class SystemUrlOpener final : public loa::contracts::UrlOpener {
  public:
    // Platform families with distinct opener commands.
    enum class Os { Windows, Mac, Other };

    [[nodiscard]] static Os current_os() noexcept;
    // Program and arguments that open `url`; the URL is always one argument.
    [[nodiscard]] static std::pair<std::string_view, std::vector<std::string>> command_for(Os os, std::string_view url);

    [[nodiscard]] std::expected<void, std::string> open_url(std::string_view url) const override;
};

} // namespace loa::harness::account_support
