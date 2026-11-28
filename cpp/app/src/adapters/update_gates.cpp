#include "loa/app/adapters/update_gates.hpp"

#include <asio/co_spawn.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>
#include <asio/use_future.hpp>

#include <algorithm>
#include <array>
#include <cerrno>
#include <cstring>
#include <fstream>
#include <iterator>
#include <map>
#include <set>
#include <variant>

#include "loa/harness/kit/rpc.hpp"
#include "loa/harness/omp/oneshot.hpp"
#include "loa/harness/omp/rpc/client.hpp"
#include "loa/harness/omp/rpc/typed.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"

namespace loa::app::adapters {

namespace {

#include "update_gates_goldens.inc"

namespace json = loa::runtime::json;
namespace rpc = loa::harness::omp::rpc;
namespace command = loa::harness::omp::rpc::command;
using loa::contracts::Awaitable;
using loa::updater::ConfigSchema;
using loa::updater::ContractFailure;
using loa::updater::ToolSchema;
using detail::Json;
using Path = std::filesystem::path;

template <typename T>
using Res = std::expected<T, std::string>;

constexpr std::array<detail::Golden, 2> GOLDENS{{{"basic", GOLDEN_BASIC}, {"branched", GOLDEN_BRANCHED}}};

// Flags that keep probes from loading project context, skills, rules, language servers or extensions.
constexpr std::array<std::string_view, 6> QUIET{"--no-context-files", "--no-rules", "--no-skills",
                                                "--no-title",         "--no-lsp",   "--no-extensions"};

// Runs `aw` on `exec` and blocks the calling thread for its result.
template <typename T>
T block_on(loa::runtime::Executor& exec, Awaitable<T> aw) {
    return asio::co_spawn(exec.context(), std::move(aw), asio::use_future).get();
}

std::string timeout_text(std::chrono::milliseconds timeout) {
    return "omp did not answer within " + loa::harness::kit::duration_debug(timeout);
}

// Runs `work` within `timeout`.
template <typename T>
Awaitable<Res<T>> timed(Awaitable<Res<T>> work, std::chrono::milliseconds timeout) {
    using namespace asio::experimental::awaitable_operators;
    asio::steady_timer timer(co_await asio::this_coro::executor, timeout);
    auto raced = co_await (std::move(work) || timer.async_wait(asio::use_awaitable));
    if (raced.index() == 1) co_return std::unexpected(timeout_text(timeout));
    co_return std::move(std::get<0>(raced));
}

Res<loa::runtime::TempDir> workdir() {
    auto dir = loa::runtime::TempDir::create();
    if (!dir) return std::unexpected("creating a probe directory: " + dir.error().message());
    return std::move(*dir);
}

Res<std::string> read_text(const Path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::unexpected(std::string(std::strerror(errno)));
    return std::string(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

Res<void> write_text(const Path& path, std::string_view text) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    if (!out || !out.write(text.data(), static_cast<std::streamsize>(text.size())) || !out.flush())
        return std::unexpected(std::string(std::strerror(errno)));
    return {};
}

std::string_view trim(std::string_view s) {
    constexpr std::string_view ws = " \t\r\n\v\f";
    const auto b = s.find_first_not_of(ws);
    if (b == std::string_view::npos) return {};
    return s.substr(b, s.find_last_not_of(ws) - b + 1);
}

// `str::lines`: split on `\n`, a trailing `\r` dropped, no empty last line.
std::vector<std::string_view> lines(std::string_view text) {
    std::vector<std::string_view> out;
    while (!text.empty()) {
        const auto nl = text.find('\n');
        auto line = text.substr(0, nl);
        if (line.ends_with('\r')) line.remove_suffix(1);
        out.push_back(line);
        if (nl == std::string_view::npos) break;
        text.remove_prefix(nl + 1);
    }
    return out;
}

const Json* field(const Json& v, std::string_view key) { return v.find(key); }

const std::string* str_field(const Json& v, std::string_view key) {
    const auto* f = v.find(key);
    return f ? f->as_string() : nullptr;
}

std::string kind(const Json& e) {
    const auto* t = str_field(e, "type");
    return t ? *t : "?";
}

std::string short_text(const Json& value) {
    auto text = json::dump(value);
    std::size_t chars = 0;
    for (std::size_t at = 0; at < text.size(); ++at) {
        if ((static_cast<unsigned char>(text[at]) & 0xC0) == 0x80) continue;
        if (chars++ == 80) return text.substr(0, at) + "…";
    }
    return text;
}

// A line for every place `actual` lacks what `expected` holds; objects may add keys, arrays match element-wise.
void subset_diff(const Json& expected, const Json& actual, const std::string& path, std::vector<std::string>& out) {
    const auto* want = expected.as_object();
    const auto* have = actual.as_object();
    if (want && have) {
        for (const auto& [key, value] : *want) {
            const auto at = path + "." + key;
            if (const auto* found = actual.find(key)) subset_diff(value, *found, at, out);
            else out.push_back("- " + at + ": missing (was " + short_text(value) + ")");
        }
        return;
    }
    const auto* wa = expected.as_array();
    const auto* ha = actual.as_array();
    if (wa && ha && wa->size() == ha->size()) {
        for (std::size_t i = 0; i < wa->size(); ++i) subset_diff((*wa)[i], (*ha)[i], path + "[" + std::to_string(i) + "]", out);
        return;
    }
    if (loa::updater::json_equal(expected, actual)) return;
    out.push_back("~ " + path + ": " + short_text(expected) + " -> " + short_text(actual));
}

// Tool schemas out of `get_state` data.
Res<std::vector<ToolSchema>> parse_dump_tools(const Json& state) {
    const auto* dump = field(state, "dumpTools");
    const auto* tools = dump ? dump->as_array() : nullptr;
    if (!tools) return std::unexpected(std::string("get_state has no dumpTools"));
    std::vector<ToolSchema> out;
    for (const auto& tool : *tools) {
        const auto* name = str_field(tool, "name");
        if (!name) return std::unexpected(std::string("dumpTools entry without a name"));
        const auto* params = field(tool, "parameters");
        out.push_back(ToolSchema{*name, params ? *params : Json(json::Object{{"type", "object"}})});
    }
    return out;
}

std::string first_line(std::string_view text) {
    for (auto line : lines(text))
        if (auto t = trim(line); !t.empty()) return std::string(t);
    return {};
}

Awaitable<Res<Json>> get_state_json(rpc::RpcClient client) {
    auto state = co_await client.request(command::GetState{});
    if (!state) co_return std::unexpected(state.error().message());
    co_return std::move(*state);
}

// A process resumed on a golden session in its own directory, which must outlive it.
struct Resumed {
    loa::runtime::TempDir dir;
    Path file;
    rpc::RpcClient rpc;
};

struct Ctx {
    std::chrono::milliseconds timeout;

    Awaitable<rpc::RpcResult<rpc::RpcClient>> spawn(Path executable, std::vector<std::string> args, Path cwd) const {
        rpc::SpawnOptions opts(std::move(executable));
        opts.args = std::move(args);
        opts.cwd = std::move(cwd);
        opts.ready_timeout = timeout;
        opts.negotiate = false;
        auto spawned = co_await rpc::RpcClient::spawn(std::move(opts));
        if (!spawned) co_return std::unexpected(std::move(spawned.error()));
        co_return std::move(spawned->client);
    }

    Awaitable<Res<std::shared_ptr<Resumed>>> resume(Path executable, std::string name, std::string_view jsonl) const {
        auto dir = workdir();
        if (!dir) co_return std::unexpected(dir.error());
        const auto sessions = dir->path() / "sessions";
        std::error_code ec;
        std::filesystem::create_directories(sessions, ec);
        if (ec) co_return std::unexpected("creating " + sessions.string() + ": " + ec.message());
        auto file = sessions / (name + ".jsonl");
        auto text = detail::with_cwd(jsonl, dir->path());
        if (!text) co_return std::unexpected(text.error());
        if (auto w = write_text(file, *text); !w) co_return std::unexpected("writing " + file.string() + ": " + w.error());
        std::vector<std::string> args(QUIET.begin(), QUIET.end());
        args.insert(args.end(), {"--session-dir", sessions.string(), "--resume", file.string()});
        auto cwd = dir->path();
        auto client = co_await spawn(std::move(executable), std::move(args), std::move(cwd));
        if (!client) co_return std::unexpected(client.error().message());
        co_return std::make_shared<Resumed>(Resumed{std::move(*dir), std::move(file), std::move(*client)});
    }

    // Forks the resumed session and returns the lines of the file the fork wrote.
    static Awaitable<Res<std::vector<Json>>> fork(std::shared_ptr<Resumed> resumed) {
        auto forked = co_await resumed->rpc.request<rpc::CancellationResult>(command::Fork{});
        if (!forked) co_return std::unexpected("fork: " + forked.error().message());
        auto state = co_await resumed->rpc.request<rpc::SessionState>(command::GetState{});
        if (!state) co_return std::unexpected("get_state: " + state.error().message());
        if (!state->session_file) co_return std::unexpected(std::string("get_state after fork has no sessionFile"));
        const auto& file = *state->session_file;
        if (Path(file) == resumed->file) co_return std::unexpected(std::string("fork did not switch to a new session file"));
        auto text = read_text(file);
        if (!text) co_return std::unexpected("reading forked " + file + ": " + text.error());
        co_return detail::parse_lines(*text);
    }

    Awaitable<Res<std::vector<ContractFailure>>> round_trip(Path executable, std::string name, std::string_view jsonl) const {
        auto expected = detail::entries(jsonl);
        if (!expected) co_return std::unexpected(expected.error());
        auto resumed = co_await resume(std::move(executable), name, jsonl);
        if (!resumed) co_return std::unexpected(resumed.error());
        auto data = co_await (*resumed)->rpc.request(command::GetEntries{});
        if (!data) {
            (*resumed)->rpc.kill();
            co_return std::unexpected("get_entries: " + data.error().message());
        }
        auto forked = co_await fork(*resumed);
        (*resumed)->rpc.kill();
        const auto* entries_field = field(*data, "entries");
        const auto* read = entries_field ? entries_field->as_array() : nullptr;
        if (!read) co_return std::unexpected(std::string("get_entries has no entries"));
        std::optional<std::string_view> leaf;
        if (const auto* l = str_field(*data, "leafId")) leaf = *l;
        if (!forked) co_return std::unexpected(forked.error());
        std::vector<Json> fork_entries;
        for (auto& line : *forked)
            if (detail::is_entry(line)) fork_entries.push_back(std::move(line));

        std::vector<ContractFailure> failures;
        auto fail = [&](std::string_view test, const std::vector<std::string>& diff) {
            if (diff.empty()) return;
            std::string joined;
            for (const auto& d : diff) joined += (joined.empty() ? "" : "\n") + d;
            failures.push_back(ContractFailure{name + ": " + std::string(test), std::move(joined)});
        };
        fail("read back", detail::diff_entries(*expected, *read));
        std::optional<std::string_view> expected_leaf;
        if (!expected->empty()) expected_leaf = detail::entry_id(expected->back());
        if (leaf != expected_leaf) {
            fail("leaf", {"- leaf " + std::string(expected_leaf.value_or("none")), "+ leaf " + std::string(leaf.value_or("none"))});
        }
        // A fork must carry the active branch; it may carry the other branches too, unchanged.
        std::set<std::string_view> required, carried;
        for (const auto* e : detail::branch(*expected, expected_leaf))
            if (auto id = detail::entry_id(*e)) required.insert(*id);
        for (const auto& e : fork_entries)
            if (auto id = detail::entry_id(e)) carried.insert(*id);
        std::vector<Json> expected_fork;
        for (const auto& e : *expected) {
            auto id = detail::entry_id(e);
            if (id && (required.contains(*id) || carried.contains(*id))) expected_fork.push_back(e);
        }
        fail("fork round trip", detail::diff_entries(expected_fork, fork_entries));
        co_return failures;
    }

    Awaitable<Res<std::vector<ToolSchema>>> tools(Path executable, std::vector<std::string> wanted) const {
        auto dir = workdir();
        if (!dir) co_return std::unexpected(dir.error());
        // Each refusal names one tool the version lacks; drop it and retry.
        while (!wanted.empty()) {
            std::string list;
            for (const auto& t : wanted) list += (list.empty() ? "" : ",") + t;
            std::vector<std::string> args{"--no-session", "--tools", list};
            auto client = co_await spawn(executable, std::move(args), dir->path());
            if (client) {
                auto state = co_await timed(get_state_json(*client), timeout);
                client->kill();
                if (!state) co_return std::unexpected(state.error());
                co_return parse_dump_tools(*state);
            }
            const auto* exited = client.error().get<rpc::RpcError::Exited>();
            if (!exited) co_return std::unexpected(client.error().message());
            auto tool = detail::refused_tool(exited->stderr);
            if (!tool || std::ranges::find(wanted, *tool) == wanted.end())
                co_return std::unexpected("omp refused to start: " + exited->stderr);
            std::erase(wanted, *tool);
        }
        co_return std::vector<ToolSchema>{};
    }

    Awaitable<Res<std::vector<Json>>> resume_and_fork(Path executable) const {
        const auto& golden = GOLDENS[0];
        auto resumed = co_await resume(std::move(executable), std::string(golden.name), golden.jsonl);
        if (!resumed) co_return std::unexpected(resumed.error());
        auto forked = co_await fork(*resumed);
        (*resumed)->rpc.kill();
        co_return forked;
    }

    // Reads the `ready` frame of `--mode rpc --no-session`; nothing is sent, so no model is called.
    Awaitable<Res<std::vector<std::uint32_t>>> protocol_versions(Path executable) const {
        auto dir = workdir();
        if (!dir) co_return std::unexpected(dir.error());
        std::vector<std::string> args{"--no-session"};
        auto client = co_await spawn(std::move(executable), std::move(args), dir->path());
        if (client) {
            client->kill();
            co_return client->ready().offered_versions();
        }
        if (const auto* version = client.error().get<rpc::RpcError::Version>()) co_return version->offered;
        co_return std::unexpected(client.error().message());
    }

    Awaitable<Res<std::vector<ContractFailure>>> run(Path executable) const {
        using namespace asio::experimental::awaitable_operators;
        auto [first, second] = co_await (timed(round_trip(executable, std::string(GOLDENS[0].name), GOLDENS[0].jsonl), timeout) &&
                                         timed(round_trip(executable, std::string(GOLDENS[1].name), GOLDENS[1].jsonl), timeout));
        std::vector<ContractFailure> failures;
        std::array<Res<std::vector<ContractFailure>>*, 2> results{&first, &second};
        for (std::size_t i = 0; i < GOLDENS.size(); ++i) {
            auto& result = *results[i];
            if (!result) co_return std::unexpected(std::string(GOLDENS[i].name) + ": " + result.error());
            std::ranges::move(*result, std::back_inserter(failures));
        }
        co_return failures;
    }
};

} // namespace

std::unique_ptr<loa::updater::Probe> OmpPorts::probe(const loa::updater::OmpGates& gates, std::chrono::milliseconds timeout) {
    std::vector<std::string> tools;
    for (const auto& [name, _] : gates.used_tools) tools.push_back(name);
    return std::make_unique<OmpProbe>(std::move(tools), timeout);
}

std::unique_ptr<loa::updater::ChangelogSummarizer> OmpPorts::reviewer(const std::filesystem::path& reviewer, const std::string& model,
                                                                      std::chrono::milliseconds timeout) {
    auto oneshot = std::make_unique<const loa::harness::omp::OneShot>(
        loa::harness::omp::OneShot(reviewer, std::filesystem::temp_directory_path()).with_timeout(timeout).with_model(model));
    return std::make_unique<SmolReviewer>(std::move(oneshot));
}

SmolReviewer::SmolReviewer(std::unique_ptr<const loa::contracts::Summarizer> summarizer)
    : summarizer_(std::move(summarizer)), exec_(std::make_unique<loa::runtime::Executor>(1)) {}

SmolReviewer::~SmolReviewer() = default;

loa::updater::PortResult<loa::updater::ChangelogReview> SmolReviewer::review(const std::string& changelog) {
    auto reply = block_on(*exec_, summarizer_->summarize(std::string(loa::updater::REVIEW_PROMPT), changelog));
    if (!reply) return std::unexpected(reply.error());
    return loa::updater::parse_review(*reply);
}

OmpProbe::OmpProbe(std::vector<std::string> tools, std::chrono::milliseconds timeout)
    : tools_(std::move(tools)), timeout_(timeout), exec_(std::make_unique<loa::runtime::Executor>(2)) {}

OmpProbe::~OmpProbe() = default;

loa::updater::PortResult<std::vector<ToolSchema>> OmpProbe::tools(const std::filesystem::path& executable) {
    return block_on(*exec_, Ctx{timeout_}.tools(executable, tools_));
}

loa::updater::PortResult<std::uint32_t> OmpProbe::session_version(const std::filesystem::path& executable) {
    const Ctx ctx{timeout_};
    auto forked = block_on(*exec_, timed(ctx.resume_and_fork(executable), timeout_));
    if (!forked) return std::unexpected(forked.error());
    auto header = std::ranges::find_if(*forked, [](const Json& l) {
        const auto* t = str_field(l, "type");
        return t && *t == "session";
    });
    if (header == forked->end()) return std::unexpected(std::string("forked session file has no session header"));
    const auto* v = field(*header, "version");
    auto version = v ? v->as_u64() : std::nullopt;
    if (!version) return std::unexpected(std::string("session header has no version"));
    if (*version > UINT32_MAX) return std::unexpected("session version " + std::to_string(*version) + " out of range");
    return static_cast<std::uint32_t>(*version);
}

loa::updater::PortResult<ConfigSchema> OmpProbe::config_keys(const std::filesystem::path& executable) {
    auto dir = workdir();
    if (!dir) return std::unexpected(dir.error());
    auto cmd = loa::runtime::exec::command(executable, {"config", "list", "--json"}, {}, dir->path());
    auto out = loa::runtime::exec::output(cmd, timeout_);
    if (!out) {
        if (out.error().code() == loa::ErrorCode::timeout) return std::unexpected("config list: " + timeout_text(timeout_));
        return std::unexpected("config list: " + out.error().to_string());
    }
    if (!out->status.success())
        return std::unexpected("config list: " + out->status.to_string() + ": " + first_line(out->err));
    return detail::parse_config_list(out->out);
}

loa::updater::PortResult<std::vector<std::uint32_t>> OmpProbe::protocol_versions(const std::filesystem::path& executable) {
    return block_on(*exec_, Ctx{timeout_}.protocol_versions(executable));
}

loa::updater::PortResult<std::vector<ContractFailure>> OmpProbe::run(const std::filesystem::path& executable) {
    return block_on(*exec_, Ctx{timeout_}.run(executable));
}

namespace detail {

std::span<const Golden> goldens() { return GOLDENS; }

std::optional<std::string> refused_tool(std::string_view stderr_text) {
    constexpr std::array<std::string_view, 2> MARKERS{"Unknown tool in --tools:", "Built-in tool unavailable in this session:"};
    for (auto line : lines(stderr_text)) {
        for (auto marker : MARKERS) {
            const auto at = line.find(marker);
            if (at == std::string_view::npos) continue;
            auto name = trim(line.substr(at + marker.size()));
            while (name.ends_with('.')) name.remove_suffix(1);
            name = trim(name);
            const bool spaced = name.find_first_of(" \t\r\n\v\f") != std::string_view::npos;
            if (!name.empty() && !spaced) return std::string(name);
            break;
        }
    }
    return std::nullopt;
}

std::expected<ConfigSchema, std::string> parse_config_list(std::string_view stdout_text) {
    auto parsed = json::parse(stdout_text);
    if (!parsed) return std::unexpected("config list is not a JSON object: " + parsed.error().message());
    const auto* map = parsed->as_object();
    if (!map) return std::unexpected(std::string("config list is not a JSON object: invalid type: expected a map"));
    ConfigSchema schema;
    for (const auto& [key, info] : *map) {
        schema.keys.insert(key);
        if (const auto* t = str_field(info, "type"); t && *t == "record") schema.open.insert(key);
    }
    return schema;
}

std::expected<std::vector<Json>, std::string> parse_lines(std::string_view text) {
    std::vector<Json> out;
    for (auto line : lines(text)) {
        if (trim(line).empty()) continue;
        auto parsed = json::parse(line);
        if (!parsed) return std::unexpected("invalid session line: " + parsed.error().message());
        out.push_back(std::move(*parsed));
    }
    return out;
}

std::optional<std::string_view> entry_id(const Json& entry) {
    if (const auto* id = str_field(entry, "id")) return *id;
    return std::nullopt;
}

bool is_entry(const Json& line) {
    if (!line.find("id")) return false;
    const auto* t = str_field(line, "type");
    return !(t && (*t == "session" || *t == "title"));
}

std::expected<std::vector<Json>, std::string> entries(std::string_view jsonl) {
    auto parsed = parse_lines(jsonl);
    if (!parsed) return std::unexpected(parsed.error());
    std::erase_if(*parsed, [](const Json& l) { return !is_entry(l); });
    return parsed;
}

std::expected<std::string, std::string> with_cwd(std::string_view jsonl, const std::filesystem::path& dir) {
    auto parsed = parse_lines(jsonl);
    if (!parsed) return std::unexpected(parsed.error());
    std::string out;
    out.reserve(jsonl.size());
    for (auto& line : *parsed) {
        const auto* t = str_field(line, "type");
        if (t && *t == "session" && line.is_object()) line.set("cwd", dir.string());
        out += json::dump(line);
        out += '\n';
    }
    return out;
}

std::vector<const Json*> branch(const std::vector<Json>& entries, std::optional<std::string_view> leaf) {
    std::map<std::string_view, const Json*> by_id;
    for (const auto& e : entries)
        if (auto id = entry_id(e)) by_id.emplace(*id, &e);
    std::vector<const Json*> path;
    std::set<std::string_view> seen;
    auto at = leaf;
    while (at) {
        auto found = by_id.find(*at);
        if (found == by_id.end() || !seen.insert(*at).second) break;
        path.push_back(found->second);
        at.reset();
        if (const auto* parent = str_field(*found->second, "parentId")) at = *parent;
    }
    std::ranges::reverse(path);
    return path;
}

std::vector<std::string> diff_entries(const std::vector<Json>& expected, const std::vector<Json>& actual) {
    std::map<std::string_view, const Json*> actual_by_id;
    for (const auto& e : actual)
        if (auto id = entry_id(e)) actual_by_id.emplace(*id, &e);
    std::vector<std::string> out;
    for (const auto& entry : expected) {
        const auto id = std::string(entry_id(entry).value_or("?"));
        auto found = actual_by_id.find(id);
        if (found == actual_by_id.end()) out.push_back("- entry " + id + " (" + kind(entry) + ") missing");
        else subset_diff(entry, *found->second, id, out);
    }
    std::set<std::string_view> expected_ids;
    for (const auto& e : expected)
        if (auto id = entry_id(e)) expected_ids.insert(*id);
    for (const auto& e : actual) {
        auto id = entry_id(e);
        if (id && !expected_ids.contains(*id)) out.push_back("+ entry " + std::string(*id) + " (" + kind(e) + ") unexpected");
    }
    return out;
}

} // namespace detail

} // namespace loa::app::adapters
