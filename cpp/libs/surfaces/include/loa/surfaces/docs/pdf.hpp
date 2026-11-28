#pragma once

// PDF pages rendered and read with QtPdf (`QPdfDocument`). Port of
// `loa-surfaces/src/docs/pdf.rs`. Qt stays in `src/docs/pdf.cpp`.

#include <cstdint>
#include <memory>
#include <span>
#include <string>
#include <vector>

#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::docs {

class PdfDoc final : public Paged {
  public:
    // Parses a PDF file. Encrypted files that need a password are rejected.
    [[nodiscard]] static Result<std::unique_ptr<PdfDoc>> parse(std::span<const std::uint8_t> bytes);
    ~PdfDoc() override;

    // Text of every page, one entry per page.
    [[nodiscard]] std::vector<std::string> text() const;

    [[nodiscard]] std::size_t page_count() const override;
    [[nodiscard]] Result<PageSize> page_size(std::size_t index) const override;
    [[nodiscard]] Result<Rgba> render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const override;
    [[nodiscard]] std::optional<Result<json::Value>> query(const DataQuery& query) const override;

    struct Impl;

  private:
    explicit PdfDoc(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
};

} // namespace loa::surfaces::docs
