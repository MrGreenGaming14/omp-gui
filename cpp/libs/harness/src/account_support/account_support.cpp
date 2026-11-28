#include "loa/harness/account_support/account_support.hpp"

#include <string>

#include "unicode.hpp"

namespace loa::harness::account_support {

using loa::settings::models::Error;

loa::settings::models::Result<> validate_model_name(std::string_view field, std::string_view name) {
    if (name.empty()) return std::unexpected(Error::invalid(std::string(field), "model name is empty"));
    for (std::size_t i = 0; i < name.size();) {
        const char32_t c = detail::next_char(name, i);
        if (detail::is_whitespace(c) || detail::is_control(c))
            return std::unexpected(Error::invalid(std::string(field), "model name contains whitespace or control characters"));
    }
    return {};
}

} // namespace loa::harness::account_support
