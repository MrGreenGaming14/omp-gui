// The sandbox area (port of `loa-app/src/areas/sandbox/tests.rs`).

#include <algorithm>
#include <string>
#include <vector>

#include <doctest/doctest.h>

#include "areas/sandbox/sandbox.hpp"
#include "loa/app/app.hpp"
#include "loa/sandbox/tool.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::TestApp;
namespace sandbox = loa::app::areas::sandbox;

TEST_CASE("sandbox settings open in the panel and its tools reach sessions") {
    TestApp test;
    test.command("sandbox.settings");
    const auto& tabs = test.app->ctx.shell.tabs();
    CHECK(std::ranges::any_of(tabs, [](const auto& tab) { return tab.id == sandbox::TAB; }));
    CHECK(test.area<sandbox::State>().view() == sandbox::View::Settings);
    for (auto [key, value] : {std::pair{"ssh.hosts", "a, b c"}, std::pair{"docker.image", " "}, std::pair{"vm.disk", "/disk.img"}})
        test.send(sandbox::State::message(sandbox::msg::Edit{key, value}));
    const auto config = test.area<sandbox::State>().draft().config();
    CHECK_MESSAGE((config.ssh_hosts == std::vector<std::string>{"a", "b", "c"}), "the form writes the config");
    CHECK(config.vm_disk == std::filesystem::path("/disk.img"));
    CHECK_MESSAGE(config.docker_image == loa::sandbox::SandboxConfig{}.docker_image, "an empty image is the default one");
    test.session("hello");
    test.pump_feeds();
    CHECK_MESSAGE(test.area<sandbox::State>().runs().entries().empty(), "no run yet");
    CHECK_MESSAGE(std::ranges::any_of(test.app->ctx.app_tools, [](const auto& tool) { return tool.name == loa::sandbox::TASK_TOOL; }),
                  "sandbox_task is listed among the app tools");
}

TEST_CASE("sandbox settings save to the data directory and reload") {
    TestApp test;
    test.send(sandbox::State::message(sandbox::msg::Edit{"docker.image", "alpine:3"}));
    test.send(sandbox::State::message(sandbox::msg::Network{true}));
    test.send(sandbox::State::message(sandbox::msg::Save{}));
    CHECK(test.area<sandbox::State>().service().config().docker_image == "alpine:3");
    CHECK(test.area<sandbox::State>().service().config().docker_network);
    test.reopen();
    CHECK(test.area<sandbox::State>().service().config().docker_image == "alpine:3");
    CHECK(test.area<sandbox::State>().draft().network);
}

TEST_CASE("a finished run with a patch is listed with its diff") {
    TestApp test;
    namespace ev = loa::sandbox::event;
    loa::sandbox::SandboxRun run{7, "parent", "worker", loa::sandbox::SandboxKind::Local, "fix it", true, test.dir.path()};
    test.send(sandbox::State::message(sandbox::msg::Event{ev::Started{run}}));
    CHECK(test.area<sandbox::State>().runs().running() == 1);
    const std::string patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n";
    test.send(sandbox::State::message(sandbox::msg::Event{ev::Finished{7, "done", patch}}));
    CHECK(test.area<sandbox::State>().runs().running() == 0);
    test.send(sandbox::State::message(sandbox::msg::Select{7}));
    CHECK(test.area<sandbox::State>().selected() == 7);
    CHECK(test.area<sandbox::State>().patch().files().size() == 1);
}
