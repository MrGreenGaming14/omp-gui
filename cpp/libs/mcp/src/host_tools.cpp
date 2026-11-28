#include "loa/mcp/host_tools.hpp"

#include <asio.hpp>

#include <array>
#include <cctype>
#include <thread>

#include "loa/runtime/log.hpp"

namespace loa::mcp::host_tools {

namespace json = loa::runtime::json;
using contracts::ImageBlock;
using contracts::OutputBlock;
using contracts::TextBlock;
using contracts::ToolOutput;

namespace {

// Standard base64 with padding; none on any other byte.
std::optional<std::vector<std::byte>> base64_decode(std::string_view text) {
    static constexpr auto table = [] {
        std::array<int, 256> t{};
        t.fill(-1);
        constexpr std::string_view alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for (std::size_t i = 0; i < alphabet.size(); ++i) t[static_cast<unsigned char>(alphabet[i])] = static_cast<int>(i);
        return t;
    }();
    if (text.size() % 4 != 0) return std::nullopt;
    std::vector<std::byte> out;
    out.reserve(text.size() / 4 * 3);
    for (std::size_t i = 0; i < text.size(); i += 4) {
        int v[4];
        int pad = 0;
        for (int k = 0; k < 4; ++k) {
            const char c = text[i + k];
            if (c == '=' && i + 4 == text.size() && k >= 2) {
                v[k] = 0;
                ++pad;
                continue;
            }
            if (pad > 0) return std::nullopt;
            v[k] = table[static_cast<unsigned char>(c)];
            if (v[k] < 0) return std::nullopt;
        }
        const std::uint32_t n = (static_cast<std::uint32_t>(v[0]) << 18) | (static_cast<std::uint32_t>(v[1]) << 12) |
                                (static_cast<std::uint32_t>(v[2]) << 6) | static_cast<std::uint32_t>(v[3]);
        out.push_back(static_cast<std::byte>(n >> 16));
        if (pad < 2) out.push_back(static_cast<std::byte>((n >> 8) & 0xff));
        if (pad < 1) out.push_back(static_cast<std::byte>(n & 0xff));
    }
    return out;
}

std::string field(const json::Value& object, std::string_view key) {
    const auto* value = object.find(key);
    return value && value->as_string() ? *value->as_string() : std::string();
}

template <typename T, typename Work>
contracts::Awaitable<T> off_thread(Work work) {
    co_return co_await asio::async_initiate<decltype(asio::use_awaitable), void(T)>(
        [work = std::move(work)](auto handler) mutable {
            std::thread([work = std::move(work), handler = std::move(handler)]() mutable {
                T result = work();
                auto executor = asio::get_associated_executor(handler);
                asio::post(executor, [handler = std::move(handler), result = std::move(result)]() mutable { handler(std::move(result)); });
            }).detach();
        },
        asio::use_awaitable);
}

} // namespace

std::string host_tool_name(std::string_view server, std::string_view tool) {
    const std::string name = std::string(server) + "__" + std::string(tool);
    std::string out;
    for (const char c : name) {
        const auto u = static_cast<unsigned char>(c);
        // A UTF-8 continuation byte belongs to a character already replaced by one `_`, as Rust's `chars()` maps it.
        if ((u & 0xC0) == 0x80) continue;
        out.push_back((u < 128 && (std::isalnum(u) || c == '_' || c == '-')) ? c : '_');
    }
    return out;
}

ToolOutput to_output(const model::CallToolResult& result) {
    ToolOutput output;
    for (const auto& block : result.content) {
        const auto kind = field(block, "type");
        if (kind == "text") {
            output.blocks.emplace_back(TextBlock{field(block, "text")});
        } else if (kind == "image") {
            const auto mime = field(block, "mimeType");
            if (auto data = base64_decode(field(block, "data"))) output.blocks.emplace_back(ImageBlock{mime, std::move(*data)});
            else output.blocks.emplace_back(TextBlock{"[undecodable " + mime + " image]"});
        } else if (kind == "resource") {
            const auto* resource = block.find("resource");
            if (resource && resource->find("text") && resource->find("text")->as_string())
                output.blocks.emplace_back(TextBlock{*resource->find("text")->as_string()});
            else if (resource && resource->find("blob"))
                output.blocks.emplace_back(TextBlock{"[binary resource " + field(*resource, "uri") + "]"});
            else output.blocks.emplace_back(TextBlock{"[resource]"});
        } else if (kind == "resource_link") {
            output.blocks.emplace_back(TextBlock{"[resource " + field(block, "uri") + "]"});
        } else if (kind == "audio") {
            output.blocks.emplace_back(TextBlock{"[" + field(block, "mimeType") + " audio]"});
        } else {
            output.blocks.emplace_back(TextBlock{"[unsupported content]"});
        }
    }
    if (output.blocks.empty() && result.structured_content) output.blocks.emplace_back(TextBlock{json::dump(*result.structured_content)});
    output.is_error = result.is_error == true;
    return output;
}

std::string text(const ToolOutput& output) {
    std::string out;
    bool first = true;
    for (const auto& block : output.blocks) {
        if (!first) out.push_back('\n');
        first = false;
        if (const auto* t = std::get_if<TextBlock>(&block)) out += t->text;
        else {
            const auto& image = std::get<ImageBlock>(block);
            out += "[" + image.mime + " image, " + std::to_string(image.data.size()) + " bytes]";
        }
    }
    return out;
}

McpHostTools::McpHostTools(Manager manager, std::shared_ptr<const ToolFilter> filter)
    : manager_(std::move(manager)), filter_(std::move(filter)) {
    for (auto& server : manager_.view(*filter_)) {
        if (server.status != Status::running()) continue;
        const auto& server_name = server.spec.name;
        for (auto& view : server.tools) {
            if (!view.allowed) continue;
            const auto& tool = view.tool;
            auto name = host_tool_name(server_name, tool.name);
            if (routes_.contains(name)) {
                runtime::log::warn("loa_mcp", name + ": two MCP tools map to the same host tool name; keeping the first");
                continue;
            }
            std::string description = tool.description && !tool.description->empty()
                                          ? "[" + server_name + "] " + *tool.description
                                          : "Tool `" + tool.name + "` of the MCP server `" + server_name + "`";
            specs_.push_back(contracts::HostToolSpec{name, std::move(description), json::dump(json::Value(tool.input_schema))});
            routes_.emplace(std::move(name), std::pair{server_name, tool.name});
        }
    }
}

std::expected<ToolOutput, std::string> McpHostTools::run(std::string_view tool, std::string_view args) const {
    auto route = routes_.find(tool);
    if (route == routes_.end()) return std::unexpected("unknown MCP tool `" + std::string(tool) + "`");
    auto parsed = json::parse(args);
    if (!parsed) return std::unexpected("arguments are not JSON: " + parsed.error().message());
    std::optional<model::JsonObject> arguments;
    if (const auto* object = parsed->as_object()) arguments = *object;
    else if (!parsed->is_null()) return std::unexpected("arguments must be a JSON object, got " + json::dump(*parsed));
    auto result = manager_.call_tool(route->second.first, route->second.second, std::move(arguments), *filter_);
    if (!result) return std::unexpected(result.error().to_string());
    auto output = to_output(*result);
    if (output.is_error) return std::unexpected(text(output));
    return output;
}

contracts::Awaitable<std::expected<std::string, std::string>> McpHostTools::call(std::string, std::string tool, std::string args) const {
    auto self = *this;
    co_return co_await off_thread<std::expected<std::string, std::string>>(
        [self = std::move(self), tool = std::move(tool), args = std::move(args)]() -> std::expected<std::string, std::string> {
            auto output = self.run(tool, args);
            if (!output) return std::unexpected(std::move(output.error()));
            return text(*output);
        });
}

contracts::Awaitable<std::expected<std::vector<OutputBlock>, std::string>> McpHostTools::call_content(std::string, std::string tool,
                                                                                                     std::string args) const {
    auto self = *this;
    co_return co_await off_thread<std::expected<std::vector<OutputBlock>, std::string>>(
        [self = std::move(self), tool = std::move(tool), args = std::move(args)]() -> std::expected<std::vector<OutputBlock>, std::string> {
            auto output = self.run(tool, args);
            if (!output) return std::unexpected(std::move(output.error()));
            return std::move(output->blocks);
        });
}

} // namespace loa::mcp::host_tools
