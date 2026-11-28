#include <doctest/doctest.h>

#include <algorithm>

#include "loa/harness/account_support/opener.hpp"

using loa::harness::account_support::SystemUrlOpener;
using Os = SystemUrlOpener::Os;

TEST_CASE("account_support: only web URLs are opened") {
    CHECK_FALSE(SystemUrlOpener{}.open_url("file:///etc/passwd"));
    CHECK_FALSE(SystemUrlOpener{}.open_url("javascript:alert(1)"));
    CHECK(SystemUrlOpener{}.open_url("ftp://x").error() == "refusing to open a non-web URL: ftp://x");
}

TEST_CASE("account_support: Windows uses rundll32 with one URL argument") {
    const std::string url = "https://example.com/a?x=1&y=2";
    auto [program, args] = SystemUrlOpener::command_for(Os::Windows, url);
    CHECK(program == "rundll32.exe");
    CHECK(args == std::vector<std::string>{"url.dll,FileProtocolHandler", url});
    CHECK(std::ranges::none_of(args, [](const std::string& a) { return a == "cmd" || a == "CMD" || a == "/C" || a == "start"; }));
}

TEST_CASE("account_support: unix openers are unchanged") {
    const std::string url = "https://example.com/a?x=1&y=2";
    CHECK(SystemUrlOpener::command_for(Os::Mac, url) == std::pair<std::string_view, std::vector<std::string>>{"open", {url}});
    CHECK(SystemUrlOpener::command_for(Os::Other, url) == std::pair<std::string_view, std::vector<std::string>>{"xdg-open", {url}});
    CHECK(SystemUrlOpener::current_os() == Os::Other);
}
