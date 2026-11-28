#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <map>

#include "loa/runtime/fs.hpp"
#include "loa/updater/model.hpp"
#include "loa/updater/service.hpp"
#include "loa/updater/stage.hpp"
#include "loa/updater/state.hpp"

using namespace loa::updater;

namespace {

Path golden(std::string_view name) { return Path(LOA_GOLDEN_DIR) / "updater" / std::string(name); }
std::string bytes(const Path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), {}};
}
loa::runtime::json::Value read(std::string_view name) { return *loa::runtime::json::parse(bytes(golden(name))); }
template <typename T>
void roundtrip(std::string_view name, std::expected<T, std::string> (*decode)(const loa::runtime::json::Value&),
               loa::runtime::json::Value (*encode)(const T&)) {
    const auto input = bytes(golden(name));
    auto parsed = loa::runtime::json::parse(input);
    REQUIRE(parsed);
    auto value = decode(*parsed);
    REQUIRE(value);
    CHECK(loa::runtime::json::dump_pretty(encode(*value)) == input);
}

} // namespace

TEST_CASE("Rust staged.json and active.json round-trip byte-identically") {
    roundtrip<StagedManifest>("staged.json", staged_manifest_from_json, to_json);
    roundtrip<StagedManifest>("staged-unsigned.json", staged_manifest_from_json, to_json);
    for (const char* name : {"active.json", "active-first.json"}) {
        const auto input = bytes(golden(name));
        auto active = loa::settings::from_json<loa::settings::Active>(read(name));
        REQUIRE(active);
        CHECK(loa::runtime::json::dump_pretty(loa::settings::to_json(*active)) == input);
    }
}

TEST_CASE("Rust feeds.json round-trips byte-identically") {
        auto input = read("feeds.json");
        REQUIRE(input.as_object());
        loa::runtime::json::Object encoded;
        for (const auto& [key, value] : *input.as_object()) {
            auto cached = cached_feed_from_json(value);
            REQUIRE(cached);
            encoded.emplace_back(key, to_json(*cached));
        }
        CHECK(loa::runtime::json::dump_pretty(encoded) == bytes(golden("feeds.json")));
}

TEST_CASE("Rust state.json round-trips byte-identically") {
    roundtrip<UpdaterState>("state.json", updater_state_from_json, to_json);
}

TEST_CASE("Rust product catalog round-trips byte-identically") {
    const auto input = read("products.json");
    REQUIRE(input.as_array());
    loa::runtime::json::Array encoded;
    for (const auto& item : *input.as_array()) {
        auto product = product_spec_from_json(item);
        REQUIRE(product);
        encoded.push_back(to_json(*product));
    }
    CHECK(loa::runtime::json::dump_pretty(encoded) == bytes(golden("products.json")));
    auto catalog = default_products(*Version::parse("0.1.0"));
    loa::runtime::json::Array defaults;
    for (const auto& product : catalog) defaults.push_back(to_json(product));
    CHECK(loa::runtime::json::dump_pretty(defaults) == bytes(golden("products.json")));
}

TEST_CASE("Rust-written updater root is read and its signed previous version can be rolled back") {
    const auto root = golden("root");
    auto active = Swapper(root).active("app");
    REQUIRE(active);
    REQUIRE(*active);
    CHECK((**active).current == *Version::parse("0.2.0"));
    CHECK((**active).previous == *Version::parse("0.1.0"));
    auto s = stage::staged(root, "app", *Version::parse("0.2.0"));
    REQUIRE(s);
    REQUIRE(*s);
    CHECK((*s)->manifest.sha256 == to_hex(*signature::artifact_digest((*s)->artifact())));
    auto trusted = signature::parse_public_key(bytes(golden("sig/trusted.hex")));
    REQUIRE(trusted);
    CHECK(signature::verify_staged(**s, std::span<const VerifyingKey>(&*trusted, 1)).has_value());
    auto state = UpdaterState::load(root);
    REQUIRE(state);
    CHECK(state->last_check_unix.at("app") == 1'760'000'000);
    // Read only: the golden tree remains immutable for the manifest hash test.
}
