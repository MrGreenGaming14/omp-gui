#include "loa/settings/models/selector.hpp"

#include <algorithm>

#include "text_util.hpp"

namespace loa::settings::models::selector {

namespace {
bool is_thinking_level(std::string_view level) {
    return std::ranges::find(THINKING_LEVELS, level) != THINKING_LEVELS.end();
}
bool ascii_alnum(char c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9'); }
} // namespace

Result<> validate_role_name(std::string_view role) {
    if (role.empty() || !std::ranges::all_of(role, [](char c) { return ascii_alnum(c) || c == '_' || c == '-'; })) {
        return std::unexpected(Error::invalid("role", "`" + std::string(role) + "` must be letters, digits, `_` or `-`"));
    }
    return {};
}

Result<Selector> Selector::parse(std::string_view text) {
    text = detail::trim(text);
    if (text.empty()) {
        return std::unexpected(Error::invalid("selector", "empty selector"));
    }
    std::string_view body = text;
    std::optional<std::string> thinking;
    if (const auto colon = text.rfind(':'); colon != std::string_view::npos && is_thinking_level(text.substr(colon + 1))) {
        body = text.substr(0, colon);
        thinking = std::string(text.substr(colon + 1));
    }
    Selector selector = body.starts_with('@') ? Selector(Kind::Role, std::string(body.substr(1)), std::move(thinking))
                                              : Selector(Kind::Model, std::string(body), std::move(thinking));
    if (auto ok = selector.validate(); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    return selector;
}

std::optional<std::string_view> Selector::model_name() const {
    if (kind_ == Kind::Model) {
        return std::string_view(name_);
    }
    return std::nullopt;
}

Result<> Selector::validate() const {
    if (kind_ == Kind::Role) {
        if (auto ok = validate_role_name(name_); !ok) {
            return ok;
        }
    }
    if (name_.empty() ||
        detail::any_char(name_, [](std::uint32_t c) { return detail::is_whitespace(c) || detail::is_control(c) || c == ','; })) {
        return std::unexpected(Error::invalid("selector", "`" + name_ + "` must be non-empty without spaces or commas"));
    }
    if (thinking_ && !is_thinking_level(*thinking_)) {
        return std::unexpected(Error::invalid("selector", "unknown thinking level `" + *thinking_ + "`"));
    }
    return {};
}

std::string Selector::to_string() const {
    std::string out = kind_ == Kind::Role ? "@" + name_ : name_;
    if (thinking_) {
        out += ':';
        out += *thinking_;
    }
    return out;
}

std::optional<std::pair<std::string_view, std::string_view>> ModelRef::split(std::string_view selector) {
    const auto slash = selector.find('/');
    if (slash == std::string_view::npos || slash == 0 || slash + 1 == selector.size()) {
        return std::nullopt;
    }
    return std::pair{selector.substr(0, slash), selector.substr(slash + 1)};
}

std::optional<ModelRef> ModelRef::parse(std::string_view selector) {
    auto parts = split(selector);
    if (!parts) {
        return std::nullopt;
    }
    return ModelRef{std::string(parts->first), std::string(parts->second)};
}

} // namespace loa::settings::models::selector
