#include <doctest/doctest.h>

#include "loa/settings/updates.hpp"

using namespace loa::settings;

namespace {
Version v(std::string_view s) { return *Version::parse(s); }
} // namespace

TEST_CASE("gate report verdicts and display") {
    GateReport report{"omp", v("1.0.0"), v("1.1.0"),
                      {CheckResult::flag(GateCheck::ToolSchemas, {}),
                       CheckResult::flag(GateCheck::Changelog, {"removed `foo`"}),
                       CheckResult::failed_to_run(GateCheck::RpcProtocol, "timeout")}};
    CHECK(!report.is_clean());
    REQUIRE(report.harmful().size() == 2);
    CHECK(report.harmful()[1]->details[0] == "check could not run: timeout");
    CHECK(report.to_string() ==
          "omp 1.0.0 -> 1.1.0\n"
          "  Tool schemas: pass\n"
          "  Changelog: HARMFUL\n"
          "    removed `foo`\n"
          "  RPC protocol version: HARMFUL\n"
          "    check could not run: timeout\n");
    CHECK(label(GateCheck::ContractTests) == "Adapter contract tests");
}

TEST_CASE("serde json forms round trip") {
    using loa::runtime::json::dump;
    HeldUpdate held{v("2.0.0"), GateReport{"app", v("1.0.0"), v("2.0.0"), {CheckResult::pass(GateCheck::ConfigSchema, {})}}, 7};
    CHECK(dump(to_json(held)) ==
          R"({"version":"2.0.0","report":{"product":"app","from":"1.0.0","to":"2.0.0","checks":[{"check":"config_schema","verdict":"pass","details":[]}]},"held_at_unix":7})");
    CHECK(*from_json<HeldUpdate>(to_json(held)) == held);

    Active active{v("1.2.3"), std::nullopt};
    CHECK(dump(to_json(active)) == R"({"current":"1.2.3","previous":null})");
    CHECK(*from_json<Active>(*loa::runtime::json::parse(R"({"current":"1.2.3"})")) == active);

    ProductKind kind{ProductKind::Kind::Harness, "claude"};
    CHECK(dump(to_json(kind)) == R"({"type":"harness","harness":"claude"})");
    CHECK(*from_json<ProductKind>(to_json(kind)) == kind);

    Release release{"omp", v("19.0.0"), "", std::nullopt,
                    Artifact{"omp.tgz", "https://x", 10, Checksum{Checksum::Algorithm::Sha512, "ab"},
                             ArchiveFormat::NpmTarball, std::nullopt}};
    CHECK(dump(to_json(release)) ==
          R"({"product":"omp","version":"19.0.0","notes":"","published_at":null,"artifact":{"name":"omp.tgz","url":"https://x","size":10,"checksum":{"algorithm":"sha512","hex":"ab"},"format":"npm_tarball","signature_url":null}})");
    CHECK(*from_json<Release>(to_json(release)) == release);

    PeerStanding peer{"n", std::nullopt, v("1.0.0"), Standing{Standing::Kind::Behind, 0, 2, 0},
                      {ProtocolStatus{"control", 1, 1, 2}}, true};
    CHECK(dump(to_json(peer.standing)) == R"({"kind":"behind","majors":0,"minors":2,"patches":0})");
    CHECK(*from_json<PeerStanding>(to_json(peer)) == peer);
    CHECK(!from_json<Standing>(*loa::runtime::json::parse(R"({"kind":"sideways"})")));
}
