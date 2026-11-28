#pragma once

// The Updates screen: what each watched product runs and what its feed offers,
// staged versions, gate reports with the choices of a held update, rollback, app
// signatures and the peers that are behind. Versions are their semver text.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace loa::view::settings::updates {

// What a product is to the app.
namespace product_kind {
struct App {
    friend bool operator==(const App&, const App&) = default;
};
struct Omp {
    friend bool operator==(const Omp&, const Omp&) = default;
};
struct Harness {
    std::string harness;
    friend bool operator==(const Harness&, const Harness&) = default;
};
} // namespace product_kind

using ProductKind = std::variant<product_kind::App, product_kind::Omp, product_kind::Harness>;

// Where a product's version in use was found.
namespace installed {
struct Unknown {
    friend bool operator==(const Unknown&, const Unknown&) = default;
};
// This app.
struct Running {
    friend bool operator==(const Running&, const Running&) = default;
};
// Installed by the updater at this path.
struct Managed {
    std::string path;
    friend bool operator==(const Managed&, const Managed&) = default;
};
// Found on the system at this path.
struct System {
    std::string path;
    friend bool operator==(const System&, const System&) = default;
};
struct Missing {
    std::string path;
    friend bool operator==(const Missing&, const Missing&) = default;
};
} // namespace installed

using Installed =
    std::variant<installed::Unknown, installed::Running, installed::Managed, installed::System, installed::Missing>;

// Result of checking a staged app release's signature.
namespace signature_status {
struct Verified {
    friend bool operator==(const Verified&, const Verified&) = default;
};
struct Unsigned {
    friend bool operator==(const Unsigned&, const Unsigned&) = default;
};
struct NoTrustedKeys {
    friend bool operator==(const NoTrustedKeys&, const NoTrustedKeys&) = default;
};
struct Rejected {
    std::string reason;
    friend bool operator==(const Rejected&, const Rejected&) = default;
};
} // namespace signature_status

using SignatureStatus = std::variant<signature_status::Verified, signature_status::Unsigned,
                                     signature_status::NoTrustedKeys, signature_status::Rejected>;

// The version in use and the one a rollback returns to.
struct Active {
    std::string current;
    std::optional<std::string> previous;

    friend bool operator==(const Active&, const Active&) = default;
};

// Which gate check a result belongs to.
enum class GateCheck : std::uint8_t { ToolSchemas, SessionFormat, ConfigSchema, RpcProtocol, ContractTests, Changelog };

// "Tool schemas", "Session format version", ...
[[nodiscard]] std::string_view label(GateCheck check) noexcept;

enum class Verdict : std::uint8_t { Pass, Harmful };

// Outcome of one gate check with readable lines explaining it.
struct CheckResult {
    GateCheck check = GateCheck::ToolSchemas;
    Verdict verdict = Verdict::Pass;
    std::vector<std::string> details;

    friend bool operator==(const CheckResult&, const CheckResult&) = default;
};

// All gate results for one staged update.
struct GateReport {
    std::string from;
    std::string to;
    std::vector<CheckResult> checks;

    // The harmful checks, in order.
    [[nodiscard]] std::vector<const CheckResult*> harmful() const;

    friend bool operator==(const GateReport&, const GateReport&) = default;
};

// An update held by a harmful gate report.
struct HeldUpdate {
    std::string version;
    GateReport report;

    friend bool operator==(const HeldUpdate&, const HeldUpdate&) = default;
};

// One product on the Updates screen.
struct Product {
    std::string id;
    std::string label;
    ProductKind kind;
    // The feed, e.g. `GitHub can1357/oh-my-pi`.
    std::string source;
    std::optional<std::string> current;
    Installed installed;
    // The version the feed offers.
    std::optional<std::string> offer;
    std::optional<std::string> newest;
    // Release notes between `current` and the offer, oldest first.
    std::string changelog;
    std::optional<std::uint64_t> last_check_unix;
    std::vector<std::string> staged;
    std::optional<Active> active;
    std::optional<GateReport> report;
    std::optional<HeldUpdate> held;
    std::optional<std::string> pending_swap;
    std::optional<std::string> pin;
    std::optional<std::string> waiting_past;
    std::optional<SignatureStatus> signature;
    bool restart_required = false;
    std::optional<std::string> error;

