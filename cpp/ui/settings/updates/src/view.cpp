// Qt view of the updates screen. Port of the view half of `crates/loa-ui-settings/src/updates.rs`.

#include "loa/ui/settings/updates/view.hpp"

#include <algorithm>
#include <format>

#include <QFrame>
#include <QLabel>
#include <QPushButton>
#include <QScrollArea>
#include <QVBoxLayout>

#include "loa/runtime/time.hpp"
#include "loa/ui/kit/text.hpp"
#include "settings_widgets.hpp"
#include "strings.hpp"

namespace loa::ui::settings::updates {

using namespace sw;
using loa::core::as_str;
namespace in = vu::intent;

namespace {

// Detail lines shown per gate check before the rest is folded.
constexpr std::size_t DETAIL_LINES = 12;
// Release note lines shown before the rest is folded.
constexpr std::size_t NOTE_LINES = 16;

QLabel* caption(std::string_view s, Tone tone = Tone::Muted) { return kw::label(s, TextRole::Caption, tone); }
QLabel* dot(Tone tone) {
    auto* d = kw::label("\u25cf", TextRole::Caption, tone);
    d->setAccessibleName(QStringLiteral("status"));
    return d;
}

// A fact name in the label column beside its value.
QWidget* labeled(std::string_view name, std::string_view value) {
    auto* n = kw::label(name, TextRole::Caption, Tone::Muted);
    n->setFixedWidth(px(theme::width(LayoutRole::LabelColumn)));
    return hrow({n, caption(value, Tone::Normal)});
}

std::vector<std::string_view> lines_of(std::string_view s) {
    std::vector<std::string_view> out;
    while (!s.empty()) {
        const auto nl = s.find('\n');
        auto line = s.substr(0, nl);
        if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
        out.push_back(line);
        if (nl == std::string_view::npos) break;
        s.remove_prefix(nl + 1);
    }
    return out;
}

// An optional heading over the first `shown` of `lines`, then how many more there are.
QWidget* folded(std::optional<std::string_view> head, const std::vector<std::string_view>& lines, std::size_t shown, std::string_view unit) {
    auto* col = vcol({head ? kw::label(*head, TextRole::Label, Tone::Normal) : nullptr});
    for (std::size_t i = 0; i < std::min(shown, lines.size()); ++i) add(col, caption(lines[i]));
    if (lines.size() > shown) add(col, caption(std::format("\u2026 {} more{}", lines.size() - shown, unit)));
    return col;
}

QWidget* report(const vu::GateReport& r) {
    const auto harmful = r.harmful().size();
    const auto n = r.checks.size();
    const auto [tone, verdict] = n == 0         ? std::pair{Tone::Ok, std::string("No gate checks apply; the signature is checked at swap.")}
                                 : harmful == 0 ? std::pair{Tone::Ok, std::format("All {} checks passed.", n)}
                                                : std::pair{Tone::Danger, std::format("{} of {} checks found something harmful.", harmful, n)};
    auto* col = vcol({hrow({kw::label(std::format("Gate report {} \u2192 {}", r.from, r.to), TextRole::Label, Tone::Normal),
                            kw::label(verdict, TextRole::Label, tone)},
                           spi(SpaceRole::M))},
                     spi(SpaceRole::S));
    for (const auto& c : r.checks) {
        const bool pass = c.verdict == vu::Verdict::Pass;
        const Tone t = pass ? Tone::Ok : Tone::Danger;
        auto* icon = new QLabel;
        icon->setPixmap(kit::tinted(pass ? kit::Icon::Check : kit::Icon::TriangleAlert, TextRole::Caption, t));
        icon->setAccessibleName(QString::fromLatin1(pass ? "pass" : "harmful"));
        add(col, hrow({icon, caption(vu::label(c.check), Tone::Normal),
                       caption(pass ? "pass" : "harmful", t)},
                      spi(SpaceRole::Xs)));
        if (!c.details.empty()) {
            std::vector<std::string_view> details(c.details.begin(), c.details.end());
            auto* d = folded(std::nullopt, details, DETAIL_LINES, "");
            d->layout()->setContentsMargins(spi(SpaceRole::L), 0, 0, 0);
            add(col, d);
        }
    }
    return col;
}

} // namespace

UpdatesView::UpdatesView(QWidget* parent) : QWidget(parent) {
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(0, 0, 0, 0);
    setAccessibleName(QStringLiteral("updates settings"));
}

void UpdatesView::render(const State& state) {
    const auto& d = state.data;
    auto post = [this](Message m) { emit message(m); };
    auto start = [post](Intent e) -> Callback { return [post, e] { post(Message::local(local::Start{e})); }; };
    auto choose = [post](const std::string& id, HoldChoice c) -> Callback {
        return [post, id, c] { post(Message::event(in::Choose{id, c})); };
    };

    auto actions = [&](const Product& p) -> QWidget* {
        if (const auto step = state.busy(p.id)) return caption(as_str(*step));
        const auto& id = p.id;
        auto* row = hrow({}, spi(SpaceRole::Xs));
        const bool staged_offer = p.offer && std::ranges::find(p.staged, *p.offer) != p.staged.end();
        if (p.held) {
            add(row, action(std::format("Update anyway to {}", p.held->version), ButtonKind::danger(), choose(id, HoldChoice::UpdateAnyway)));
            if (p.current) add(row, action(std::format("Pin {}", *p.current), ButtonKind::ghost(), choose(id, HoldChoice::PinCurrent)));
            add(row, action("Wait", ButtonKind::ghost(), choose(id, HoldChoice::Wait)));
        } else if (p.pending_swap) {
            add(row, action(std::format("Swap to {}", *p.pending_swap), ButtonKind::primary(), start(in::Swap{id})));
        } else if (p.offer && p.current) {
            if (!staged_offer) add(row, action(std::format("Download {}", *p.offer), ButtonKind::primary(), start(in::Stage{id})));
            else if (!p.report || p.report->to != *p.offer) add(row, action("Run gate checks", ButtonKind::primary(), start(in::Gate{id})));
        }
        const bool app = std::holds_alternative<vu::product_kind::App>(p.kind);
        if (app && staged_offer) add(row, action("Verify signature", ButtonKind::ghost(), start(in::Verify{id})));
        if (p.active && p.active->previous)
            add(row, action(std::format("Roll back to {}", *p.active->previous), ButtonKind::ghost(), start(in::Rollback{id})));
        if (p.pin || p.waiting_past)
            add(row, action("Unpin", ButtonKind::ghost(), [post, id] { post(Message::event(in::Unpin{id})); }));
        add(row, action("Check", ButtonKind::ghost(), start(in::Check{id})));
        return row;
    };

    auto details = [&](const Product& p) -> QWidget* {
        const auto checked = p.last_check_unix ? std::format("checked {}", loa::runtime::time::ago(static_cast<std::int64_t>(d.now_unix) * 1000,
                                                                                                     static_cast<std::int64_t>(*p.last_check_unix) * 1000))
                                               : std::string("not checked yet");
        const auto in_use = std::visit(
            []<class I>(const I& i) -> std::string {
                namespace ins = vu::installed;
                if constexpr (std::is_same_v<I, ins::Unknown>) return "not looked for yet";
                else if constexpr (std::is_same_v<I, ins::Running>) return "this app";
                else if constexpr (std::is_same_v<I, ins::Managed>) return std::format("{} (installed by the updater)", i.path);
                else if constexpr (std::is_same_v<I, ins::System>) return i.path;
                else return std::format("not installed: {}", i.path);
            },
            p.installed);
        std::string signature = "not checked (download the update first)";
        if (p.signature)
            signature = std::visit(
                []<class S>(const S& s) -> std::string {
                    namespace ss = vu::signature_status;
                    if constexpr (std::is_same_v<S, ss::Verified>) return "verified with a trusted key";
                    else if constexpr (std::is_same_v<S, ss::Unsigned>) return "unsigned: it will not be swapped in";
                    else if constexpr (std::is_same_v<S, ss::NoTrustedKeys>) return "no trusted release key configured: updates cannot be verified";
                    else return s.reason;
                },
                *p.signature);
        std::string staged;
        for (const auto& s : p.staged) staged += (staged.empty() ? "" : ", ") + s;
        if (staged.empty()) staged = "none";
        auto* facts = vcol({labeled("Feed", std::format("{} \u00b7 {}", p.source, checked))});
        if (p.newest) add(facts, labeled("Newest release", *p.newest));
        add(facts, labeled("In use", in_use));
        if (p.active)
            add(facts, labeled("Active", std::format("{} (rollback to {})", p.active->current, p.active->previous.value_or("none"))));
        add(facts, labeled("Staged", staged));
        if (p.pin) add(facts, labeled("Pinned", *p.pin));
        if (p.waiting_past) add(facts, labeled("Waiting", std::format("skipping {} and older", *p.waiting_past)));
        if (std::holds_alternative<vu::product_kind::App>(p.kind)) add(facts, labeled("Signature", signature));
        QWidget* gate = p.report ? report(*p.report) : caption("No gate report yet.");
        auto* col = vcol({facts, gate}, spi(SpaceRole::M));
        if (!str::trim(p.changelog).empty()) add(col, folded("Release notes", lines_of(p.changelog), NOTE_LINES, " lines"));
        return col;
    };

    auto product = [&](const Product& p) -> QWidget* {
        const bool open = state.selected == p.id;
        const auto [tone, line] = status(p, d.turn_running);
        const char* kind = std::holds_alternative<vu::product_kind::App>(p.kind) ? "app"
                           : std::holds_alternative<vu::product_kind::Omp>(p.kind) ? "omp"
                                                                                   : "harness";
        auto* name = kw::text_button(std::format("{} {}", open ? "\u25be" : "\u25b8", p.label), ButtonKind::flat(),
                                     [post, id = p.id] { post(Message::local(local::Toggle{id})); });
        name->setAccessibleName(qs(p.label));
        const auto versions = !p.current ? std::string("not installed") : p.offer ? std::format("{} \u2192 {}", *p.current, *p.offer) : *p.current;
        auto* top = hrow({name, kw::badge(kind, Tone::Muted), kw::label(versions, TextRole::Mono, Tone::Normal)}, spi(SpaceRole::M));
        stretch(top);
        add(top, actions(p));
        auto* col = vcol({top, hrow({dot(tone), kw::label(line, TextRole::Label, tone)})}, spi(SpaceRole::S));
        if (p.error) add(col, errors({*p.error}));
        if (open) add(col, details(p));
        return kw::card(col);
    };

    auto* head = hrow({kw::label("Updates", TextRole::Body, Tone::Normal)});
    stretch(head);
    add(head, action("Check all", ButtonKind::ghost(), state.any_idle() ? start(in::CheckAll{}) : Callback{}));
    auto* products = vcol({}, spi(SpaceRole::S));
    for (const auto& p : d.products) add(products, product(p));

    QWidget* peers = nullptr;
    if (d.peers.empty()) {
        peers = kw::label("No peers yet.", TextRole::Label, Tone::Muted);
    } else {
        const auto behind = std::ranges::count_if(d.peers, [](const PeerStanding& p) { return std::holds_alternative<vu::standing::Behind>(p.standing); });
        const auto summary = behind == 0 ? std::string("No peer is behind.") : behind == 1 ? std::string("1 peer is behind.") : std::format("{} peers are behind.", behind);
        peers = vcol({kw::label(summary, TextRole::Label, Tone::Muted)}, spi(SpaceRole::S));
        for (const auto& p : d.peers) {
            const auto [tone, text] = standing(p);
            const auto name = p.name ? *p.name : kit::text::truncate_trailing(p.node, 13);
            std::string shared;
            for (const auto& s : p.protocols) {
                if (!shared.empty()) shared += " \u00b7 ";
                shared += s.shared ? std::format("{} {}", s.family, *s.shared) : std::format("{} none", s.family);
            }
            add(peers, hrow({dot(tone), kw::label(name, TextRole::Label, Tone::Normal), kw::label(p.app_version.value_or("unknown"), TextRole::Mono, Tone::Normal),
                             kw::label(text, TextRole::Label, tone), caption(shared)},
                            spi(SpaceRole::M)));
        }
    }

    auto* content = vcol({head,
                          kw::label("Updates install side by side, pass the gate checks, and swap in between sessions. The previous version stays for rollback.",
                                    TextRole::Label, Tone::Muted),
                          section("Products", products),
                          d.github_requests_left ? caption(std::format("GitHub API: {} requests left this hour", *d.github_requests_left)) : nullptr,
                          section("Peers", peers)},
                         spi(SpaceRole::L));
    stretch(content);
    content_ = swap_content(*this, content_, sw::scroll(content, true));
}

} // namespace loa::ui::settings::updates
