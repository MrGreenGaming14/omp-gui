#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <iterator>

#include "loa/core/sink.hpp"
#include "loa/harness/opencode/native.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

namespace {
namespace fs = std::filesystem;
using loa::runtime::json::Value;

Value read(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::string text((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    return loa::runtime::json::parse(text).value();
}

// serde_json `Value` equality: objects compare as maps.
bool same(const Value& a, const Value& b) {
    if (const auto* x = a.as_object()) {
        const auto* y = b.as_object();
        if (!y || x->size() != y->size()) return false;
        for (const auto& [k, v] : *x) {
            const auto* other = b.find(k);
            if (!other || !same(v, *other)) return false;
        }
        return true;
    }
    if (const auto* x = a.as_array()) {
        const auto* y = b.as_array();
        if (!y || x->size() != y->size()) return false;
        for (std::size_t i = 0; i < x->size(); ++i)
            if (!same((*x)[i], (*y)[i])) return false;
        return true;
    }
    if (a.is_number() && b.is_number()) return a.as_f64() == b.as_f64();
    return a == b;
}
} // namespace

TEST_CASE("opencode native exports reproduce their imports") {
    std::vector<fs::path> files;
    for (const auto& e : fs::directory_iterator(fs::path(LOA_HARNESS_FIXTURES) / "opencode"))
        if (e.path().extension() == ".json") files.push_back(e.path());
    REQUIRE_FALSE(files.empty());
    const loa::harness::opencode::OpenCodeNative codec;
    for (const auto& fixture : files) {
        CAPTURE(fixture.string());
        loa::core::MemorySink sink;
        auto imported = codec.import(fixture, sink);
        REQUIRE_MESSAGE(imported.has_value(), (imported ? std::string() : imported.error().message()));
        auto dir = loa::runtime::TempDir::create().value();
        const auto out = dir.path() / "out.json";
        auto exported = codec.export_(sink.dom(), out);
        REQUIRE_MESSAGE(exported.has_value(), (exported ? std::string() : exported.error().message()));
        CHECK(same(read(out), read(fixture)));
        auto value = loa::harness::opencode::export_value(sink.dom()).value();
        CHECK(same(value, read(fixture)));
    }
}
