#pragma once

// Model and role selector grammar (port of `models/selector.rs`).

#include <array>
#include <compare>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/settings/models/error.hpp"

namespace loa::settings::models::selector {

// Thinking levels a chat selector may carry as a `:level` suffix.
inline constexpr std::array<std::string_view, 9> THINKING_LEVELS = {
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto", "inherit",
};

[[nodiscard]] Result<> validate_role_name(std::string_view role);

// One candidate of a role assignment or fallback chain: a model selector
// (`provider/model`, a bare or fuzzy id, or a glob) or another role, written `@role`.
class Selector {
  public:
    enum class Kind : std::uint8_t { Model, Role };

    // A model selector without a thinking suffix.
    [[nodiscard]] static Selector model(std::string model) { return Selector(Kind::Model, std::move(model), {}); }
    // A role alias without a thinking suffix.
    [[nodiscard]] static Selector role(std::string role) { return Selector(Kind::Role, std::move(role), {}); }
    [[nodiscard]] static Selector model(std::string model, std::optional<std::string> thinking) {
        return Selector(Kind::Model, std::move(model), std::move(thinking));
    }
    [[nodiscard]] static Selector role(std::string role, std::optional<std::string> thinking) {
        return Selector(Kind::Role, std::move(role), std::move(thinking));
    }

    // Parses one candidate such as `anthropic/claude-opus-4-5:high` or `@smol`.
    [[nodiscard]] static Result<Selector> parse(std::string_view text);

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    [[nodiscard]] bool is_role() const noexcept { return kind_ == Kind::Role; }
    // The model or role name (without `@` and suffix).
    [[nodiscard]] const std::string& name() const noexcept { return name_; }
    // The model part of a model selector.
    [[nodiscard]] std::optional<std::string_view> model_name() const;
    // The thinking suffix, if any.
    [[nodiscard]] const std::optional<std::string>& thinking() const noexcept { return thinking_; }

    [[nodiscard]] Result<> validate() const;
    // `model[:thinking]` or `@role[:thinking]`.
    [[nodiscard]] std::string to_string() const;

    friend bool operator==(const Selector&, const Selector&) = default;

  private:
    Selector(Kind kind, std::string name, std::optional<std::string> thinking)
        : kind_(kind), name_(std::move(name)), thinking_(std::move(thinking)) {}

    Kind kind_ = Kind::Model;
    std::string name_;
    std::optional<std::string> thinking_;
};

// One model, named the way omp's `set_model` takes it.
struct ModelRef {
    std::string provider;
    std::string id;

    // Splits a `provider/id` selector at its first `/`. IDs may contain `/`.
    [[nodiscard]] static std::optional<std::pair<std::string_view, std::string_view>> split(std::string_view selector);
    // Parses the `provider/id` selector form.
    [[nodiscard]] static std::optional<ModelRef> parse(std::string_view selector);
    // The `provider/id` selector.
    [[nodiscard]] std::string selector() const { return provider + "/" + id; }
    [[nodiscard]] std::string to_string() const { return selector(); }

    friend bool operator==(const ModelRef&, const ModelRef&) = default;
    friend auto operator<=>(const ModelRef&, const ModelRef&) = default;
};

// A model the user can pick.
struct ModelInfo {
    ModelRef model;
    // Display name; empty falls back to the id.
    std::string name;
    // Display name of the provider; empty falls back to the provider id.
    std::string provider_name;

    [[nodiscard]] const std::string& display_name() const noexcept { return name.empty() ? model.id : name; }
    [[nodiscard]] const std::string& provider_display_name() const noexcept {
        return provider_name.empty() ? model.provider : provider_name;
    }
    friend bool operator==(const ModelInfo&, const ModelInfo&) = default;
};

// A model role and the model it currently resolves to.
struct RoleAssignment {
    std::string role;
    ModelRef model;
    friend bool operator==(const RoleAssignment&, const RoleAssignment&) = default;
};

// The models and role assignments the picker offers, as omp lists them.
struct Catalog {
    std::vector<ModelInfo> models;
    std::vector<RoleAssignment> roles;
    friend bool operator==(const Catalog&, const Catalog&) = default;
};

} // namespace loa::settings::models::selector
