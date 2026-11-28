#pragma once

// Typed editor for omp's `config.yml`: model roles, fallback chains, enabled models,
// enabled and disabled providers and provider order (port of `models/config.rs`).
//
// | Setting | YAML |
// | --- | --- |
// | Role assignments | `modelRoles.<role>`: a selector or comma separated candidates |
// | Fallback chains | `retry.fallbackChains.<role, provider/model or provider/*>`: a list; `[]` means no fallback |
// | Enabled models | `enabledModels`: selectors, globs or path scoped `{ path, models }` entries |
// | Foreign discovery sources | `enabledProviders`: ids or path scoped `{ path, providers }` entries |
// | Disabled providers | `disabledProviders`: ids or path scoped `{ path, providers }` entries |
// | Provider order | `modelProviderOrder`: provider ids |
//
// Every edit changes only the touched key's lines; saving writes atomically and
// refuses when the file changed since it was loaded.

#include <array>
#include <compare>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/settings/models/error.hpp"
#include "loa/settings/models/selector.hpp"
#include "loa/settings/models/yaml.hpp"

namespace loa::settings::models::config {

using selector::Selector;

// Built in chat roles, in the order omp's model picker lists them.
inline constexpr std::array<std::string_view, 11> CHAT_ROLES = {
    "default", "smol", "slow", "vision", "plan", "commit", "tiny", "memory", "task", "advisor", "widget",
};

// Built in model-kind roles: they select runners and take no thinking suffix.
inline constexpr std::array<std::string_view, 5> KIND_ROLES = {"image", "web", "speech", "dictation", "judge"};

// Whether `role` is one of omp's model-kind roles.
[[nodiscard]] bool is_kind_role(std::string_view role) noexcept;

// A role and its ordered primary candidates (`modelRoles.<role>`).
struct RoleAssignment {
    std::string role;
    std::vector<Selector> candidates;
    friend bool operator==(const RoleAssignment&, const RoleAssignment&) = default;
};

// The key a fallback chain applies to (`retry.fallbackChains.<key>`).
struct ChainKey {
    enum class Kind : std::uint8_t {
        Role,     // a role, such as `default` or `judge`
        Model,    // an exact `provider/model` selector
        Provider, // every model of a provider, written `provider/*`
    };
    Kind kind = Kind::Role;
    std::string value;

    [[nodiscard]] static ChainKey role(std::string r) { return {Kind::Role, std::move(r)}; }
    [[nodiscard]] static ChainKey model(std::string m) { return {Kind::Model, std::move(m)}; }
    [[nodiscard]] static ChainKey provider(std::string p) { return {Kind::Provider, std::move(p)}; }

    // Parses a key as written in `config.yml`.
    [[nodiscard]] static Result<ChainKey> parse(std::string_view text);
    [[nodiscard]] Result<> validate() const;
    // The key as written in `config.yml`.
    [[nodiscard]] std::string yaml_key() const;

    friend bool operator==(const ChainKey&, const ChainKey&) = default;
    friend auto operator<=>(const ChainKey&, const ChainKey&) = default;
};

// A fallback chain. An empty list is meaningful: omp then tries no fallback.
struct FallbackChain {
    ChainKey key;
    std::vector<Selector> fallbacks;
    friend bool operator==(const FallbackChain&, const FallbackChain&) = default;
};

// An entry of a path scopable list (`enabledModels`, `enabledProviders`,
// `disabledProviders`).
struct ScopedEntry {
    enum class Kind : std::uint8_t {
        Everywhere, // `value` applies everywhere
        Paths,      // `values` apply when the working directory is one of `paths` or below it
    };
    Kind kind = Kind::Everywhere;
    std::string value;
    std::vector<std::string> paths;
    std::vector<std::string> values;

    [[nodiscard]] static ScopedEntry everywhere(std::string value) { return {Kind::Everywhere, std::move(value), {}, {}}; }
    [[nodiscard]] static ScopedEntry scoped(std::vector<std::string> paths, std::vector<std::string> values) {
        return {Kind::Paths, {}, std::move(paths), std::move(values)};
    }
    [[nodiscard]] bool is_everywhere() const noexcept { return kind == Kind::Everywhere; }

