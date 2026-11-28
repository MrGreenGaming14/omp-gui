#include "loa/settings/models/config.hpp"

#include <algorithm>
#include <set>

#include <asio/awaitable.hpp>
#include <asio/any_io_executor.hpp>

#include "text_util.hpp"

namespace loa::settings::models::config {

using yaml::Yaml;

namespace {

struct ScopedKey {
    std::string_view key;
    std::string_view value_key;
};

constexpr std::array<ScopedKey, 3> SCOPED_KEYS = {{
    {"enabledModels", "models"},
    {"enabledProviders", "providers"},
    {"disabledProviders", "providers"},
}};

constexpr std::string_view PROVIDER_ORDER = "modelProviderOrder";

constexpr std::array<std::string_view, 4> PATH_KEYS = {"path", "paths", "pathPrefix", "pathPrefixes"};
constexpr std::array<std::string_view, 5> VALUE_KEYS = {"models", "providers", "values", "items", "patterns"};

const ScopedKey& metadata(ScopedList list) { return SCOPED_KEYS[static_cast<std::size_t>(list)]; }

bool bad_selector_char(std::uint32_t c) { return detail::is_whitespace(c) || detail::is_control(c) || c == ','; }

Result<> validate_value(std::string_view field, std::string_view value) {
    if (detail::trim(value).empty() || detail::any_char(value, detail::is_control) || value != detail::trim(value)) {
        return std::unexpected(Error::invalid(std::string(field), "`" + std::string(value) + "` is not a valid entry"));
    }
    return {};
}

std::optional<ScopedEntry> parse_scoped(const Yaml& item) {
    if (const auto* s = item.as_str()) {
        return ScopedEntry::everywhere(*s);
    }
    if (!item.as_hash()) {
        return std::nullopt;
    }
    auto strings = [&](std::string_view key, std::vector<std::string>& out) {
        const Yaml* v = item.get(key);
        if (!v) {
            return;
        }
        if (const auto* s = v->as_str()) {
            out.push_back(*s);
        } else if (const auto* a = v->as_vec()) {
            for (const auto& x : *a) {
                if (const auto* xs = x.as_str()) {
                    out.push_back(*xs);
                }
            }
        }
    };
    std::vector<std::string> paths;
    std::vector<std::string> values;
    for (auto k : PATH_KEYS) {
        strings(k, paths);
    }
    for (auto k : VALUE_KEYS) {
        strings(k, values);
    }
    if (paths.empty() || values.empty()) {
        return std::nullopt;
    }
    return ScopedEntry::scoped(std::move(paths), std::move(values));
}

Result<Yaml> scoped_yaml(ScopedList list, const ScopedEntry& entry) {
    const auto key = metadata(list).key;
    if (entry.kind == ScopedEntry::Kind::Everywhere) {
        if (auto ok = validate_value(key, entry.value); !ok) {
            return std::unexpected(std::move(ok.error()));
        }
        return yaml::ystr(entry.value);
    }
    if (entry.paths.empty() || entry.values.empty()) {
        return std::unexpected(Error::invalid(std::string(key), "a path scoped entry needs at least one path and one value"));
    }
    for (const auto* list_values : {&entry.paths, &entry.values}) {
        for (const auto& v : *list_values) {
            if (auto ok = validate_value(key, v); !ok) {
                return std::unexpected(std::move(ok.error()));
            }
        }
    }
    std::pair<std::string, Yaml> path_yaml =
        entry.paths.size() == 1 ? std::pair<std::string, Yaml>{"path", yaml::ystr(entry.paths[0])}
                                : std::pair<std::string, Yaml>{"paths", yaml::ystrs(entry.paths)};
    std::vector<std::pair<std::string, Yaml>> pairs;
    pairs.push_back(std::move(path_yaml));
    pairs.emplace_back(std::string(metadata(list).value_key), yaml::ystrs(entry.values));
    return yaml::ymap(std::move(pairs));
}

std::string join_selectors(std::span<const Selector> selectors, std::string_view sep) {
    std::string out;
    for (std::size_t i = 0; i < selectors.size(); ++i) {
        if (i > 0) {
            out += sep;
        }
        out += selectors[i].to_string();
    }
    return out;
}

} // namespace

bool is_kind_role(std::string_view role) noexcept { return std::ranges::find(KIND_ROLES, role) != KIND_ROLES.end(); }

Result<ChainKey> ChainKey::parse(std::string_view text) {
    ChainKey key;
    if (text.ends_with("/*")) {
        key = provider(std::string(text.substr(0, text.size() - 2)));
    } else if (text.find('/') != std::string_view::npos) {
        key = model(std::string(text));
    } else {
        key = role(std::string(text));
    }
    if (auto ok = key.validate(); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    return key;
}

Result<> ChainKey::validate() const {
    if (kind == Kind::Role) {
        return selector::validate_role_name(value);
    }
    if (value.empty() || detail::any_char(value, bad_selector_char)) {
        return std::unexpected(Error::invalid("fallback chain key", "`" + value + "` is not a selector"));
    }
    return {};
}

std::string ChainKey::yaml_key() const { return kind == Kind::Provider ? value + "/*" : value; }

Result<OmpConfig> OmpConfig::load(const std::filesystem::path& path) {
    auto loaded = load_snapshot(path);
    if (!loaded) {
        return std::unexpected(std::move(loaded.error()));
    }
    auto& [snapshot, text] = *loaded;
    auto doc = yaml::YamlDocument::parse(text ? *text : std::string_view(), path.string());
    if (!doc) {
        return std::unexpected(std::move(doc.error()));
    }
    return OmpConfig(std::move(*doc), std::move(snapshot));
}

Result<> OmpConfig::save() {
    if (auto ok = save_snapshot(snapshot_, doc_.text()); !ok) {
        return ok;
    }
    touched_.clear();
    return {};
}

contracts::Awaitable<Result<>> OmpConfig::save_validated(const SettingsValidator& validator) {
    auto ok = co_await validate(validator);
    if (!ok) {
        co_return ok;
    }
    co_return save();
}

contracts::Awaitable<Result<>> OmpConfig::validate(const SettingsValidator& validator) const {
    std::vector<std::pair<std::string, runtime::json::Value>> expected;
    for (const auto& path : touched_) {
        std::vector<std::string_view> refs(path.begin(), path.end());
        const Yaml* value = doc_.get(std::span<const std::string_view>(refs));
        std::string dotted;
        for (std::size_t i = 0; i < path.size(); ++i) {
            if (i > 0) {
                dotted += '.';
            }
            dotted += path[i];
        }
        expected.emplace_back(std::move(dotted), value ? yaml::to_json(*value) : runtime::json::Value());
    }
    co_return co_await validator.check_config(doc_.text(), std::move(expected));
}

Result<> OmpConfig::set(std::span<const std::string_view> path, Yaml value) {
    if (auto ok = doc_.set(path, std::move(value)); !ok) {
        return ok;
    }
    touch(path);
    return {};
}

Result<bool> OmpConfig::remove(std::span<const std::string_view> path) {
    auto removed = doc_.remove(path);
    if (removed && *removed) {
        touch(path);
    }
    return removed;
}

void OmpConfig::touch(std::span<const std::string_view> path) {
    // omp exposes records (modelRoles, retry.fallbackChains) and arrays as one setting,
    // so validation compares the whole setting.
    std::vector<std::string> setting;
    if (path.size() >= 2 && path[0] == "retry" && path[1] == "fallbackChains") {
        setting = {"retry", "fallbackChains"};
    } else if (!path.empty()) {
        setting = {std::string(path[0])};
    } else {
        return;
    }
    if (std::ranges::find(touched_, setting) == touched_.end()) {
        touched_.push_back(std::move(setting));
    }
}

Result<std::vector<RoleAssignment>> OmpConfig::model_roles() const {
    std::vector<RoleAssignment> out;
    const Yaml* map = doc_.get({"modelRoles"});
    if (!map || !map->as_hash()) {
        return out;
    }
    for (const auto& [k, v] : *map->as_hash()) {
        const auto* role = k.as_str();
        const auto* value = v.as_str();
        if (!role || !value) {
            continue;
        }
        RoleAssignment assignment{*role, {}};
        std::string_view rest = *value;
        while (true) {
            const auto comma = rest.find(',');
            const std::string_view part = rest.substr(0, comma);
            if (!detail::trim(part).empty()) {
                auto selector = Selector::parse(part);
                if (!selector) {
                    return std::unexpected(std::move(selector.error()));
                }
                assignment.candidates.push_back(std::move(*selector));
            }
            if (comma == std::string_view::npos) {
                break;
            }
            rest.remove_prefix(comma + 1);
        }
        out.push_back(std::move(assignment));
    }
    return out;
}

Result<std::optional<RoleAssignment>> OmpConfig::model_role(std::string_view role) const {
    auto roles = model_roles();
    if (!roles) {
        return std::unexpected(std::move(roles.error()));
    }
    for (auto& r : *roles) {
        if (r.role == role) {
            return std::optional<RoleAssignment>(std::move(r));
        }
    }
    return std::optional<RoleAssignment>{};
}

Result<> OmpConfig::set_model_role(std::string_view role, std::span<const Selector> candidates) {
    if (auto ok = selector::validate_role_name(role); !ok) {
        return ok;
    }
    if (candidates.empty()) {
        return std::unexpected(Error::invalid("modelRoles", "a role needs at least one model; clear it instead"));
    }
    for (const auto& c : candidates) {
        if (auto ok = c.validate(); !ok) {
            return ok;
        }
        if (is_kind_role(role) && c.thinking()) {
            return std::unexpected(Error::invalid("modelRoles", "`" + std::string(role) + "` selects a runner and takes no thinking suffix"));
        }
    }
    const std::array<std::string_view, 2> path = {"modelRoles", role};
    return set(path, yaml::ystr(join_selectors(candidates, ",")));
}

Result<bool> OmpConfig::clear_model_role(std::string_view role) {
    const std::array<std::string_view, 2> path = {"modelRoles", role};
    return remove(path);
}

Result<std::vector<FallbackChain>> OmpConfig::fallback_chains() const {
    std::vector<FallbackChain> out;
    const Yaml* map = doc_.get({"retry", "fallbackChains"});
    if (!map || !map->as_hash()) {
        return out;
    }
    for (const auto& [k, v] : *map->as_hash()) {
        const auto* key = k.as_str();
        if (!key) {
            continue;
        }
        std::vector<Selector> fallbacks;
        if (const auto* items = v.as_vec()) {
            for (const auto& item : *items) {
                if (const auto* s = item.as_str()) {
                    auto selector = Selector::parse(*s);
                    if (!selector) {
                        return std::unexpected(std::move(selector.error()));
                    }
                    fallbacks.push_back(std::move(*selector));
                }
            }
        } else if (const auto* s = v.as_str()) {
            auto selector = Selector::parse(*s);
            if (!selector) {
                return std::unexpected(std::move(selector.error()));
            }
            fallbacks.push_back(std::move(*selector));
        } else {
            continue;
        }
        auto chain_key = ChainKey::parse(*key);
        if (!chain_key) {
            return std::unexpected(std::move(chain_key.error()));
        }
        out.push_back(FallbackChain{std::move(*chain_key), std::move(fallbacks)});
    }
    return out;
}

Result<> OmpConfig::set_fallback_chain(const ChainKey& key, std::span<const Selector> fallbacks) {
    if (auto ok = key.validate(); !ok) {
        return ok;
    }
    std::vector<std::string> values;
    for (const auto& f : fallbacks) {
        if (auto ok = f.validate(); !ok) {
            return ok;
        }
        values.push_back(f.to_string());
    }
    const std::string yaml_key = key.yaml_key();
    const std::array<std::string_view, 3> path = {"retry", "fallbackChains", yaml_key};
    return set(path, yaml::ystrs(values));
}

Result<bool> OmpConfig::clear_fallback_chain(const ChainKey& key) {
    const std::string yaml_key = key.yaml_key();
    const std::array<std::string_view, 3> path = {"retry", "fallbackChains", yaml_key};
    return remove(path);
}

std::vector<ScopedEntry> OmpConfig::scoped_list(ScopedList list) const {
    std::vector<ScopedEntry> out;
    const Yaml* items = doc_.get({metadata(list).key});
    if (!items || !items->as_vec()) {
        return out;
    }
    for (const auto& item : *items->as_vec()) {
        if (auto entry = parse_scoped(item)) {
            out.push_back(std::move(*entry));
        }
    }
    return out;
}

Result<> OmpConfig::set_scoped_list(ScopedList list, std::span<const ScopedEntry> entries) {
    const std::array<std::string_view, 1> path = {metadata(list).key};
    if (entries.empty()) {
        if (auto removed = remove(path); !removed) {
            return std::unexpected(std::move(removed.error()));
        }
        return {};
    }
    yaml::Array items;
    items.reserve(entries.size());
    for (const auto& entry : entries) {
        auto item = scoped_yaml(list, entry);
        if (!item) {
            return std::unexpected(std::move(item.error()));
        }
        items.push_back(std::move(*item));
    }
    return set(path, Yaml(std::move(items)));
}

std::vector<std::string> OmpConfig::provider_order() const {
    std::vector<std::string> out;
    const Yaml* items = doc_.get({PROVIDER_ORDER});
    if (items && items->as_vec()) {
        for (const auto& item : *items->as_vec()) {
            if (const auto* s = item.as_str()) {
                out.push_back(*s);
            }
        }
    }
    return out;
}

Result<> OmpConfig::set_provider_order(std::span<const std::string> providers) {
    const std::array<std::string_view, 1> path = {PROVIDER_ORDER};
    if (providers.empty()) {
        if (auto removed = remove(path); !removed) {
            return std::unexpected(std::move(removed.error()));
        }
        return {};
    }
    std::set<std::string_view> seen;
    for (const auto& value : providers) {
        if (auto ok = validate_value(PROVIDER_ORDER, value); !ok) {
            return ok;
        }
        if (!seen.insert(value).second) {
            return std::unexpected(Error::invalid(std::string(PROVIDER_ORDER), "`" + value + "` is listed twice"));
        }
    }
    return set(path, yaml::ystrs(providers));
}

} // namespace loa::settings::models::config
