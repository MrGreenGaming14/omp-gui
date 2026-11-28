#include "loa/surfaces/hub/mcp.hpp"

#include <limits>

#include "loa/core/value.hpp"

namespace loa::surfaces::hub {

namespace json = loa::runtime::json;

namespace {

// A `u32` field as serde reads it; none when missing or not a fitting integer.
std::optional<std::uint32_t> u32_field(const json::Value& object, std::string_view key) {
    const auto* value = object.find(key);
    if (!value) return std::nullopt;
    const auto n = value->as_u64();
    if (!n || *n > std::numeric_limits<std::uint32_t>::max()) return std::nullopt;
    return static_cast<std::uint32_t>(*n);
}

std::string resource_error(std::string_view what) {
    return "resource is not an object with `uri` and `name`: " + std::string(what);
}

} // namespace

json::Value resource(const cs::SurfaceInfo& info) {
    json::Object meta{{"width", std::uint64_t{info.width}}, {"height", std::uint64_t{info.height}},
                      {"mode", std::string(core::as_str(info.mode))}};
    json::Object out{{"uri", info.uri.as_str()}, {"name", info.name}, {"_meta", json::Object{{std::string(META_KEY), std::move(meta)}}}};
    if (info.mime) out.emplace_back("mimeType", *info.mime);
    return out;
}

std::expected<std::optional<cs::SurfaceInfo>, std::string> parse_resource(const json::Value& resource) {
    if (!resource.is_object()) return std::unexpected(resource_error("invalid type: expected struct ResourceJson"));
    const auto* uri_field = resource.find("uri");
    if (!uri_field) return std::unexpected(resource_error("missing field `uri`"));
    if (!uri_field->as_string()) return std::unexpected(resource_error("invalid type: expected a string"));
    const auto* name_field = resource.find("name");
    if (!name_field) return std::unexpected(resource_error("missing field `name`"));
    if (!name_field->as_string()) return std::unexpected(resource_error("invalid type: expected a string"));
    std::optional<std::string> mime;
    if (const auto* m = resource.find("mimeType"); m && !m->is_null()) {
        if (!m->as_string()) return std::unexpected(resource_error("invalid type: expected a string"));
        mime = *m->as_string();
    }
    const json::Value* meta_map = nullptr;
    if (const auto* m = resource.find("_meta"); m && !m->is_null()) {
        if (!m->is_object()) return std::unexpected(resource_error("invalid type: expected a map"));
        meta_map = m;
    }

    const std::string& text = *uri_field->as_string();
    if (!text.starts_with(std::string(cs::SCHEME) + "://")) return std::optional<cs::SurfaceInfo>{};
    auto uri = cs::SurfaceUri::parse(text);
    if (!uri) return std::unexpected(uri.error().message());
    const auto* meta = meta_map ? meta_map->find(META_KEY) : nullptr;
    std::optional<std::uint32_t> width, height;
    const std::string* mode_text = nullptr;
    if (meta && meta->is_object()) {
        width = u32_field(*meta, "width");
        height = u32_field(*meta, "height");
        if (const auto* m = meta->find("mode")) mode_text = m->as_string();
    }
    if (!width || !height || !mode_text) {
        return std::unexpected("surface resource " + text + " lacks `_meta.\"omp/surface\"` with width, height and mode");
    }
    const auto mode = core::parse<cs::SurfaceMode>(*mode_text);
    if (!mode) return std::unexpected("unknown surface mode `" + *mode_text + "`");
    return cs::SurfaceInfo{std::move(*uri), *name_field->as_string(), *width, *height, *mode, std::move(mime)};
}

} // namespace loa::surfaces::hub
