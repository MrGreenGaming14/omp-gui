#include "loa/harness/omp/native.hpp"

#include <cerrno>
#include <cstring>
#include <fstream>
#include <iterator>

#include "internal.hpp"
#include "loa/harness/native.hpp"

namespace loa::harness::omp::native {

namespace fs = std::filesystem;
using loa::contracts::AdapterError;
using loa::contracts::Result;

namespace {

// `err` worded like Rust's `io::Error` display.
AdapterError io_error(int err) {
    const ErrorCode code = err == ENOENT ? ErrorCode::not_found
                           : err == EACCES || err == EPERM ? ErrorCode::permission_denied
                                                            : ErrorCode::io;
    return AdapterError::Io{loa::Error(code, std::string(std::strerror(err)) + " (os error " + std::to_string(err) + ")")};
}

} // namespace

Result<> import(const fs::path& path, loa::core::PatchSink& sink, std::string_view node) {
    errno = 0;
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(io_error(errno ? errno : EIO));
    std::string text((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    if (in.bad()) return std::unexpected(io_error(errno ? errno : EIO));
    return import_str(text, sink, node);
}

Result<> import_str(std::string_view text, loa::core::PatchSink& sink, std::string_view node) {
    return detail::run_import(text, sink, node);
}

Result<std::string> export_string(const loa::core::Dom& dom) { return detail::run_export(dom); }

Result<> OmpNative::import(const fs::path& path, loa::core::PatchSink& sink) const {
    return native::import(path, sink, "");
}

Result<> OmpNative::export_(const loa::core::Dom& dom, const fs::path& out) const {
    auto text = export_string(dom);
    if (!text) return std::unexpected(std::move(text.error()));
    errno = 0;
    std::ofstream file(out, std::ios::binary | std::ios::trunc);
    if (!file) return std::unexpected(io_error(errno ? errno : EIO));
    file.write(text->data(), static_cast<std::streamsize>(text->size()));
    file.close();
    if (!file) return std::unexpected(io_error(errno ? errno : EIO));
    return {};
}

std::vector<kit::NativeSession> OmpNative::discover(const fs::path& home) const {
    return loa::harness::native::discover_omp(home);
}

} // namespace loa::harness::omp::native
