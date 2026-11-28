#pragma once

#include <array>
#include <cstdint>
#include <iomanip>
#include <mutex>
#include <random>
#include <sstream>
#include <string>
#include <string_view>

namespace loa::harness::opencode::ids {
inline std::string ascending(std::string_view prefix, std::int64_t now_ms) {
    static std::mutex mutex;
    static std::int64_t last_ms = 0;
    static std::uint64_t counter = 0;
    std::uint64_t serial;
    {
        std::lock_guard lock(mutex);
        if (last_ms != now_ms) { last_ms = now_ms; counter = 0; }
        serial = ++counter;
    }
    const auto value = ((now_ms < 0 ? 0ULL : static_cast<std::uint64_t>(now_ms)) * 0x1000ULL + serial) & 0xffffffffffffULL;
    constexpr std::string_view alphabet = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    thread_local std::mt19937_64 rng(std::random_device{}());
    std::string id;
    id.reserve(prefix.size() + 27);
    id.append(prefix).append("_");
    constexpr char hex[] = "0123456789abcdef";
    for (int shift = 44; shift >= 0; shift -= 4) id += hex[(value >> shift) & 15];
    for (int i = 0; i < 14; ++i) id += alphabet[rng() % alphabet.size()];
    return id;
}
} // namespace loa::harness::opencode::ids
