// First: `loa/p2p/node.hpp` declares a member named `emit`, which Qt headers define away.
#include "loa/p2p/node.hpp"

#include "updater.hpp"

#include <algorithm>
#include <array>

#include <QVBoxLayout>
#include <QWidget>

#include "loa/app/adapters/update_gates.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/view/settings/updates.hpp"
#include "loa/harness/claude/driver.hpp"
#include "loa/runtime/time.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/settings/updates/view.hpp"
#include "loa/updater/peers.hpp"
#include "loa/updater/signature.hpp"

namespace loa::app::areas::updater {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

namespace vu = loa::view::settings::updates;
namespace theme = loa::ui::kit::theme;
using loa::app::view::settings::hold_choice_from_view;
using loa::app::view::settings::peer_to_view;
using loa::app::view::settings::product_to_view;

int px(float v) { return static_cast<int>(v + 0.5F); }

std::string_view trim_v(std::string_view text) {
    if (text.starts_with('v')) text.remove_prefix(1);
    return text;
}

std::string label(const Updater& updater, std::string_view id) {
    for (const auto& spec : updater.config().products)
        if (spec.product.id == id) return spec.label;
    return std::string(id);
}

// The step's product, for the `Done` that ends it.
const std::string& product_of(const screen::Intent& intent) {
    static const std::string none;
    return std::visit(overloaded{[](const vu::intent::CheckAll&) -> const std::string& { return none; },
                                 [](const auto& i) -> const std::string& { return i.product; }},
                      intent);
}

loa::app::Message done(std::optional<std::string> product, std::optional<std::string> note) {
    return State::message(msg::Done{std::move(product), std::move(note)});
}

// A blocking step of `id` on the updater, whose note (if any) ends it.
template <typename Work>
Task step(std::string id, Work work) {
    return blocking(std::move(work), [id = std::move(id)](std::optional<std::string> note) { return done(id, std::move(note)); });
}

const std::array<AreaCommand<State>, 2> COMMAND_TABLE{
    AreaCommand<State>{"updater.open", "Updates", "Commands", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return s.update_typed(msg::Open{}, ctx); }},
    AreaCommand<State>{"updater.check", "Check for updates", "Commands", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) {
                           auto open = s.update_typed(msg::Open{}, ctx);
                           open.append(s.check_all(ctx));
                           return open;
                       }},
};

} // namespace

std::span<const AreaCommand<State>> State::COMMANDS() { return COMMAND_TABLE; }

State::State(std::expected<Updater, std::string> updater, Version version, std::chrono::seconds interval)
    : updater_(std::move(updater)), turns_(std::make_shared<loa::updater::TurnGate>()), version_(std::move(version)),
      interval_(interval) {}

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto version = Version::parse(APP_VERSION);
    const auto root = ctx.services.storage.dirs.data / "updates";
    std::vector<std::string> problems;
    settings::app::UpdaterSettings settings;
    if (auto config = ctx.services.storage.config.load()) settings = config->updater;
    else problems.push_back("Update settings unreadable (" + config.error().to_string() + "); using defaults");
    loa::updater::UpdaterConfig config(root, loa::updater::default_products(*version));
    if (settings.github_token && !settings.github_token->empty()) config.github_token = settings.github_token;
    config.ports = std::make_shared<adapters::OmpPorts>();
    for (const auto& key : settings.trusted_keys) {
        if (auto parsed = loa::updater::signature::parse_public_key(key)) config.trusted_keys.push_back(std::move(*parsed));
        else problems.push_back("updater.trusted_keys: " + parsed.error().to_string());
    }
    std::expected<Updater, std::string> updater = Updater::open(std::move(config)).transform_error([](const loa::updater::Error& e) {
        return "Updates are unavailable: " + e.to_string();
    });
    auto state = std::make_unique<State>(std::move(updater), std::move(*version), settings.check_interval());
    if (state->updater_) {
        std::map<std::string, Version> pins;
        for (const auto& [id, pin] : settings.pins) {
            if (auto parsed = Version::parse(trim_v(pin))) pins.emplace(id, std::move(*parsed));
            else problems.push_back("updater.pins." + id + ": " + parsed.error().to_string());
        }
        if (auto set = state->updater_->set_pins(std::move(pins)); !set) problems.push_back("updater.pins: " + set.error().to_string());
    }
    if (!problems.empty()) {
        std::string joined;
        for (const auto& p : problems) joined += (joined.empty() ? "" : "; ") + p;
        ctx.notify(std::move(joined));
    }
    state->refresh(ctx);
    auto task = state->check_all(ctx);
    return {std::move(state), std::move(task)};
}

