// `omp usage --json` and the Usage window (port of `loa-app/src/app/usage.rs`).

#include "loa/app/usage_window.hpp"

#include <algorithm>
#include <cctype>
#include <cstdio>

#include <span>

#include <QFrame>
#include <QHBoxLayout>
#include <QScrollArea>
#include <QVBoxLayout>

#include "loa/app/perform.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/time.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::app::usage {

namespace {

namespace json = loa::runtime::json;
namespace kit = ui::kit;
namespace theme = ui::kit::theme;
namespace widgets = ui::kit::widgets;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

// `value[key]` as serde_json indexes: null when absent or not an object.
const json::Value& at(const json::Value& value, std::string_view key) {
    static const json::Value null;
    const auto* found = value.find(key);
    return found ? *found : null;
}

std::optional<std::string_view> str(const json::Value& value) {
    if (const auto* s = value.as_string()) return std::string_view(*s);
    return std::nullopt;
}

std::span<const json::Value> array(const json::Value& value) {
    if (const auto* a = value.as_array()) return *a;
    return {};
}

std::string provider_name(std::string_view id) {
    if (id == "anthropic") return "Anthropic";
    if (id == "openai-codex") return "OpenAI Codex";
    if (id == "openai") return "OpenAI";
    if (id == "google" || id == "gemini") return "Google";
    std::string out(id);
    // ASCII uppercasing of the first character, as `char::to_uppercase` does for ASCII.
    if (!out.empty() && static_cast<unsigned char>(out[0]) < 0x80)
        out[0] = static_cast<char>(std::toupper(static_cast<unsigned char>(out[0])));
    return out;
}

// Rust `{:.N}` of `value`.
std::string fixed(double value, int digits) {
    char buffer[64];
    std::snprintf(buffer, sizeof buffer, "%.*f", digits, value);
    return buffer;
}

} // namespace

std::optional<float> Provider::peak() const {
    std::optional<float> peak;
    for (const auto& account : accounts)
        for (const auto& limit : account.limits)
            if (limit.fraction) peak = peak ? std::max(*peak, *limit.fraction) : *limit.fraction;
    return peak;
}

std::optional<loa::view::shell::UsageMeter> Usage::meter() const {
    std::optional<loa::view::shell::UsageMeter> best;
    for (const auto& provider : providers)
        for (const auto& account : provider.accounts)
            for (const auto& limit : account.limits)
                // `max_by` keeps the last of equal maxima.
                if (limit.fraction && (!best || *limit.fraction >= best->fraction))
                    best = loa::view::shell::UsageMeter{*limit.fraction, limit.window};
    return best;
}

std::expected<Usage, std::string> parse(std::string_view text) {
    auto root = json::parse(text);
    if (!root) return std::unexpected("omp usage printed invalid JSON: " + root.error().to_string());
    std::vector<Provider> providers;
    for (const auto& report : array(at(*root, "reports"))) {
        std::string id(str(at(report, "provider")).value_or("unknown"));
        const auto& metadata = at(report, "metadata");
        std::string label = "Account";
        for (auto key : {"orgName", "email", "accountId"})
            if (auto value = str(at(metadata, key)); value && !value->empty()) {
                label = *value;
                break;
            }
        std::optional<std::string> reset_target;
        if (auto credential = str(at(metadata, "credentialId"))) reset_target = id + "/" + std::string(*credential);
        std::vector<Limit> limits;
        for (const auto& limit : array(at(report, "limits"))) {
            const auto& window = at(limit, "window");
            auto fraction = at(at(limit, "amount"), "usedFraction").as_f64();
            limits.push_back(Limit{
                .label = std::string(str(at(limit, "label")).value_or("Limit")),
                .window = std::string(str(at(window, "id")).value_or("")),
                .fraction = fraction ? std::optional<float>(static_cast<float>(*fraction)) : std::nullopt,
                .resets_at = at(window, "resetsAt").as_i64(),
            });
        }
        const auto& credits = at(report, "resetCredits");
        Account account{std::move(label), std::move(reset_target), std::move(limits),
                        {at(credits, "availableCount").as_u64().value_or(0), at(credits, "redeemableCount").as_u64().value_or(0)}};
        auto it = std::ranges::find_if(providers, [&](const Provider& p) { return p.id == id; });
        if (it != providers.end()) {
            it->accounts.push_back(std::move(account));
        } else {
            auto name = provider_name(id);
            providers.push_back(Provider{std::move(id), std::move(name), {std::move(account)}, std::nullopt});
        }
    }
    for (auto& provider : providers) {
        const auto* windows = at(at(*root, "capacity"), provider.id).as_array();
        if (!windows) continue;
        std::string line;
        for (const auto& window : *windows) {
            std::string meter;
            if (auto m = str(at(window, "meter"))) meter = " " + std::string(*m);
            const double used = at(window, "usedAccounts").as_f64().value_or(0.0);
            const auto accounts = at(window, "accounts").as_u64().value_or(0);
            if (!line.empty()) line += " · ";
            line += std::string(str(at(window, "window")).value_or("")) + meter + " " + fixed(used, 2) + " of " +
                    std::to_string(accounts) + " used";
        }
        if (!line.empty()) provider.capacity = std::move(line);
    }
    return Usage{at(*root, "generatedAt").as_i64().value_or(0), std::move(providers)};
}

