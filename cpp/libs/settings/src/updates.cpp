#include "loa/settings/updates.hpp"

#include <algorithm>

namespace loa::settings {

namespace json = loa::runtime::json;
using json::Array;
using json::Object;

std::string_view label(GateCheck check) noexcept {
    switch (check) {
    case GateCheck::ToolSchemas: return "Tool schemas";
    case GateCheck::SessionFormat: return "Session format version";
    case GateCheck::ConfigSchema: return "Config schema";
    case GateCheck::RpcProtocol: return "RPC protocol version";
    case GateCheck::ContractTests: return "Adapter contract tests";
    case GateCheck::Changelog: return "Changelog";
    }
    return "";
}

CheckResult CheckResult::pass(GateCheck check, std::vector<std::string> details) {
    return {check, Verdict::Pass, std::move(details)};
}

CheckResult CheckResult::harmful(GateCheck check, std::vector<std::string> details) {
    return {check, Verdict::Harmful, std::move(details)};
}

CheckResult CheckResult::flag(GateCheck check, std::vector<std::string> details) {
    return details.empty() ? pass(check, std::move(details)) : harmful(check, std::move(details));
}

CheckResult CheckResult::failed_to_run(GateCheck check, std::string_view error) {
    return harmful(check, {"check could not run: " + std::string(error)});
}

bool GateReport::is_clean() const noexcept {
    return std::ranges::all_of(checks, [](const CheckResult& c) { return c.verdict == Verdict::Pass; });
}

std::vector<const CheckResult*> GateReport::harmful() const {
    std::vector<const CheckResult*> out;
    for (const auto& c : checks)
        if (c.verdict == Verdict::Harmful) out.push_back(&c);
    return out;
}

std::string GateReport::to_string() const {
    std::string s = product + " " + from.to_string() + " -> " + to.to_string() + "\n";
    for (const auto& c : checks) {
        s += "  " + std::string(label(c.check)) + ": " + (c.verdict == Verdict::Pass ? "pass" : "HARMFUL") + "\n";
        for (const auto& line : c.details) s += "    " + line + "\n";
    }
    return s;
}

// ---- JSON ----

namespace {

struct JsonError {
    std::string message;
};

[[noreturn]] void fail(std::string message) { throw JsonError{std::move(message)}; }

Json opt(const std::optional<std::string>& v) { return v ? Json(*v) : Json(); }
Json opt(const std::optional<std::uint64_t>& v) { return v ? Json(*v) : Json(); }
Json opt(const std::optional<std::uint32_t>& v) { return v ? Json(std::uint64_t{*v}) : Json(); }
Json opt(const std::optional<Version>& v) { return v ? Json(v->to_string()) : Json(); }
Json strings(const std::vector<std::string>& v) {
    Array a;
    for (const auto& s : v) a.emplace_back(s);
    return a;
}

const Object& object(const Json& v, std::string_view what) {
    if (auto* o = v.as_object()) return *o;
    fail("invalid type, expected " + std::string(what));
}

const Json* field(const Object& o, std::string_view key) {
    for (const auto& [k, v] : o)
        if (k == key) return &v;
    return nullptr;
}

const Json& required(const Object& o, std::string_view key) {
    const Json* v = field(o, key);
    if (!v) fail("missing field `" + std::string(key) + "`");
    return *v;
}

// A field that may be missing or null.
const Json* optional(const Object& o, std::string_view key) {
    const Json* v = field(o, key);
    return v && !v->is_null() ? v : nullptr;
}

std::string str(const Json& v) {
    if (auto* s = v.as_string()) return *s;
    fail("invalid type, expected a string");
}

std::uint64_t u64(const Json& v) {
    if (auto n = v.as_u64()) return *n;
    fail("invalid type, expected u64");
}

bool boolean(const Json& v) {
    if (auto* b = v.as_bool()) return *b;
    fail("invalid type, expected a boolean");
}

Version version(const Json& v) {
    auto parsed = Version::parse(str(v));
    if (!parsed) fail(parsed.error().message());
    return *parsed;
}

std::vector<std::string> string_list(const Json& v) {
    auto* a = v.as_array();
    if (!a) fail("invalid type, expected a sequence");
    std::vector<std::string> out;
    for (const auto& e : *a) out.push_back(str(e));
    return out;
}

template <typename T>
T get(const Json& v) {
    auto r = from_json<T>(v);
    if (!r) fail(r.error().message());
    return std::move(*r);
}

template <typename T, typename F>
JsonResult<T> guarded(F f) {
    try {
        return f();
    } catch (JsonError& e) {
        return std::unexpected(loa::Error(loa::ErrorCode::parse, std::move(e.message)));
    }
}

constexpr std::string_view GATE_CHECKS[] = {"tool_schemas", "session_format", "config_schema",
                                            "rpc_protocol", "contract_tests", "changelog"};
constexpr std::string_view FORMATS[] = {"tar_gz", "zip", "npm_tarball", "raw"};

} // namespace

Json to_json(const ProductKind& v) {
    switch (v.kind) {
    case ProductKind::Kind::App: return Object{{"type", "app"}};
    case ProductKind::Kind::Omp: return Object{{"type", "omp"}};
    case ProductKind::Kind::Harness: return Object{{"type", "harness"}, {"harness", v.harness}};
    }
    return {};
}

Json to_json(const Checksum& v) {
    return Object{{"algorithm", v.algorithm == Checksum::Algorithm::Sha256 ? "sha256" : "sha512"}, {"hex", v.hex}};
}

Json to_json(ArchiveFormat v) { return Json(FORMATS[static_cast<int>(v)]); }

Json to_json(const Artifact& v) {
    return Object{{"name", v.name},
                  {"url", v.url},
                  {"size", opt(v.size)},
                  {"checksum", v.checksum ? to_json(*v.checksum) : Json()},
                  {"format", to_json(v.format)},
                  {"signature_url", opt(v.signature_url)}};
}

Json to_json(const Release& v) {
    return Object{{"product", v.product},
                  {"version", v.version.to_string()},
                  {"notes", v.notes},
                  {"published_at", opt(v.published_at)},
                  {"artifact", v.artifact ? to_json(*v.artifact) : Json()}};
}

Json to_json(const RateLimit& v) { return Object{{"remaining", v.remaining}, {"reset_unix", v.reset_unix}}; }

Json to_json(GateCheck v) { return Json(GATE_CHECKS[static_cast<int>(v)]); }

Json to_json(Verdict v) { return Json(v == Verdict::Pass ? "pass" : "harmful"); }

Json to_json(const CheckResult& v) {
    return Object{{"check", to_json(v.check)}, {"verdict", to_json(v.verdict)}, {"details", strings(v.details)}};
}

Json to_json(const GateReport& v) {
    Array checks;
    for (const auto& c : v.checks) checks.push_back(to_json(c));
    return Object{{"product", v.product},
                  {"from", v.from.to_string()},
                  {"to", v.to.to_string()},
                  {"checks", std::move(checks)}};
}

Json to_json(const HeldUpdate& v) {
    return Object{{"version", v.version.to_string()}, {"report", to_json(v.report)}, {"held_at_unix", v.held_at_unix}};
}

Json to_json(const Active& v) { return Object{{"current", v.current.to_string()}, {"previous", opt(v.previous)}}; }

Json to_json(const PeerInfo& v) {
    return Object{{"node", v.node}, {"name", opt(v.name)}, {"app_version", opt(v.app_version)}, {"alpns", strings(v.alpns)}};
}

Json to_json(const Standing& v) {
    switch (v.kind) {
    case Standing::Kind::Current: return Object{{"kind", "current"}};
    case Standing::Kind::Behind:
        return Object{{"kind", "behind"}, {"majors", v.majors}, {"minors", v.minors}, {"patches", v.patches}};
    case Standing::Kind::Ahead: return Object{{"kind", "ahead"}};
    case Standing::Kind::Unknown: return Object{{"kind", "unknown"}};
    }
    return {};
}

Json to_json(const ProtocolStatus& v) {
    return Object{{"family", v.family}, {"shared", opt(v.shared)}, {"local", opt(v.local)}, {"peer", opt(v.peer)}};
}

Json to_json(const PeerStanding& v) {
    Array protocols;
    for (const auto& p : v.protocols) protocols.push_back(to_json(p));
    return Object{{"node", v.node},
                  {"name", opt(v.name)},
                  {"app_version", opt(v.app_version)},
                  {"standing", to_json(v.standing)},
                  {"protocols", std::move(protocols)},
                  {"can_talk", v.can_talk}};
}

template <>
JsonResult<ProductKind> from_json<ProductKind>(const Json& v) {
    return guarded<ProductKind>([&] {
        const Object& o = object(v, "enum ProductKind");
        std::string type = str(required(o, "type"));
        if (type == "app") return ProductKind{ProductKind::Kind::App, {}};
        if (type == "omp") return ProductKind{ProductKind::Kind::Omp, {}};
        if (type == "harness") return ProductKind{ProductKind::Kind::Harness, str(required(o, "harness"))};
        fail("unknown variant `" + type + "`, expected one of `app`, `omp`, `harness`");
    });
}

template <>
JsonResult<Checksum> from_json<Checksum>(const Json& v) {
    return guarded<Checksum>([&] {
        const Object& o = object(v, "enum Checksum");
        std::string algorithm = str(required(o, "algorithm"));
        Checksum out;
        if (algorithm == "sha256") out.algorithm = Checksum::Algorithm::Sha256;
        else if (algorithm == "sha512") out.algorithm = Checksum::Algorithm::Sha512;
        else fail("unknown variant `" + algorithm + "`, expected `sha256` or `sha512`");
        out.hex = str(required(o, "hex"));
        return out;
    });
}

template <>
JsonResult<ArchiveFormat> from_json<ArchiveFormat>(const Json& v) {
    return guarded<ArchiveFormat>([&] {
        std::string s = str(v);
        for (int i = 0; i < 4; ++i)
            if (s == FORMATS[i]) return static_cast<ArchiveFormat>(i);
        fail("unknown variant `" + s + "`");
    });
}

template <>
JsonResult<Artifact> from_json<Artifact>(const Json& v) {
    return guarded<Artifact>([&] {
        const Object& o = object(v, "struct Artifact");
        Artifact out;
        out.name = str(required(o, "name"));
        out.url = str(required(o, "url"));
        if (auto* s = optional(o, "size")) out.size = u64(*s);
        if (auto* c = optional(o, "checksum")) out.checksum = get<Checksum>(*c);
        out.format = get<ArchiveFormat>(required(o, "format"));
        if (auto* s = optional(o, "signature_url")) out.signature_url = str(*s);
        return out;
    });
}

template <>
JsonResult<Release> from_json<Release>(const Json& v) {
    return guarded<Release>([&] {
        const Object& o = object(v, "struct Release");
        Release out;
        out.product = str(required(o, "product"));
        out.version = version(required(o, "version"));
        if (auto* n = field(o, "notes")) out.notes = str(*n);
        if (auto* p = optional(o, "published_at")) out.published_at = str(*p);
        if (auto* a = optional(o, "artifact")) out.artifact = get<Artifact>(*a);
        return out;
    });
}

template <>
JsonResult<RateLimit> from_json<RateLimit>(const Json& v) {
    return guarded<RateLimit>([&] {
        const Object& o = object(v, "struct RateLimit");
        return RateLimit{u64(required(o, "remaining")), u64(required(o, "reset_unix"))};
    });
}

template <>
JsonResult<GateCheck> from_json<GateCheck>(const Json& v) {
    return guarded<GateCheck>([&] {
        std::string s = str(v);
        for (int i = 0; i < 6; ++i)
            if (s == GATE_CHECKS[i]) return static_cast<GateCheck>(i);
        fail("unknown variant `" + s + "`");
    });
}

template <>
JsonResult<Verdict> from_json<Verdict>(const Json& v) {
    return guarded<Verdict>([&] {
        std::string s = str(v);
        if (s == "pass") return Verdict::Pass;
        if (s == "harmful") return Verdict::Harmful;
        fail("unknown variant `" + s + "`, expected `pass` or `harmful`");
    });
}

template <>
JsonResult<CheckResult> from_json<CheckResult>(const Json& v) {
    return guarded<CheckResult>([&] {
        const Object& o = object(v, "struct CheckResult");
        return CheckResult{get<GateCheck>(required(o, "check")), get<Verdict>(required(o, "verdict")),
                           string_list(required(o, "details"))};
    });
}

template <>
JsonResult<GateReport> from_json<GateReport>(const Json& v) {
    return guarded<GateReport>([&] {
        const Object& o = object(v, "struct GateReport");
        GateReport out;
        out.product = str(required(o, "product"));
        out.from = version(required(o, "from"));
        out.to = version(required(o, "to"));
        auto* checks = required(o, "checks").as_array();
        if (!checks) fail("invalid type, expected a sequence");
        for (const auto& c : *checks) out.checks.push_back(get<CheckResult>(c));
        return out;
    });
}

template <>
JsonResult<HeldUpdate> from_json<HeldUpdate>(const Json& v) {
    return guarded<HeldUpdate>([&] {
        const Object& o = object(v, "struct HeldUpdate");
        return HeldUpdate{version(required(o, "version")), get<GateReport>(required(o, "report")),
                          u64(required(o, "held_at_unix"))};
    });
}

template <>
JsonResult<Active> from_json<Active>(const Json& v) {
    return guarded<Active>([&] {
        const Object& o = object(v, "struct Active");
        Active out{version(required(o, "current")), std::nullopt};
        if (auto* p = optional(o, "previous")) out.previous = version(*p);
        return out;
    });
}

template <>
JsonResult<PeerInfo> from_json<PeerInfo>(const Json& v) {
    return guarded<PeerInfo>([&] {
        const Object& o = object(v, "struct PeerInfo");
        PeerInfo out;
        out.node = str(required(o, "node"));
        if (auto* n = optional(o, "name")) out.name = str(*n);
        if (auto* a = optional(o, "app_version")) out.app_version = version(*a);
        out.alpns = string_list(required(o, "alpns"));
        return out;
    });
}

template <>
JsonResult<Standing> from_json<Standing>(const Json& v) {
    return guarded<Standing>([&] {
        const Object& o = object(v, "enum Standing");
        std::string kind = str(required(o, "kind"));
        Standing out;
        if (kind == "current") out.kind = Standing::Kind::Current;
        else if (kind == "ahead") out.kind = Standing::Kind::Ahead;
        else if (kind == "unknown") out.kind = Standing::Kind::Unknown;
        else if (kind == "behind") {
            out.kind = Standing::Kind::Behind;
            out.majors = u64(required(o, "majors"));
            out.minors = u64(required(o, "minors"));
            out.patches = u64(required(o, "patches"));
        } else fail("unknown variant `" + kind + "`, expected one of `current`, `behind`, `ahead`, `unknown`");
        return out;
    });
}

template <>
JsonResult<ProtocolStatus> from_json<ProtocolStatus>(const Json& v) {
    return guarded<ProtocolStatus>([&] {
        const Object& o = object(v, "struct ProtocolStatus");
        ProtocolStatus out;
        out.family = str(required(o, "family"));
        auto u32 = [&](std::string_view key) -> std::optional<std::uint32_t> {
            const Json* x = optional(o, key);
            if (!x) return std::nullopt;
            std::uint64_t n = u64(*x);
            if (n > UINT32_MAX) fail("invalid value, expected u32");
            return static_cast<std::uint32_t>(n);
        };
        out.shared = u32("shared");
        out.local = u32("local");
        out.peer = u32("peer");
        return out;
    });
}

template <>
JsonResult<PeerStanding> from_json<PeerStanding>(const Json& v) {
    return guarded<PeerStanding>([&] {
        const Object& o = object(v, "struct PeerStanding");
        PeerStanding out;
        out.node = str(required(o, "node"));
        if (auto* n = optional(o, "name")) out.name = str(*n);
        if (auto* a = optional(o, "app_version")) out.app_version = version(*a);
        out.standing = get<Standing>(required(o, "standing"));
        auto* protocols = required(o, "protocols").as_array();
        if (!protocols) fail("invalid type, expected a sequence");
        for (const auto& p : *protocols) out.protocols.push_back(get<ProtocolStatus>(p));
        out.can_talk = boolean(required(o, "can_talk"));
        return out;
    });
}

} // namespace loa::settings