void State::refresh(Ctx& ctx) {
    if (!updater_) return;
    const auto overview = updater_->overview();
    managed_.clear();
    for (const auto& product : overview.products)
        if (product.installed.kind == InstalledStatus::Kind::Managed) managed_.emplace(product.id, product.installed.path);
    ctx.shell.sidebar_mut().set_updates(waiting(overview));
    std::vector<loa::p2p::PeerInfo> known;
    if (auto node = ctx.node.get()) known = node->peers();
    vu::Data data;
    if (overview.github_rate_limit) data.github_requests_left = overview.github_rate_limit->remaining;
    for (const auto& product : overview.products) data.products.push_back(product_to_view(product));
    for (const auto& peer : standings(version_, known)) data.peers.push_back(peer_to_view(peer));
    data.turn_running = running_ > 0;
    data.now_unix = loa::runtime::time::now_secs();
    screen_.set_data(std::move(data));
}

Task State::check_all(Ctx& ctx) {
    auto event = screen_.update(screen::Message::local(screen::local::Start{vu::intent::CheckAll{}}));
    return event ? run(std::move(*event), ctx) : Task::none();
}

Task State::run(screen::Intent intent, Ctx& ctx) {
    if (!updater_) {
        ctx.notify(updater_.error());
        screen_.finish_all();
        return Task::none();
    }
    const Updater updater = *updater_;
    auto turns = turns_;
    std::string id = product_of(intent);
    return std::visit(
        overloaded{
            [&](vu::intent::CheckAll&) {
                return blocking(
                    [updater]() -> std::optional<std::string> {
                        updater.detect();
                        const auto results = updater.check_all();
                        const auto failed = std::ranges::count_if(results, [](const auto& r) { return !r.second.has_value(); });
                        if (failed == 0) return std::nullopt;
                        if (failed == 1) return "1 update feed couldn't be read; see Updates";
                        return std::to_string(failed) + " update feeds couldn't be read; see Updates";
                    },
                    [](std::optional<std::string> note) { return done(std::nullopt, std::move(note)); });
            },
            [&](vu::intent::Check&) {
                return step(id, [updater, id]() -> std::optional<std::string> {
                    (void)updater.check(id);
                    return std::nullopt;
                });
            },
            [&](vu::intent::Stage&) {
                return step(id, [updater, id]() -> std::optional<std::string> {
                    (void)updater.stage(id);
                    return std::nullopt;
                });
            },
            [&](vu::intent::Verify&) {
                return step(id, [updater, id]() -> std::optional<std::string> {
                    (void)updater.verify(id);
                    return std::nullopt;
                });
            },
            [&](vu::intent::Gate&) {
                return step(id, [updater, id, turns]() -> std::optional<std::string> {
                    auto report = updater.gate(id);
                    if (!report) return std::nullopt;
                    // Cleared: swap now unless a turn runs.
                    if (report->is_clean()) return swap_note(updater, id, updater.swap(id, *turns));
                    return label(updater, id) + " " + report->to.to_string() + " held: " + std::to_string(report->harmful().size()) +
                           " gate check(s) found harmful changes";
                });
            },
            [&](vu::intent::Choose& c) {
                std::optional<std::string> note;
                if (auto chosen = updater.choose(id, hold_choice_from_view(c.choice)); chosen && *chosen)
                    note = swap_note(updater, id, updater.swap(id, *turns));
                save_pins(updater, ctx);
                return Task::done(done(id, std::move(note)));
            },
            [&](vu::intent::Unpin&) {
                (void)updater.unpin(id);
                save_pins(updater, ctx);
                return Task::done(done(id, std::nullopt));
            },
            [&](vu::intent::Swap&) { return Task::done(done(id, swap_note(updater, id, updater.swap(id, *turns)))); },
            [&](vu::intent::Rollback&) { return Task::done(done(id, swap_note(updater, id, updater.rollback(id, *turns)))); },
        },
        intent);
}

void State::save_pins(const Updater& updater, Ctx& ctx) const {
    std::map<std::string, std::string> pins;
    for (const auto& product : updater.overview().products)
        if (product.pin) pins.emplace(product.id, product.pin->to_string());
    ctx.edit_config("pins", [&](settings::app::Config& config) { config.updater.pins = std::move(pins); });
}

void State::set_running(std::size_t running, Ctx& ctx) {
    if (running == running_) return;
    running_ = running;
    turns_->set(running);
    if (running == 0 && updater_) {
        std::string notes;
        for (const auto& [id, outcome] : updater_->apply_pending(*turns_))
            if (auto note = swap_note(*updater_, id, outcome)) notes += (notes.empty() ? "" : "; ") + *note;
        if (!notes.empty()) ctx.notify(std::move(notes));
    }
    refresh(ctx);
}

