// The `models` smoke screen: the models frame on its sign in page, the model settings page and
// the model picker over sample data, each wired to its reducer.

#include <memory>
#include <optional>

#include <QHBoxLayout>
#include <QWidget>

#include "loa/ui/app/smoke.hpp"
#include "loa/ui/settings/models/view.hpp"

namespace loa::ui::app {

namespace {

namespace m = settings::models;
namespace vm = loa::view::settings::models;

// The page states outlive the views that render them.
struct Screen : QWidget {
    m::signin::State signin;
    std::unique_ptr<m::settings::State> settings;
    std::optional<m::picker::State> picker;
};

vm::ModelSettings sample_settings() {
    vm::ModelSettings s;
    s.roles = {{"default", {"anthropic/claude-sonnet-4-5:high", "openai/gpt-5"}}};
    s.models = {{"openai/gpt-5", true}, {"anthropic/claude-sonnet-4-5", true}};
    s.providers = {{"anthropic", "Anthropic", true}, {"openai", "OpenAI", true}};
    s.overrides = {{"openai/gpt-5", {{"contextWindow", vm::FieldValue{std::int64_t{400'000}}}}}};
    s.custom_providers = {{"local-llm", "http://localhost:8080/v1", "openai-completions", vm::AuthMode::ApiKey, "LOCAL_LLM_KEY", {"qwen3"}}};
    s.harnesses = {{"codex", "Codex", "~/.codex/config.toml", "", "", {"low", "high"}}};
    return s;
}

} // namespace

QWidget* models_screen() {
    auto* screen = new Screen;
    screen->setAccessibleName(QStringLiteral("models screen"));
    screen->signin.set_providers({
        {{"omp", "anthropic"}, "Anthropic", "omp", vm::auth::Login{true, "me@example.com"}},
        {{"omp", "openai"}, "OpenAI", "omp", vm::auth::Login{false, std::nullopt}},
        {{"codex", "openai"}, "OpenAI", "Codex", vm::auth::ApiKey{std::nullopt, "Set OPENAI_API_KEY"}},
    });
    (void)screen->signin.update(m::signin::Message::local(m::signin::local::SignIn{{"omp", "openai"}}));
    screen->signin.login_update({"omp", "openai"}, vm::login_update::Url{"https://auth.example/login", std::nullopt, std::nullopt});
    screen->settings = std::make_unique<m::settings::State>(sample_settings(), std::vector<std::string>{"openai/gpt-5", "anthropic/claude-haiku-4-5"});
    vm::Catalog catalog{{{{"anthropic", "claude-sonnet"}, "Claude Sonnet", "Anthropic"}, {{"openai", "gpt-5"}, "GPT-5", "OpenAI"}},
                        {{"default", {"anthropic", "claude-sonnet"}}}};
    auto [picker, task] = m::picker::State::open(std::move(catalog), vm::picker::target::Session{}, vm::ModelRef{"openai", "gpt-5"});
    screen->picker.emplace(std::move(picker));

    auto* row = new QHBoxLayout(screen);

    auto* sv = new m::SignInView;
    sv->render(screen->signin);
    QObject::connect(sv, &m::SignInView::message, sv, [screen, sv](const m::signin::Message& msg) {
        (void)screen->signin.update(msg);
        sv->render(screen->signin);
    });
    row->addWidget(m::frame(m::Page::SignIn, sv, [](m::Page) {}, [] {}), 1);

    auto* setv = new m::SettingsView;
    setv->render(*screen->settings);
    QObject::connect(setv, &m::SettingsView::message, setv, [screen, setv](const m::settings::Message& msg) {
        (void)screen->settings->update(msg);
        setv->render(*screen->settings);
    });
    row->addWidget(setv, 1);

    auto* pv = new m::PickerView;
    pv->render(*screen->picker);
    pv->apply(task);
    QObject::connect(pv, &m::PickerView::message, pv, [screen, pv](const m::picker::Message& msg) {
        (void)screen->picker->update(msg);
        pv->render(*screen->picker);
        pv->apply(screen->picker->scroll());
    });
    row->addWidget(pv, 0, Qt::AlignTop);
    screen->resize(1400, 720);
    return screen;
}

std::string check_models_screen(QWidget& screen) {
    for (const auto* name : {"models", "sign in body", "model settings body", "model picker", "Open browser", "Save", "GPT-5", "Claude Sonnet"}) {
        bool found = false;
        for (const auto* w : screen.findChildren<QWidget*>())
            if (w->accessibleName() == QString::fromUtf8(name) && w->isVisible()) found = true;
        if (!found) return std::string("missing ") + name;
    }
    return {};
}

} // namespace loa::ui::app