// ---- State -------------------------------------------------------------------------------------

Task State::refresh() {
    loading_ = true;
    return perform([source = source_] { return source->fetch(); },
                   [](std::expected<std::string, std::string> json) -> loa::app::Message {
                       return loa::app::msg::Usage{usage::msg::Loaded{std::move(json)}};
                   });
}

std::pair<Task, std::optional<Event>> State::update(Message message) {
    return std::visit(
        overloaded{
            [&](usage::msg::Refresh&) -> std::pair<Task, std::optional<Event>> { return {refresh(), std::nullopt}; },
            [&](usage::msg::Loaded& l) -> std::pair<Task, std::optional<Event>> {
                loading_ = false;
                std::expected<Usage, std::string> result =
                    l.json ? parse(*l.json) : std::expected<Usage, std::string>(std::unexpect, std::move(l.json.error()));
                std::optional<loa::view::shell::UsageMeter> meter;
                if (result) meter = result->meter();
                usage_ = std::move(result);
                return {Task::none(), Event{event::Meter{std::move(meter)}}};
            },
            [&](usage::msg::Toggle& t) -> std::pair<Task, std::optional<Event>> {
                if (auto it = open_.find(t.provider); it != open_.end()) open_.erase(it);
                else open_.insert(std::move(t.provider));
                return {Task::none(), std::nullopt};
            },
            [&](usage::msg::Spend& s) -> std::pair<Task, std::optional<Event>> {
                spending_ = s.target;
                return {perform([source = source_, target = std::move(s.target)] { return source->spend(target); },
                                [](std::expected<void, std::string> result) -> loa::app::Message {
                                    return loa::app::msg::Usage{usage::msg::Spent{std::move(result)}};
                                }),
                        std::nullopt};
            },
            [&](usage::msg::Spent& s) -> std::pair<Task, std::optional<Event>> {
                spending_.reset();
                if (!s.result) usage_ = std::unexpected(std::move(s.result.error()));
                return {refresh(), std::nullopt};
            },
            [&](usage::msg::Close&) -> std::pair<Task, std::optional<Event>> { return {Task::none(), Event{event::Close{}}}; },
        },
        message);
}

// ---- view --------------------------------------------------------------------------------------

