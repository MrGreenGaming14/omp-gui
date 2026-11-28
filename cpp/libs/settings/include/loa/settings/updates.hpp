#pragma once

// Releases, gate reports and peer standings the Updates screen shows. The serde-derived
// types convert to and from `loa::runtime::json` in the Rust field order and tagging.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/runtime/error.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/semver.hpp"

namespace loa::settings {

using Version = loa::runtime::semver::Version;
using Json = loa::runtime::json::Value;
template <typename T>
using JsonResult = std::expected<T, loa::Error>;

// What a product is to the app: decides gate checks and signature rules.
// JSON: `{"type":"app"}`, `{"type":"omp"}`, `{"type":"harness","harness":"<name>"}`.
struct ProductKind {
    enum class Kind : std::uint8_t { App, Omp, Harness } kind = Kind::App;
    std::string harness; // for `Harness`
    friend bool operator==(const ProductKind&, const ProductKind&) = default;
};

// Expected digest of a downloaded artifact, as hex. JSON: `{"algorithm":"sha256","hex":"…"}`.
struct Checksum {
    enum class Algorithm : std::uint8_t { Sha256, Sha512 } algorithm = Algorithm::Sha256;
    std::string hex;
    friend bool operator==(const Checksum&, const Checksum&) = default;
};

// How an artifact is unpacked; `NpmTarball` is a `.tgz` rooted at `package/`.
enum class ArchiveFormat : std::uint8_t { TarGz, Zip, NpmTarball, Raw };

// A downloadable file of a release.
struct Artifact {
    std::string name;
    std::string url;
    std::optional<std::uint64_t> size;
    std::optional<Checksum> checksum;
    ArchiveFormat format = ArchiveFormat::Raw;
    std::optional<std::string> signature_url;
    friend bool operator==(const Artifact&, const Artifact&) = default;
};

// A release of one product, as a feed reports it.
struct Release {
    std::string product;
    Version version;
    std::string notes;
    std::optional<std::string> published_at;
    std::optional<Artifact> artifact;
    friend bool operator==(const Release&, const Release&) = default;
};

// Rate limit state a feed reported.
struct RateLimit {
    std::uint64_t remaining = 0;
    std::uint64_t reset_unix = 0;
    friend bool operator==(const RateLimit&, const RateLimit&) = default;
};

// Which gate check a result belongs to.
enum class GateCheck : std::uint8_t { ToolSchemas, SessionFormat, ConfigSchema, RpcProtocol, ContractTests, Changelog };
[[nodiscard]] std::string_view label(GateCheck check) noexcept;

enum class Verdict : std::uint8_t { Pass, Harmful };

// Outcome of one check with readable lines explaining it.
struct CheckResult {
    GateCheck check = GateCheck::ToolSchemas;
    Verdict verdict = Verdict::Pass;
    std::vector<std::string> details;

    [[nodiscard]] static CheckResult pass(GateCheck check, std::vector<std::string> details);
    [[nodiscard]] static CheckResult harmful(GateCheck check, std::vector<std::string> details);
    // Harmful when `details` is not empty.
    [[nodiscard]] static CheckResult flag(GateCheck check, std::vector<std::string> details);
    // The check's port failed; counts as harmful.
    [[nodiscard]] static CheckResult failed_to_run(GateCheck check, std::string_view error);
    friend bool operator==(const CheckResult&, const CheckResult&) = default;
};

// All gate results for one staged update.
struct GateReport {
    std::string product;
    Version from;
    Version to;
    std::vector<CheckResult> checks;

