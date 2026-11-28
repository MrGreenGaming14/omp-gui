#pragma once

// Office Open XML packages: zip parts (minizip), relationships and pugixml
// attribute helpers shared by the DOCX, XLSX and PPTX readers. Port of
// `loa-surfaces/src/docs/ooxml.rs`.

#include <cstdint>
#include <expected>
#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include <pugixml.hpp>

#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::docs::ooxml {

// Largest decompressed part accepted, a guard against zip bombs.
inline constexpr std::uint64_t MAX_PART_BYTES = std::uint64_t{512} << 20;

// Errors of the XML helpers; the package prefixes them with the part name.
template <typename T>
using XmlResult = std::expected<T, std::string>;

// A resolved package relationship.
struct Rel {
    std::string id;
    std::string kind;
    // Part path inside the zip, without a leading `/`.
    std::string target;
};

// The package zip of a `format` file. Borrows the bytes it was opened from.
class Package {
  public:
    [[nodiscard]] static Result<Package> open(std::span<const std::uint8_t> bytes, Format format);

    Package(Package&&) noexcept;
    Package& operator=(Package&&) noexcept;
    Package(const Package&) = delete;
    Package& operator=(const Package&) = delete;
    ~Package();

    [[nodiscard]] Format format() const noexcept { return format_; }
    [[nodiscard]] DocsError invalid(std::string message) const { return DocsError::invalid(format_, std::move(message)); }

    // Bytes of a part, `std::nullopt` when the zip has no such entry.
    [[nodiscard]] Result<std::optional<std::vector<std::uint8_t>>> read(std::string_view name);

    // A UTF-8 XML part handed to `parse`, with errors naming the part.
    template <typename T, typename F>
    [[nodiscard]] Result<std::optional<T>> parsed(std::string_view name, F&& parse) {
        auto xml = read_xml(name);
        if (!xml) return std::unexpected(std::move(xml.error()));
        if (!*xml) return std::optional<T>{};
        XmlResult<T> out = std::forward<F>(parse)(std::string_view(**xml));
        if (!out) return std::unexpected(invalid(std::string(name) + ": " + out.error()));
        return std::optional<T>(std::move(*out));
    }

    // The internal relationships of `part`, none when it has no rels part.
    [[nodiscard]] Result<std::vector<Rel>> rels(std::string_view part);

  private:
    struct Impl;
    Package(std::unique_ptr<Impl> impl, Format format) noexcept;
    // The part as UTF-8 text.
    [[nodiscard]] Result<std::optional<std::string>> read_xml(std::string_view name);

    std::unique_ptr<Impl> impl_;
    Format format_;
};

// Resolves a relationship target against the directory of its source part.
[[nodiscard]] std::string resolve_target(std::string_view source, std::string_view target);

// Parses `xml` (any byte order mark stripped) keeping whitespace-only text,
// with entities and character references resolved.
[[nodiscard]] XmlResult<std::unique_ptr<pugi::xml_document>> xml_reader(std::string_view xml);

// The name of `node` without its namespace prefix.
[[nodiscard]] std::string_view local_name(const pugi::xml_node& node);
[[nodiscard]] std::string_view local_name(const pugi::xml_attribute& attr);
// The namespace prefix of an attribute, empty when it has none.
[[nodiscard]] std::string_view prefix(const pugi::xml_attribute& attr);

// Calls `f` on every element of `xml` in document order.
[[nodiscard]] XmlResult<void> each_element(std::string_view xml,
                                           const std::function<XmlResult<void>(const pugi::xml_node&)>& f);

// Value of the first attribute whose name matches `matches`.
[[nodiscard]] std::optional<std::string_view> find_attr(const pugi::xml_node& e,
                                                        const std::function<bool(const pugi::xml_attribute&)>& matches);
// Attribute by local name, ignoring any prefix.
[[nodiscard]] std::optional<std::string_view> attr(const pugi::xml_node& e, std::string_view local);
// A whole number attribute (EMU, hundredths of a point).
[[nodiscard]] XmlResult<std::optional<double>> num_attr(const pugi::xml_node& e, std::string_view local);

} // namespace loa::surfaces::docs::ooxml
