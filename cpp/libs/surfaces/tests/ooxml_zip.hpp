#pragma once

// Test helper: a deflated zip of named parts, built with minizip through a temp file.

#include <cstdint>
#include <fstream>
#include <iterator>
#include <string>
#include <utility>
#include <vector>

#include <minizip/zip.h>

#include "loa/runtime/tempfile.hpp"

namespace loa::surfaces::testing {

inline std::vector<std::uint8_t> zip_parts(const std::vector<std::pair<std::string, std::string>>& parts) {
    auto dir = loa::runtime::TempDir::create();
    if (!dir) return {};
    const auto path = dir->path() / "parts.zip";
    zipFile zip = zipOpen64(path.c_str(), APPEND_STATUS_CREATE);
    if (zip == nullptr) return {};
    for (const auto& [name, data] : parts) {
        zip_fileinfo info{};
        zipOpenNewFileInZip64(zip, name.c_str(), &info, nullptr, 0, nullptr, 0, nullptr, Z_DEFLATED, Z_DEFAULT_COMPRESSION, 0);
        zipWriteInFileInZip(zip, data.data(), static_cast<unsigned>(data.size()));
        zipCloseFileInZip(zip);
    }
    zipClose(zip, nullptr);
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

} // namespace loa::surfaces::testing
