#pragma once

// Typed agent tools: a `Tool<Args>` is registered in a `ToolSet`, served as host
// tools or over MCP.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

// `Tool<Args>::call` is defined here and returns an awaitable by value; the
// awaitable class alone is light (the rest of Asio stays out).
#include <asio/awaitable.hpp>
#include <asio/any_io_executor.hpp>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/runtime/json.hpp"

namespace loa::contracts {

using Json = loa::runtime::json::Value;

// Result of a tool run; `Err` reaches the model as a tool error.
using ToolResult = std::expected<ToolOutput, std::string>;

// MCP-style hints about what a tool does.
struct ToolAnnotations {
    std::optional<std::string> title;
    bool read_only = false;
    bool destructive = false;
    friend bool operator==(const ToolAnnotations&, const ToolAnnotations&) = default;
};

// Who is calling.
struct CallCtx {
    std::string session_id;
    friend bool operator==(const CallCtx&, const CallCtx&) = default;
};

// Object-safe tool, taking JSON arguments.
class DynTool {
  public:
    virtual ~DynTool() = default;
    [[nodiscard]] virtual HostToolSpec spec() const = 0;
    [[nodiscard]] virtual ToolAnnotations annotations() const { return {}; }
    // `ctx` must outlive the returned coroutine.
    [[nodiscard]] virtual Awaitable<ToolResult> call(const CallCtx& ctx, Json args) const = 0;
};

// A coroutine that completes at once with `result`.
[[nodiscard]] Awaitable<ToolResult> ready(ToolResult result);

// Argument types of a `Tool`: the JSON Schema (`schema::Root::dump`, matching
// schemars) and serde-style decoding.
template <typename A>
concept ToolArgs = requires(const Json& json) {
    { A::schema() } -> std::convertible_to<std::string>;
    { A::from_json(json) } -> std::same_as<std::expected<A, std::string>>;
};

// A tool with typed arguments; its JSON Schema comes from `Args::schema()`.
template <ToolArgs A>
class Tool : public DynTool {
  public:
    using Args = A;

    [[nodiscard]] virtual std::string_view name() const = 0;
    [[nodiscard]] virtual std::string_view description() const = 0;
    [[nodiscard]] virtual Awaitable<ToolResult> run(const CallCtx& ctx, Args args) const = 0;

    [[nodiscard]] HostToolSpec spec() const final {
        return HostToolSpec{std::string(name()), std::string(description()), std::string(A::schema())};
    }

    [[nodiscard]] Awaitable<ToolResult> call(const CallCtx& ctx, Json args) const final {
        auto parsed = A::from_json(args);
        if (!parsed) {
            return ready(std::unexpected("invalid arguments for `" + std::string(name()) + "`: " + parsed.error()));
        }
        return run(ctx, std::move(*parsed));
    }
};

// Tools by name; also the `HostTools` handler serving them.
class ToolSet final : public HostTools {
  public:
    // Adds `tool`, replacing one with the same name.
    ToolSet& add(std::shared_ptr<const DynTool> tool);
    [[nodiscard]] const DynTool* get(std::string_view name) const;
    // Specs ordered by name.
    [[nodiscard]] std::vector<HostToolSpec> specs() const;
    // `ctx` must outlive the returned coroutine.
    [[nodiscard]] Awaitable<ToolResult> call(const CallCtx& ctx, std::string name, Json args) const;

    [[nodiscard]] Awaitable<std::expected<std::string, std::string>> call(
        std::string session_id, std::string tool, std::string args) const override;
    [[nodiscard]] Awaitable<std::expected<std::vector<OutputBlock>, std::string>> call_content(
        std::string session_id, std::string tool, std::string args) const override;

  private:
    std::map<std::string, std::shared_ptr<const DynTool>, std::less<>> tools_;
};

// serde-style field decoding for `ToolArgs::from_json`; errors read like serde_json's.
namespace args {

// `invalid type: <found>, expected <what>` unless `json` is an object.
[[nodiscard]] std::expected<void, std::string> object(const Json& json, std::string_view what);
// `missing field `name``, or an `invalid type` error.
[[nodiscard]] std::expected<std::string, std::string> string(const Json& json, std::string_view name);
[[nodiscard]] std::expected<std::optional<std::string>, std::string> optional_string(const Json& json, std::string_view name);
[[nodiscard]] std::expected<std::optional<bool>, std::string> optional_bool(const Json& json, std::string_view name);
[[nodiscard]] std::expected<std::optional<std::uint64_t>, std::string> optional_u64(const Json& json, std::string_view name);
[[nodiscard]] std::expected<std::optional<double>, std::string> optional_f64(const Json& json, std::string_view name);

} // namespace args

} // namespace loa::contracts
