#pragma once

// Unpacker strategies writing into one entry sink that keeps every entry inside the payload.

#include <cstddef>
#include <memory>

#include "loa/settings/updates.hpp"
#include "loa/updater/error.hpp"

namespace loa::updater::unpack {

// Unpacks an artifact into a payload directory it creates.
class Unpacker {
  public:
    virtual ~Unpacker() = default;
    [[nodiscard]] virtual Result<> unpack(const Path& artifact, const Path& payload) const = 0;
};

// A gzipped tar, dropping the first `strip` components (npm tarballs nest under `package/`).
class TarGz final : public Unpacker {
  public:
    explicit TarGz(std::size_t strip = 0) : strip(strip) {}
    [[nodiscard]] Result<> unpack(const Path& artifact, const Path& payload) const override;
    std::size_t strip;
};

class Zip final : public Unpacker {
  public:
    [[nodiscard]] Result<> unpack(const Path& artifact, const Path& payload) const override;
};

// A single executable file kept under its own name.
class Raw final : public Unpacker {
  public:
    [[nodiscard]] Result<> unpack(const Path& artifact, const Path& payload) const override;
};

// The strategy for `format`.
[[nodiscard]] std::unique_ptr<Unpacker> unpacker(loa::settings::ArchiveFormat format);

} // namespace loa::updater::unpack
