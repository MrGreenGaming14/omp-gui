#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <sstream>

#include "loa/runtime/error.hpp"

TEST_CASE("no public header includes an Asio or yyjson header") {
    int count = 0;
    for (const auto& entry : std::filesystem::directory_iterator(LOA_RUNTIME_INCLUDE_DIR)) {
        std::ifstream in(entry.path());
        std::string line;
        while (std::getline(in, line)) {
            if (!line.starts_with("#include")) continue;
            CHECK_MESSAGE(line.find("asio") == std::string::npos, entry.path().string(), ": ", line);
            CHECK_MESSAGE(line.find("yyjson") == std::string::npos, entry.path().string(), ": ", line);
        }
        ++count;
    }
    CHECK(count >= 6);
}

TEST_CASE("loa::Error chains sources") {
    loa::Error root(loa::ErrorCode::io, "disk full");
    auto wrapped = root.context("saving settings");
    CHECK(wrapped.code() == loa::ErrorCode::io);
    REQUIRE(wrapped.source());
    CHECK(*wrapped.source() == root);
    CHECK(wrapped.to_string() == "saving settings: disk full");
    CHECK(loa::to_string(loa::ErrorCode::parse) == "parse");
}
