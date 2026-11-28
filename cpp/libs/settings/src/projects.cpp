#include "loa/settings/projects.hpp"

namespace loa::settings {

FileNote FileNote::parse(std::string_view text) {
    auto at = text.find(": ");
    if (at == std::string_view::npos) return {std::string(text), {}};
    return {std::string(text.substr(0, at)), std::string(text.substr(at + 2))};
}

std::string FileNote::encode() const { return note.empty() ? path : path + ": " + note; }

} // namespace loa::settings
