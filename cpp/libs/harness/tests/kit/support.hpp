#pragma once

#include <asio.hpp>

#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>

#include "loa/runtime/executor.hpp"

namespace loa::harness::test {

// Runs `aw` on `exec` and blocks the test thread for its result.
template <typename T>
T block_on(loa::runtime::Executor& exec, asio::awaitable<T> aw) {
    return asio::co_spawn(exec.context(), std::move(aw), asio::use_future).get();
}

inline std::string read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

inline std::filesystem::path golden(const std::string& rel) { return std::filesystem::path(LOA_GOLDEN_DIR) / rel; }

} // namespace loa::harness::test