    friend bool operator==(const Product&, const Product&) = default;
};

// The user's answer to a held update.
enum class HoldChoice : std::uint8_t {
    UpdateAnyway,
    // Stay on the current version until unpinned.
    PinCurrent,
    // Skip the held version and offer the next newer one.
    Wait,
};

// A peer's app version relative to this node's; `Behind` counts only the most significant differing part.
namespace standing {
struct Current {
    friend bool operator==(const Current&, const Current&) = default;
};
struct Behind {
    std::uint64_t majors = 0;
    std::uint64_t minors = 0;
    std::uint64_t patches = 0;
    friend bool operator==(const Behind&, const Behind&) = default;
};
struct Ahead {
    friend bool operator==(const Ahead&, const Ahead&) = default;
};
struct Unknown {
    friend bool operator==(const Unknown&, const Unknown&) = default;
};
} // namespace standing

using Standing = std::variant<standing::Current, standing::Behind, standing::Ahead, standing::Unknown>;

// One protocol family and the highest version both sides speak.
struct ProtocolStatus {
    std::string family;
    std::optional<std::uint32_t> shared;

    friend bool operator==(const ProtocolStatus&, const ProtocolStatus&) = default;
};

// One row of the peers list.
struct PeerStanding {
    std::string node;
    std::optional<std::string> name;
    std::optional<std::string> app_version;
    Standing standing;
    std::vector<ProtocolStatus> protocols;
    // Whether the peer and this node share the protocols they need to talk.
    bool can_talk = false;

    friend bool operator==(const PeerStanding&, const PeerStanding&) = default;
};

// The app's snapshot the screen shows.
struct Data {
    std::vector<Product> products;
    // GitHub API requests left in the current window, when known.
    std::optional<std::uint64_t> github_requests_left;
    std::vector<PeerStanding> peers;
    // Whether a session is mid turn: swaps wait for it.
    bool turn_running = false;
    // Current Unix time, for "checked 5m ago".
    std::uint64_t now_unix = 0;

    friend bool operator==(const Data&, const Data&) = default;
};

// A request for the app's updater; every variant but `CheckAll` names a product id.
namespace intent {
struct CheckAll {
    friend bool operator==(const CheckAll&, const CheckAll&) = default;
};
struct Check {
    std::string product;
    friend bool operator==(const Check&, const Check&) = default;
};
// Download and unpack the offer beside the version in use.
struct Stage {
    std::string product;
    friend bool operator==(const Stage&, const Stage&) = default;
};
// Run the staged offer's gate checks.
struct Gate {
    std::string product;
    friend bool operator==(const Gate&, const Gate&) = default;
};
// Check the staged app release's signature.
struct Verify {
    std::string product;
    friend bool operator==(const Verify&, const Verify&) = default;
};
// The answer to a held update.
struct Choose {
    std::string product;
    HoldChoice choice = HoldChoice::Wait;
    friend bool operator==(const Choose&, const Choose&) = default;
};
// Swap in the pending version (between turns only).
struct Swap {
    std::string product;
    friend bool operator==(const Swap&, const Swap&) = default;
};
struct Rollback {
    std::string product;
    friend bool operator==(const Rollback&, const Rollback&) = default;
};
// Drop a product's pin and wait.
struct Unpin {
    std::string product;
    friend bool operator==(const Unpin&, const Unpin&) = default;
};
} // namespace intent

using Intent = std::variant<intent::CheckAll, intent::Check, intent::Stage, intent::Gate, intent::Verify,
                            intent::Choose, intent::Swap, intent::Rollback, intent::Unpin>;

} // namespace loa::view::settings::updates