    [[nodiscard]] bool is_clean() const noexcept;
    [[nodiscard]] std::vector<const CheckResult*> harmful() const;
    // The Rust `Display`: a header line, then one line per check and its details.
    [[nodiscard]] std::string to_string() const;
    friend bool operator==(const GateReport&, const GateReport&) = default;
};

// An update held by a harmful gate report.
struct HeldUpdate {
    Version version;
    GateReport report;
    std::uint64_t held_at_unix = 0;
    friend bool operator==(const HeldUpdate&, const HeldUpdate&) = default;
};

// Contents of a product's `active.json`.
struct Active {
    Version current;
    std::optional<Version> previous;
    friend bool operator==(const Active&, const Active&) = default;
};

// Where a product's version in use was found.
struct InstalledStatus {
    enum class Kind : std::uint8_t { Unknown, Running, Managed, System, Missing } kind = Kind::Unknown;
    std::filesystem::path path; // Managed, System
    std::string reason;         // Missing
    friend bool operator==(const InstalledStatus&, const InstalledStatus&) = default;
};

// Result of checking a staged artifact's signature.
struct SignatureStatus {
    enum class Kind : std::uint8_t { Verified, Unsigned, NoTrustedKeys, Rejected } kind = Kind::Unsigned;
    std::string reason; // Rejected
    friend bool operator==(const SignatureStatus&, const SignatureStatus&) = default;
};

// One product on the Updates screen.
struct ProductStatus {
    std::string id;
    std::string label;
    ProductKind kind;
    // The feed, e.g. `GitHub can1357/oh-my-pi`.
    std::string source;
    std::optional<Version> current;
    InstalledStatus installed;
    std::optional<Release> offer;
    std::optional<Version> newest;
    // Release notes between `current` and the offer, oldest first.
    std::string changelog;
    std::optional<std::uint64_t> last_check_unix;
    std::vector<Version> staged;
    std::optional<Active> active;
    std::optional<GateReport> report;
    std::optional<HeldUpdate> held;
    std::optional<Version> pending_swap;
    std::optional<Version> pin;
    std::optional<Version> waiting_past;
    std::optional<SignatureStatus> signature;
    bool restart_required = false;
    std::optional<std::string> error;
    friend bool operator==(const ProductStatus&, const ProductStatus&) = default;
};

// Snapshot of every product for the Updates screen.
struct Overview {
    std::vector<ProductStatus> products;
    std::optional<RateLimit> github_rate_limit;
    friend bool operator==(const Overview&, const Overview&) = default;
};

// The user's answer to a held update.
enum class HoldChoice : std::uint8_t {
    UpdateAnyway,
    // Stay on the current version until unpinned.
    PinCurrent,
    // Skip the held version and offer the next newer one.
    Wait,
};

// What this node knows about a peer.
struct PeerInfo {
    std::string node;
    std::optional<std::string> name;
    std::optional<Version> app_version;
    std::vector<std::string> alpns;
    friend bool operator==(const PeerInfo&, const PeerInfo&) = default;
};

// A peer's app version relative to this node's; `Behind` counts only the most significant differing part.
// JSON: `{"kind":"current"}`, `{"kind":"behind","majors":…,"minors":…,"patches":…}`, ….
struct Standing {
    enum class Kind : std::uint8_t { Current, Behind, Ahead, Unknown } kind = Kind::Unknown;
    std::uint64_t majors = 0, minors = 0, patches = 0; // Behind
    friend bool operator==(const Standing&, const Standing&) = default;
};

// One protocol family as both sides speak it.
struct ProtocolStatus {
    std::string family;
    // The highest version with an exact label announced by both sides.
    std::optional<std::uint32_t> shared;
    // The highest version this node announces, if any.
    std::optional<std::uint32_t> local;
    // The highest version the peer announces, if any.
    std::optional<std::uint32_t> peer;
    friend bool operator==(const ProtocolStatus&, const ProtocolStatus&) = default;
};

// One row of the Peers list.
struct PeerStanding {
    std::string node;
    std::optional<std::string> name;
    std::optional<Version> app_version;
    Standing standing;
    std::vector<ProtocolStatus> protocols;
    // True when every family this node speaks, and at least control and session, has the
    // same label on both sides.
    bool can_talk = false;
    friend bool operator==(const PeerStanding&, const PeerStanding&) = default;
};

// serde JSON forms.
[[nodiscard]] Json to_json(const ProductKind& v);
[[nodiscard]] Json to_json(const Checksum& v);
[[nodiscard]] Json to_json(ArchiveFormat v);
[[nodiscard]] Json to_json(const Artifact& v);
[[nodiscard]] Json to_json(const Release& v);
[[nodiscard]] Json to_json(const RateLimit& v);
[[nodiscard]] Json to_json(GateCheck v);
[[nodiscard]] Json to_json(Verdict v);
[[nodiscard]] Json to_json(const CheckResult& v);
[[nodiscard]] Json to_json(const GateReport& v);
[[nodiscard]] Json to_json(const HeldUpdate& v);
[[nodiscard]] Json to_json(const Active& v);
[[nodiscard]] Json to_json(const PeerInfo& v);
[[nodiscard]] Json to_json(const Standing& v);
[[nodiscard]] Json to_json(const ProtocolStatus& v);
[[nodiscard]] Json to_json(const PeerStanding& v);

template <typename T>
[[nodiscard]] JsonResult<T> from_json(const Json& v);

template <> JsonResult<ProductKind> from_json<ProductKind>(const Json& v);
template <> JsonResult<Checksum> from_json<Checksum>(const Json& v);
template <> JsonResult<ArchiveFormat> from_json<ArchiveFormat>(const Json& v);
template <> JsonResult<Artifact> from_json<Artifact>(const Json& v);
template <> JsonResult<Release> from_json<Release>(const Json& v);
template <> JsonResult<RateLimit> from_json<RateLimit>(const Json& v);
template <> JsonResult<GateCheck> from_json<GateCheck>(const Json& v);
template <> JsonResult<Verdict> from_json<Verdict>(const Json& v);
template <> JsonResult<CheckResult> from_json<CheckResult>(const Json& v);
template <> JsonResult<GateReport> from_json<GateReport>(const Json& v);
template <> JsonResult<HeldUpdate> from_json<HeldUpdate>(const Json& v);
template <> JsonResult<Active> from_json<Active>(const Json& v);
template <> JsonResult<PeerInfo> from_json<PeerInfo>(const Json& v);
template <> JsonResult<Standing> from_json<Standing>(const Json& v);
template <> JsonResult<ProtocolStatus> from_json<ProtocolStatus>(const Json& v);
template <> JsonResult<PeerStanding> from_json<PeerStanding>(const Json& v);

} // namespace loa::settings