    friend bool operator==(const ScopedEntry&, const ScopedEntry&) = default;
};
// Which path scopable list an entry belongs to; picks the value key omp reads.
enum class ScopedList : std::uint8_t {
    EnabledModels,     // `enabledModels`: model selectors, globs or fuzzy ids
    EnabledProviders,  // `enabledProviders`: foreign discovery sources to load
    DisabledProviders, // `disabledProviders`: model providers or discovery sources to hide
};

// omp's `config.yml`, loaded for editing.
class OmpConfig {
  public:
    // Loads `path`; a missing file is an empty config.
    [[nodiscard]] static Result<OmpConfig> load(const std::filesystem::path& path);

    // The edited document text.
    [[nodiscard]] const std::string& text() const noexcept { return doc_.text(); }
    // The file this config was loaded from.
    [[nodiscard]] const std::filesystem::path& path() const noexcept { return snapshot_.path(); }

    // Writes the edits atomically.
    [[nodiscard]] Result<> save();
    // Asks `validator` (omp itself) whether it reads the edited keys back as written,
    // then saves. The config must outlive the coroutine.
    [[nodiscard]] contracts::Awaitable<Result<>> save_validated(const SettingsValidator& validator);
    // Asks `validator` whether it reads the keys edited since the last save back as
    // written. Writes nothing.
    [[nodiscard]] contracts::Awaitable<Result<>> validate(const SettingsValidator& validator) const;
    // Whether keys were edited since the load or the last save.
    [[nodiscard]] bool is_edited() const noexcept { return !touched_.empty(); }

    // Role assignments in file order. Comma separated values become several candidates.
    [[nodiscard]] Result<std::vector<RoleAssignment>> model_roles() const;
    // The assignment of one role, if set.
    [[nodiscard]] Result<std::optional<RoleAssignment>> model_role(std::string_view role) const;
    // Assigns `candidates` (first available wins) to `role`.
    [[nodiscard]] Result<> set_model_role(std::string_view role, std::span<const Selector> candidates);
    // Removes `role`'s assignment so omp's default applies.
    [[nodiscard]] Result<bool> clear_model_role(std::string_view role);

    // Fallback chains in file order.
    [[nodiscard]] Result<std::vector<FallbackChain>> fallback_chains() const;
    // Sets the fallback chain for `key`. An empty list writes `[]` ("no fallback").
    [[nodiscard]] Result<> set_fallback_chain(const ChainKey& key, std::span<const Selector> fallbacks);
    // Removes the fallback chain for `key`.
    [[nodiscard]] Result<bool> clear_fallback_chain(const ChainKey& key);

    // Entries of a path scopable list. Malformed entries, which omp ignores, are skipped.
    [[nodiscard]] std::vector<ScopedEntry> scoped_list(ScopedList list) const;
    // Replaces a path scopable list. An empty list removes the key.
    [[nodiscard]] Result<> set_scoped_list(ScopedList list, std::span<const ScopedEntry> entries);

    // The provider order used when a bare model id exists under several providers.
    [[nodiscard]] std::vector<std::string> provider_order() const;
    // Replaces the provider order; an empty order removes the key.
    [[nodiscard]] Result<> set_provider_order(std::span<const std::string> providers);

  private:
    OmpConfig(yaml::YamlDocument doc, runtime::fs::Snapshot snapshot) : doc_(std::move(doc)), snapshot_(std::move(snapshot)) {}

    Result<> set(std::span<const std::string_view> path, yaml::Yaml value);
    Result<bool> remove(std::span<const std::string_view> path);
    void touch(std::span<const std::string_view> path);

    yaml::YamlDocument doc_;
    runtime::fs::Snapshot snapshot_;
    std::vector<std::vector<std::string>> touched_;
};

} // namespace loa::settings::models::config
