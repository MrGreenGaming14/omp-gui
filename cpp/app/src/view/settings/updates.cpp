#include "loa/app/view/settings/updates.hpp"

#include <utility>

namespace loa::app::view::settings {

namespace {

namespace domain = ::loa::settings;
namespace shown = ::loa::view::settings::updates;

std::optional<std::string> version(const std::optional<domain::Version>& v) {
    if (!v) return std::nullopt;
    return v->to_string();
}

shown::GateCheck check_to_view(domain::GateCheck check) {
    switch (check) {
    case domain::GateCheck::ToolSchemas: return shown::GateCheck::ToolSchemas;
    case domain::GateCheck::SessionFormat: return shown::GateCheck::SessionFormat;
    case domain::GateCheck::ConfigSchema: return shown::GateCheck::ConfigSchema;
    case domain::GateCheck::RpcProtocol: return shown::GateCheck::RpcProtocol;
    case domain::GateCheck::ContractTests: return shown::GateCheck::ContractTests;
    case domain::GateCheck::Changelog: return shown::GateCheck::Changelog;
    }
    return shown::GateCheck::ToolSchemas;
}

shown::GateReport report_to_view(const domain::GateReport& r) {
    shown::GateReport out{.from = r.from.to_string(), .to = r.to.to_string(), .checks = {}};
    out.checks.reserve(r.checks.size());
    for (const auto& c : r.checks) {
        out.checks.push_back(shown::CheckResult{
            .check = check_to_view(c.check),
            .verdict = c.verdict == domain::Verdict::Harmful ? shown::Verdict::Harmful : shown::Verdict::Pass,
            .details = c.details,
        });
    }
    return out;
}

shown::ProductKind kind_to_view(const domain::ProductKind& kind) {
    switch (kind.kind) {
    case domain::ProductKind::Kind::App: return shown::product_kind::App{};
    case domain::ProductKind::Kind::Omp: return shown::product_kind::Omp{};
    case domain::ProductKind::Kind::Harness: return shown::product_kind::Harness{kind.harness};
    }
    return shown::product_kind::App{};
}

shown::Installed installed_to_view(const domain::InstalledStatus& installed) {
    using Kind = domain::InstalledStatus::Kind;
    switch (installed.kind) {
    case Kind::Unknown: return shown::installed::Unknown{};
    case Kind::Running: return shown::installed::Running{};
    case Kind::Managed: return shown::installed::Managed{installed.path.string()};
    case Kind::System: return shown::installed::System{installed.path.string()};
    case Kind::Missing: return shown::installed::Missing{installed.reason};
    }
    return shown::installed::Unknown{};
}

shown::SignatureStatus signature_to_view(const domain::SignatureStatus& s) {
    using Kind = domain::SignatureStatus::Kind;
    switch (s.kind) {
    case Kind::Verified: return shown::signature_status::Verified{};
    case Kind::Unsigned: return shown::signature_status::Unsigned{};
    case Kind::NoTrustedKeys: return shown::signature_status::NoTrustedKeys{};
    case Kind::Rejected: return shown::signature_status::Rejected{s.reason};
    }
    return shown::signature_status::Unsigned{};
}

shown::Standing standing_to_view(const domain::Standing& s) {
    using Kind = domain::Standing::Kind;
    switch (s.kind) {
    case Kind::Current: return shown::standing::Current{};
    case Kind::Behind: return shown::standing::Behind{s.majors, s.minors, s.patches};
    case Kind::Ahead: return shown::standing::Ahead{};
    case Kind::Unknown: return shown::standing::Unknown{};
    }
    return shown::standing::Unknown{};
}

} // namespace

shown::Product product_to_view(const domain::ProductStatus& p) {
    shown::Product out{
        .id = p.id,
        .label = p.label,
        .kind = kind_to_view(p.kind),
        .source = p.source,
        .current = version(p.current),
        .installed = installed_to_view(p.installed),
        .offer = p.offer ? std::optional<std::string>(p.offer->version.to_string()) : std::nullopt,
        .newest = version(p.newest),
        .changelog = p.changelog,
        .last_check_unix = p.last_check_unix,
        .staged = {},
        .active = std::nullopt,
        .report = std::nullopt,
        .held = std::nullopt,
        .pending_swap = version(p.pending_swap),
        .pin = version(p.pin),
        .waiting_past = version(p.waiting_past),
        .signature = std::nullopt,
        .restart_required = p.restart_required,
        .error = p.error,
    };
    out.staged.reserve(p.staged.size());
    for (const auto& v : p.staged) out.staged.push_back(v.to_string());
    if (p.active) out.active = shown::Active{p.active->current.to_string(), version(p.active->previous)};
    if (p.report) out.report = report_to_view(*p.report);
    if (p.held) out.held = shown::HeldUpdate{p.held->version.to_string(), report_to_view(p.held->report)};
    if (p.signature) out.signature = signature_to_view(*p.signature);
    return out;
}

shown::PeerStanding peer_to_view(const domain::PeerStanding& p) {
    shown::PeerStanding out{
        .node = p.node,
        .name = p.name,
        .app_version = version(p.app_version),
        .standing = standing_to_view(p.standing),
        .protocols = {},
        .can_talk = p.can_talk,
    };
    out.protocols.reserve(p.protocols.size());
    for (const auto& s : p.protocols) out.protocols.push_back(shown::ProtocolStatus{s.family, s.shared});
    return out;
}

domain::HoldChoice hold_choice_from_view(shown::HoldChoice choice) noexcept {
    switch (choice) {
    case shown::HoldChoice::UpdateAnyway: return domain::HoldChoice::UpdateAnyway;
    case shown::HoldChoice::PinCurrent: return domain::HoldChoice::PinCurrent;
    case shown::HoldChoice::Wait: return domain::HoldChoice::Wait;
    }
    return domain::HoldChoice::Wait;
}

} // namespace loa::app::view::settings
