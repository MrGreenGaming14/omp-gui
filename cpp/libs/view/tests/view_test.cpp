#include <doctest/doctest.h>

#include <fstream>
#include <iterator>
#include <string>

#include "api_listing.hpp"
#include "loa/view/panel.hpp"
#include "loa/view/session.hpp"
#include "loa/view/settings_models.hpp"
#include "loa/view/settings_peers.hpp"
#include "loa/view/version.hpp"

using namespace loa::view;

TEST_CASE("the listing of include/loa/view matches api.txt at VERSION") {
    std::ifstream in(LOA_VIEW_API_TXT, std::ios::binary);
    REQUIRE(in);
    const std::string saved{std::istreambuf_iterator<char>(in), {}};
    CHECK(saved.starts_with("version = 4.0.0\n"));
    const auto result = api::gate(saved, api::render(LOA_VIEW_INCLUDE_DIR), VERSION, false);
    INFO(result.message);
    CHECK(result.ok);
    CHECK_FALSE(result.write);
}

TEST_CASE("the gate follows the semver rules") {
    const std::string body = "struct a::A\n  int x\nstruct a::B\n";
    const std::string saved = "version = 4.0.0\n" + body;
    const std::string removed = "struct a::A\n";
    const std::string added = body + "struct a::C\n";

    CHECK_FALSE(api::gate(saved, removed, "4.0.0", true).ok);       // changed without a bump
    CHECK_FALSE(api::gate(saved, removed, "4.1.0", true).ok);       // removal needs a major bump
    CHECK(api::gate(saved, removed, "5.0.0", true).write);          // ... which is enough
    CHECK_FALSE(api::gate(saved, added, "4.0.1", true).ok);         // addition needs a minor bump
    CHECK(api::gate(saved, added, "4.1.0", true).ok);
    CHECK_FALSE(api::gate(saved, added, "4.1.0", false).ok);        // stale without UPDATE_VIEW_API
    CHECK_FALSE(api::gate(saved, body, "3.9.0", true).ok);          // versions only go up
    CHECK(api::gate("version = 0.2.0\n" + body, removed, "0.3.0", true).ok); // 0.x: minor is major
}

TEST_CASE("composer model options and the unattended policy by decision kind") {
    using session::composer::ModelOption;
    CHECK(ModelOption{"m", "Sonnet", "Anthropic", "plan"}.to_string() == "plan · Sonnet (Anthropic)");
    CHECK(ModelOption{"m", "Sonnet", "Anthropic", {}}.to_string() == "Sonnet (Anthropic)");

    session::decisions::UnattendedView policy;
    policy.for_kind(loa::core::DecisionKind::Plan) = session::decisions::DecisionDefaultView{30, "approve", {}};
    CHECK(policy.plan->answer == "approve");
    CHECK_FALSE(std::as_const(policy).for_kind(loa::core::DecisionKind::Form));
}

TEST_CASE("names and ids follow omp's rules") {
    using namespace settings::models;
    CHECK((is_env_var_name("OPENAI_API_KEY") && is_env_var_name("_X1")));
    CHECK_FALSE((is_env_var_name("sk-ant-api03") || is_env_var_name("1KEY") || is_env_var_name("")));
    CHECK((is_provider_id("local-llm.v1_x") && !is_provider_id("my provider") && !is_provider_id("")));
    CHECK(ModelRef{"openai", "gpt-5"}.selector() == "openai/gpt-5");
}

TEST_CASE("peer versions, ages and grant countdowns") {
    using namespace settings::peers;
    Peer peer{.key = "abcdefghijklmnop", .app_version = "v1.2.3-beta+5"};
    CHECK(peer.short_key() == "abcdefghijkl");
    CHECK(peer.version_relation("1.2.4") == VersionRelation::Behind);
    CHECK(peer.version_relation("1.2.3") == VersionRelation::Current);
    CHECK(peer.version_relation("1.2") == VersionRelation::Unknown);
    peer.last_seen = 1000;
    CHECK(peer.last_seen_text(1000 + 7199) == "1h ago");

    Grant grant{.direction = GrantDirection::Given, .expires_at = 1000 + 3601, .renews_at = 1000};
    CHECK(grant.expiry_text(1000) == "expires in 2h; renewal due");
    grant.ended = true;
    CHECK(grant.expiry_text(1000) == "revoked");
}

TEST_CASE("tool rules") {
    using namespace panel;
    const auto mcp = ToolId::mcp("fs", "read");
    CHECK(ToolRule{tool_rule::Server{"fs"}}.allows(mcp));
    CHECK_FALSE(ToolRule{tool_rule::AllBuiltin{}}.allows(mcp));
    CHECK(ToolRule{tool_rule::AllBuiltin{}}.allows(ToolId::builtin("bash")));
    CHECK(ToolId::builtin("z") < mcp); // Builtin orders before Mcp, as in Rust
    CHECK_FALSE(PeerTools{}.allows(mcp));
}