namespace {

int px(float v) { return static_cast<int>(v + 0.5F); }

QVBoxLayout* column(QWidget* w, theme::SpaceRole role) {
    auto* layout = new QVBoxLayout(w);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(px(theme::sp(role)));
    return layout;
}

QHBoxLayout* row(QWidget* w, theme::SpaceRole role) {
    auto* layout = new QHBoxLayout(w);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(px(theme::sp(role)));
    return layout;
}

kit::Tone used_tone(float fraction) { return fraction >= 0.5F ? kit::Tone::Warn : kit::Tone::Muted; }

std::string percent(float fraction) { return fixed(static_cast<double>(fraction) * 100.0, 0) + "%"; }

QWidget* account_view(const State& state, const Account& account, std::int64_t now, const Task::Emit& send) {
    const auto [left, usable] = account.resets;
    std::string resets;
    if (left == 0) resets = "No saved resets";
    else if (left == 1 && usable == 0) resets = "1 saved reset";
    else if (usable == 0) resets = std::to_string(left) + " saved resets";
    else resets = std::to_string(left) + " saved resets, " + std::to_string(usable) + " usable now";
    auto* content = new QWidget;
    auto* rows = column(content, theme::SpaceRole::M);
    auto* head = new QWidget;
    auto* line = row(head, theme::SpaceRole::S);
    line->addWidget(widgets::label(account.label, theme::TextRole::Body, kit::Tone::Normal), 1);
    line->addWidget(widgets::label(resets, theme::TextRole::Caption, kit::Tone::Muted));
    if (account.reset_target && usable > 0) {
        const bool busy = state.spending() == account.reset_target;
        widgets::Callback on;
        if (!busy) on = [send, target = *account.reset_target] { send(loa::app::msg::Usage{usage::msg::Spend{target}}); };
        line->addWidget(widgets::text_button(busy ? "Using…" : "Use a reset", kit::ButtonKind::secondary(), std::move(on)));
    }
    rows->addWidget(head);
    for (const auto& limit : account.limits) {
        auto* block = new QWidget;
        auto* parts = column(block, theme::SpaceRole::Xs);
        auto* title = new QWidget;
        auto* title_row = row(title, theme::SpaceRole::S);
        title_row->addWidget(widgets::label(limit.label, theme::TextRole::Label, kit::Tone::Normal), 1);
        if (limit.fraction) title_row->addWidget(widgets::label(percent(*limit.fraction) + " used", theme::TextRole::Label, used_tone(*limit.fraction)));
        else title_row->addWidget(widgets::label("Not reported", theme::TextRole::Label, kit::Tone::Faint));
        parts->addWidget(title);
        parts->addWidget(widgets::meter(limit.fraction.value_or(0.0F)));
        if (limit.fraction && limit.resets_at)
            parts->addWidget(widgets::label("Resets in " + loa::runtime::time::until(*limit.resets_at - now), theme::TextRole::Caption, kit::Tone::Faint));
        rows->addWidget(block);
    }
    return widgets::card(content);
}

QWidget* provider_view(const State& state, const Provider& provider, std::int64_t now, const Task::Emit& send) {
    const float peak = provider.peak().value_or(0.0F);
    const auto n = provider.accounts.size();
    const std::string accounts = n == 1 ? "1 account" : std::to_string(n) + " accounts";
    auto* out = new QWidget;
    auto* parts = column(out, theme::SpaceRole::Xs);
    auto* summary = new QWidget;
    auto* line = row(summary, theme::SpaceRole::S);
    auto* bar = widgets::meter(peak);
    bar->setFixedWidth(px(theme::width(theme::LayoutRole::LabelColumn) / 2.0F));
    line->addWidget(bar);
    line->addWidget(widgets::label("Highest " + percent(peak) + " used", theme::TextRole::Label, used_tone(peak)));
    line->addStretch(1);
    parts->addWidget(summary);
    auto* body = new QWidget;
    auto* list = column(body, theme::SpaceRole::S);
    for (const auto& account : provider.accounts) list->addWidget(account_view(state, account, now, send));
    if (provider.capacity) list->addWidget(widgets::label(*provider.capacity, theme::TextRole::Caption, kit::Tone::Faint));
    parts->addWidget(widgets::collapsible(provider.name + " · " + accounts, state.is_open(provider.id),
                                          [send, id = provider.id] { send(loa::app::msg::Usage{usage::msg::Toggle{id}}); }, body));
    return out;
}

} // namespace

QWidget* view(const State& state, std::int64_t now, const Task::Emit& send) {
    auto* window = new QWidget;
    window->setAccessibleName(QStringLiteral("Usage"));
    auto* parts = column(window, theme::SpaceRole::M);
    std::string updated;
    if (state.loading()) updated = "Updating…";
    else if (state.usage() && *state.usage()) updated = "Updated " + loa::runtime::time::ago(now, (*state.usage())->generated_at);
    parts->addWidget(widgets::section_header(
        "Usage", {widgets::label(updated, theme::TextRole::Caption, kit::Tone::Faint),
                  widgets::text_button("Refresh", kit::ButtonKind::ghost(), [send] { send(loa::app::msg::Usage{usage::msg::Refresh{}}); }),
                  widgets::icon_button(loa::view::theme::Icon::X, "Close", [send] { send(loa::app::msg::Usage{usage::msg::Close{}}); })}));
    auto* body = new QWidget;
    auto* rows = column(body, theme::SpaceRole::M);
    auto note = [&](std::string_view s) { rows->addWidget(widgets::label(s, theme::TextRole::Body, kit::Tone::Muted)); };
    const auto& usage = state.usage();
    if (!usage) note("Reading usage from omp…");
    else if (!*usage) note(usage->error());
    else if ((*usage)->providers.empty()) note("No signed-in providers report usage.");
    else
        for (const auto& provider : (*usage)->providers) rows->addWidget(provider_view(state, provider, now, send));
    // A plain QFrame line is one device pixel by default, so no size literal is needed.
    auto* rule = new QFrame;
    rule->setFrameShape(QFrame::HLine);
    rule->setFrameShadow(QFrame::Plain);
    rows->addWidget(rule);
    rows->addWidget(widgets::label("Same numbers as omp’s /usage. Resets apply to an account, not a session.", theme::TextRole::Caption,
                                   kit::Tone::Faint));
    rows->addStretch(1);
    auto* scroll = new QScrollArea;
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setWidget(body);
    parts->addWidget(scroll, 1);
    return window;
}

} // namespace loa::app::usage
