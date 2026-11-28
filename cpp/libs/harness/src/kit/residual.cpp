#include "loa/harness/kit/residual.hpp"

#include <algorithm>

namespace loa::harness::kit::residual {

namespace json = loa::runtime::json;

std::string attr_name(std::string_view harness) { return "x-" + std::string(harness); }

json::Object take(json::Object& obj, std::span<const std::string_view> keep) {
    json::Object gone;
    std::erase_if(obj, [&](auto& entry) {
        if (std::ranges::find(keep, std::string_view(entry.first)) != keep.end()) return false;
        gone.push_back(std::move(entry));
        return true;
    });
    return gone;
}

json::Object restore(const loa::core::Node& node, std::string_view harness) {
    auto text = node.str_attr(attr_name(harness));
    if (!text) return {};
    auto parsed = json::parse(*text);
    if (!parsed) return {};
    if (auto* obj = parsed->as_object()) return std::move(*obj);
    return {};
}

std::pair<std::string, std::vector<loa::core::Value>> join_parts(std::span<const std::string_view> parts,
                                                                 std::string_view separator) {
    std::pair<std::string, std::vector<loa::core::Value>> out;
    out.second.reserve(parts.size());
    for (std::size_t i = 0; i < parts.size(); ++i) {
        if (i > 0) out.first.append(separator);
        out.first.append(parts[i]);
        out.second.emplace_back(static_cast<std::int64_t>(parts[i].size()));
    }
    return out;
}

std::optional<std::vector<std::string_view>> split_parts(std::string_view text, std::string_view separator,
                                                         std::span<const loa::core::Value> lengths) {
    std::vector<std::string_view> pieces;
    pieces.reserve(lengths.size());
    std::string_view rest = text;
    for (std::size_t index = 0; index < lengths.size(); ++index) {
        if (index > 0) {
            if (!rest.starts_with(separator)) return std::nullopt;
            rest.remove_prefix(separator.size());
        }
        auto length = lengths[index].as_int();
        if (!length || *length < 0) return std::nullopt;
        const auto len = static_cast<std::size_t>(*length);
        // `str::is_char_boundary`: in range and not inside a UTF-8 sequence.
        if (len > rest.size()) return std::nullopt;
        if (len < rest.size() && (static_cast<unsigned char>(rest[len]) & 0xC0) == 0x80) return std::nullopt;
        pieces.push_back(rest.substr(0, len));
        rest.remove_prefix(len);
    }
    if (!rest.empty()) return std::nullopt;
    return pieces;
}

} // namespace loa::harness::kit::residual
