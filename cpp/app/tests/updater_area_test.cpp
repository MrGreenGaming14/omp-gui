// The updater area (port of `loa-app/src/areas/updater/tests.rs`): the Updates screen from the
// palette, and swaps and rollbacks run from the screen, deferred while a turn runs.

// `loa/p2p/node.hpp` declares a member named `emit`: before any Qt header.
#include "loa/p2p/node.hpp"

#include "areas/updater/updater.hpp"

#include <algorithm>
#include <filesystem>
#include <fstream>

#include <doctest/doctest.h>

#include "loa/runtime/json.hpp"
#include "loa/updater/stage.hpp"
#include "loa/updater/swap.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::TestApp;
namespace ua = loa::app::areas::updater;
namespace vu = loa::view::settings::updates;
namespace up = loa::updater;
namespace fs = std::filesystem;

namespace {

ua::Version v(std::string_view text) { return *ua::Version::parse(text); }

void put(const fs::path& path, std::string_view bytes) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << bytes;
}

// Stages `version` of `product` under `root` as the updater does after a download.
void stage(const fs::path& root, std::string_view product, std::string_view version) {
    const auto dir = up::stage::version_dir(root, product, v(version));
    const auto name = std::string(product) + "-" + std::string(version);
    const auto bytes = "#!/bin/sh\necho " + std::string(version) + "\n";
    put(dir / "artifact" / name, bytes);
    put(dir / "payload" / name, bytes);
    const up::StagedManifest manifest{std::string(product), v(version), name, std::string(64, '0'), bytes.size(), {}};
    put(dir / "staged.json", loa::runtime::json::dump(up::to_json(manifest)));
}

// The app with its startup feed checks over, as if they had finished.
struct UpdaterApp : TestApp {
    fs::path root = dir.path() / "loa" / "updates";

    UpdaterApp() { send(ua::State::message(ua::msg::Done{std::nullopt, std::nullopt})); }

    [[nodiscard]] ua::State& area() const { return TestApp::area<ua::State>(); }
    // Starts `intent` from the screen, as its buttons do.
    void intent(vu::Intent intent) {
        send(ua::State::message(ua::msg::Screen{ua::screen::Message::local(ua::screen::local::Start{std::move(intent)})}));
    }
};

} // namespace

TEST_CASE("updater: the Updates screen opens from the palette") {
    TestApp test;
    const auto titles = test.palette_titles();
    CHECK(std::ranges::find(titles, "Check for updates") != titles.end());
    test.command("updater.open");
    CHECK(test.app->ctx.screen() == area_id::UPDATER);
}

TEST_CASE("updater: a rollback from the screen keeps the version rolled back from") {
    UpdaterApp test;
    REQUIRE(test.area().service().has_value());
    stage(test.root, "omp", "1.0.0");
    stage(test.root, "omp", "1.1.0");
    up::Swapper swapper(test.root);
    up::TurnGate idle;
    REQUIRE(swapper.try_swap("omp", v("1.0.0"), std::nullopt, idle) == up::SwapOutcome::swapped({}, v("1.0.0")));
    REQUIRE(swapper.try_swap("omp", v("1.1.0"), std::nullopt, idle) == up::SwapOutcome::swapped(v("1.0.0"), v("1.1.0")));
    test.send(ua::State::message(ua::msg::Open{}));
    CHECK(test.area().managed().contains("omp"));

    test.intent(vu::intent::Rollback{"omp"});
    CHECK(test.notice() == "omp is now 1.0.0; new sessions use it");
    auto active = swapper.active("omp");
    REQUIRE(active);
    REQUIRE(active->has_value());
    CHECK((*active)->current == v("1.0.0"));
    CHECK((*active)->previous == v("1.1.0"));
    CHECK(test.area().managed().at("omp").string().find("1.0.0") != std::string::npos);

    // Nothing pending: the error shows on the product, not as a notice.
    test.app->ctx.clear_notice();
    test.intent(vu::intent::Swap{"omp"});
    CHECK(test.notice().empty());
}

TEST_CASE("updater: swap notices name the outcome and defer while a turn runs") {
    UpdaterApp test;
    REQUIRE(test.area().service().has_value());
    const auto& updater = *test.area().service();
    CHECK(ua::swap_note(updater, "omp", up::SwapOutcome::deferred()) == "omp swaps in once the running turn ends");
    CHECK_FALSE(ua::swap_note(updater, "omp", up::SwapOutcome::already_active()));
    CHECK(ua::swap_note(updater, "omp", up::SwapOutcome::swapped(v("1.0.0"), v("1.1.0"))) == "omp is now 1.1.0; new sessions use it");
    CHECK(ua::swap_note(updater, "app", up::SwapOutcome::swapped(v("0.1.0"), v("0.2.0"))) ==
          "loa 0.2.0 is installed; restart Loa to use it");

    // A rollback while a turn runs waits for it.
    stage(test.root, "omp", "1.0.0");
    stage(test.root, "omp", "1.1.0");
    up::Swapper swapper(test.root);
    up::TurnGate idle;
    REQUIRE(swapper.try_swap("omp", v("1.0.0"), std::nullopt, idle));
    REQUIRE(swapper.try_swap("omp", v("1.1.0"), std::nullopt, idle));
    loa::contracts::Prompt prompt;
    test.area().before_send(SessionId("busy"), prompt, test.app->ctx);
    CHECK(test.area().running() == 1);
    test.intent(vu::intent::Rollback{"omp"});
    CHECK(test.notice() == "omp swaps in once the running turn ends");
    CHECK((*swapper.active("omp"))->current == v("1.1.0"));
}

TEST_CASE("updater: waiting counts installed products with something for the user") {
    loa::settings::Overview overview;
    loa::settings::ProductStatus idle;
    idle.current = v("1.0.0");
    loa::settings::ProductStatus restart = idle;
    restart.restart_required = true;
    loa::settings::ProductStatus pending = idle;
    pending.pending_swap = v("1.1.0");
    loa::settings::ProductStatus missing;
    missing.pending_swap = v("1.1.0");
    overview.products = {idle, restart, pending, missing};
    CHECK(ua::waiting(overview) == 2);
}

TEST_CASE("updater: peers without a hello are left out of the standings") {
    loa::p2p::PeerInfo silent;
    silent.key = loa::trust::NodeIdentity::from_secret_bytes(std::array<std::uint8_t, 32>{1}).public_key();
    loa::p2p::PeerInfo behind = silent;
    behind.key = loa::trust::NodeIdentity::from_secret_bytes(std::array<std::uint8_t, 32>{2}).public_key();
    behind.app_version = "v0.0.9";
    behind.protocols = loa::p2p::local_protocols();
    const auto standings = ua::standings(v("0.1.0"), {silent, behind});
    REQUIRE(standings.size() == 1);
    CHECK(standings[0].node == behind.key.to_string());
    CHECK(standings[0].app_version == v("0.0.9"));
    CHECK(standings[0].standing.kind == loa::settings::Standing::Kind::Behind);
}