Task State::update_typed(Message m, Ctx& ctx) {
    return std::visit(overloaded{
                          [&](msg::Open&) {
                              ctx.show_screen(ID);
                              refresh(ctx);
                              return Task::none();
                          },
                          [&](msg::Screen& s) {
                              auto event = screen_.update(std::move(s.message));
                              return event ? run(std::move(*event), ctx) : Task::none();
                          },
                          [&](msg::Done& d) {
                              if (d.product) screen_.finish(*d.product);
                              else screen_.finish_all();
                              refresh(ctx);
                              if (d.note) ctx.notify(std::move(*d.note));
                              return Task::none();
                          },
                          [&](msg::CheckDue&) { return check_all(ctx); },
                      },
                      m);
}

QWidget* State::view_screen(const Ctx&, const Emit& send) const {
    QWidget* body = nullptr;
    if (updater_) {
        auto* view = new screen::UpdatesView;
        view->render(screen_);
        QObject::connect(view, &screen::UpdatesView::message, view,
                         [send](const screen::Message& m) { send(message(msg::Screen{m})); });
        body = view;
    } else {
        body = loa::ui::kit::widgets::label(updater_.error(), theme::TextRole::Body, loa::ui::kit::Tone::Normal);
    }
    body->setMaximumWidth(px(theme::width(theme::LayoutRole::ContentWidth)));
    auto* root = new QWidget;
    root->setAccessibleName(QStringLiteral("updater"));
    auto* layout = new QVBoxLayout(root);
    layout->setContentsMargins(px(theme::sp(theme::SpaceRole::Xl)), px(theme::sp(theme::SpaceRole::M)), px(theme::sp(theme::SpaceRole::Xl)),
                               px(theme::sp(theme::SpaceRole::M)));
    layout->addWidget(body, 1, Qt::AlignHCenter);
    return root;
}

void State::contribute(contracts::SessionConfig& config, const Target& target, const Ctx&) const {
    // The updater names Claude Code's product `claude`.
    const std::string_view product = target.harness == loa::harness::claude::HARNESS ? std::string_view("claude") : target.harness;
    if (auto it = managed_.find(product); it != managed_.end()) config.executable = it->second;
}

void State::before_send(const SessionId& session, contracts::Prompt&, Ctx& ctx) {
    // The turn starts now; a swap must not slip in before its patch.
    std::size_t others = 0;
    for (const auto& [id, open] : ctx.runtime.sessions().iter())
        if (open.is_busy() && id != session) ++others;
    set_running(others + 1, ctx);
}

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    namespace se = app_runtime::session_event;
    if (std::holds_alternative<se::TurnStarted>(event) || std::holds_alternative<se::TurnEnded>(event) ||
        std::holds_alternative<se::Closed>(event) || std::holds_alternative<se::Loaded>(event)) {
        std::size_t running = 0;
        for (const auto& [_, open] : ctx.runtime.sessions().iter())
            if (open.is_busy()) ++running;
        set_running(running, ctx);
    }
    return Task::none();
}

Subscriptions State::subscription() const {
    return {subscription::Every{"updater.check", std::chrono::duration_cast<std::chrono::milliseconds>(interval_), message(msg::CheckDue{})}};
}

std::size_t waiting(const Overview& overview) {
    return static_cast<std::size_t>(std::ranges::count_if(overview.products, [](const auto& p) {
        return p.current && (p.offer || p.held || p.pending_swap || p.restart_required);
    }));
}

std::vector<PeerStanding> standings(const Version& version, const std::vector<loa::p2p::PeerInfo>& known) {
    std::vector<PeerInfo> peers;
    for (const auto& peer : known) {
        // Without a hello there is nothing to compare.
        if (peer.protocols.empty()) continue;
        std::optional<Version> app_version;
        if (peer.app_version)
            if (auto parsed = Version::parse(trim_v(*peer.app_version))) app_version = std::move(*parsed);
        peers.push_back(PeerInfo{peer.key.to_string(), peer.name, std::move(app_version), peer.protocols});
    }
    return loa::updater::peers::classify(version, loa::p2p::local_protocols(), peers);
}

std::optional<std::string> swap_note(const Updater& updater, std::string_view id, const loa::updater::Result<SwapOutcome>& outcome) {
    const auto name = label(updater, id);
    const bool app = std::ranges::any_of(updater.config().products, [&](const auto& spec) {
        return spec.product.id == id && spec.product.kind.kind == ProductKind::Kind::App;
    });
    if (!outcome) return std::nullopt;
    switch (outcome->type) {
    case SwapOutcome::Type::Deferred: return name + " swaps in once the running turn ends";
    case SwapOutcome::Type::AlreadyActive: return std::nullopt;
    case SwapOutcome::Type::Swapped:
        if (app) return name + " " + outcome->to.to_string() + " is installed; restart Loa to use it";
        return name + " is now " + outcome->to.to_string() + "; new sessions use it";
    }
    return std::nullopt;
}

} // namespace loa::app::areas::updater
