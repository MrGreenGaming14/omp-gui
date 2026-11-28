#include "loa/contracts/tools.hpp"

#include <asio.hpp>

#include <format>

namespace loa::contracts {

namespace json = loa::runtime::json;

ToolOutput ToolOutput::text(std::string text) { return ToolOutput{{TextBlock{std::move(text)}}, false}; }

ToolOutput ToolOutput::error(std::string text) { return ToolOutput{{TextBlock{std::move(text)}}, true}; }

std::string ToolOutput::joined_text() const {
    std::string out;
    bool first = true;
    for (const auto& block : blocks) {
        const auto* text = std::get_if<TextBlock>(&block);
        if (!text) continue;
        if (!first) out += '\n';
        out += text->text;
        first = false;
    }
    return out;
}

Awaitable<ToolResult> ready(ToolResult result) { co_return result; }

ToolSet& ToolSet::add(std::shared_ptr<const DynTool> tool) {
    tools_.insert_or_assign(tool->spec().name, std::move(tool));
    return *this;
}

const DynTool* ToolSet::get(std::string_view name) const {
    auto it = tools_.find(name);
    return it == tools_.end() ? nullptr : it->second.get();
}

std::vector<HostToolSpec> ToolSet::specs() const {
    std::vector<HostToolSpec> out;
    out.reserve(tools_.size());
    for (const auto& [_, tool] : tools_) out.push_back(tool->spec());
    return out;
}

Awaitable<ToolResult> ToolSet::call(const CallCtx& ctx, std::string name, Json args) const {
    const auto* tool = get(name);
    if (!tool) co_return std::unexpected("unknown tool `" + name + "`");
    co_return co_await tool->call(ctx, std::move(args));
}

Awaitable<std::expected<std::string, std::string>> ToolSet::call(std::string session_id, std::string tool,
                                                                 std::string args) const {
    auto blocks = co_await call_content(std::move(session_id), std::move(tool), std::move(args));
    if (!blocks) co_return std::unexpected(std::move(blocks.error()));
    co_return ToolOutput{std::move(*blocks), false}.joined_text();
}

Awaitable<std::expected<std::vector<OutputBlock>, std::string>> ToolSet::call_content(std::string session_id, std::string tool,
                                                                                      std::string args) const {
    // Blank args mean "no arguments": an empty object, so all-optional argument structs still parse.
    Json parsed = json::Object{};
    if (args.find_first_not_of(" \t\r\n") != std::string::npos) {
        auto value = json::parse(args);
        if (!value) co_return std::unexpected(value.error().message());
        parsed = std::move(*value);
    }
    const CallCtx ctx{std::move(session_id)};
    auto out = co_await call(ctx, std::move(tool), std::move(parsed));
    if (!out) co_return std::unexpected(std::move(out.error()));
    if (out->is_error) co_return std::unexpected(out->joined_text());
    co_return std::move(out->blocks);
}

namespace args {

namespace {

// serde's `Unexpected` wording for what was found.
std::string found(const Json& v) {
    return std::visit(
        [](const auto& x) -> std::string {
            using T = std::decay_t<decltype(x)>;
            if constexpr (std::is_same_v<T, std::monostate>) return "null";
            else if constexpr (std::is_same_v<T, bool>) return std::format("boolean `{}`", x);
            else if constexpr (std::is_same_v<T, std::int64_t> || std::is_same_v<T, std::uint64_t>) return std::format("integer `{}`", x);
            else if constexpr (std::is_same_v<T, double>) return std::format("floating point `{}`", x);
            else if constexpr (std::is_same_v<T, std::string>) return std::format("string \"{}\"", x);
            else if constexpr (std::is_same_v<T, json::Array>) return "sequence";
            else return "map";
        },
        v.storage());
}

std::string invalid(const Json& v, std::string_view expected) { return std::format("invalid type: {}, expected {}", found(v), expected); }

template <typename T, typename Get>
std::expected<std::optional<T>, std::string> optional(const Json& json, std::string_view name, std::string_view expected, Get get) {
    const auto* v = json.find(name);
    if (!v || v->is_null()) return std::nullopt;
    if (auto x = get(*v)) return std::optional<T>(*x);
    return std::unexpected(invalid(*v, expected));
}

} // namespace

std::expected<void, std::string> object(const Json& json, std::string_view what) {
    if (json.is_object()) return {};
    return std::unexpected(invalid(json, what));
}

std::expected<std::string, std::string> string(const Json& json, std::string_view name) {
    auto v = optional_string(json, name);
    if (!v) return std::unexpected(std::move(v.error()));
    if (!*v) return std::unexpected(std::format("missing field `{}`", name));
    return std::move(**v);
}

std::expected<std::optional<std::string>, std::string> optional_string(const Json& json, std::string_view name) {
    return optional<std::string>(json, name, "a string", [](const Json& v) { return v.as_string(); });
}

std::expected<std::optional<bool>, std::string> optional_bool(const Json& json, std::string_view name) {
    return optional<bool>(json, name, "a boolean", [](const Json& v) { return v.as_bool(); });
}

std::expected<std::optional<std::uint64_t>, std::string> optional_u64(const Json& json, std::string_view name) {
    return optional<std::uint64_t>(json, name, "u64", [](const Json& v) { return v.as_u64(); });
}

std::expected<std::optional<double>, std::string> optional_f64(const Json& json, std::string_view name) {
    return optional<double>(json, name, "f64", [](const Json& v) { return v.as_f64(); });
}

} // namespace args

} // namespace loa::contracts
