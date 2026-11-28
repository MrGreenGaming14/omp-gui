#include <doctest/doctest.h>

#include <string>
#include <variant>
#include <vector>

#include "loa/app/view.hpp"
#include "loa/settings/app/toml.hpp"
#include "loa/settings/models/agents.hpp"
#include "loa/settings/models/selector.hpp"

namespace app_view = loa::app::view;
namespace app_settings = loa::app::view::settings;

TEST_CASE("theme: appearance round trips") {
    namespace domain = loa::settings::app;
    auto overrides = domain::toml::parse("\"color.accent\" = \"#ff0000\"\n[space]\nm = 12\n");
    REQUIRE(overrides);
    const domain::Appearance appearance{domain::ThemeRef{"/themes/neon.toml"}, *overrides};
    CHECK(app_view::from_view(app_view::to_view(appearance)) == appearance);
    CHECK(app_view::from_view(app_view::to_view(domain::Appearance{})) == domain::Appearance{});
}

TEST_CASE("panel: remote hosts round trip through the view") {
    const loa::settings::app::RemoteHost entry{
        .host = "build.example.com",
        .user = "ci",
        .port = 2222,
        .proxy_jump = "bastion",
        .allowed_paths = {"/srv", "/var/log"},
        .read_only = false,
        .commands = true,
        .auto_approve = true,
    };
    const auto [alias, back] = app_view::host_from_view(app_view::host_to_view("build", entry));
    CHECK(alias == "build");
    CHECK(back == entry);
}

TEST_CASE("session: unattended policy round trips") {
    using loa::settings::app::DecisionDefault;
    using loa::settings::app::UnattendedSettings;
    const UnattendedSettings settings{
        .approval = DecisionDefault{300, "deny", "nobody around"},
        .question = DecisionDefault{60, "green", std::nullopt},
        .plan = std::nullopt,
        .form = DecisionDefault{1800, R"({"env":"prod"})", std::nullopt},
    };
    CHECK(app_view::unattended_from_view(app_view::unattended_to_view(settings)) == settings);
    CHECK(app_view::unattended_from_view(app_view::unattended_to_view(UnattendedSettings{})) == UnattendedSettings{});
}

TEST_CASE("surface: capture round trips") {
    using loa::surfaces::CaptureMode;
    for (const CaptureMode capture : {CaptureMode::Screencast, CaptureMode::VirtualDisplay}) {
        CHECK(app_view::capture_from_view(app_view::capture_to_view(capture)) == capture);
    }
}

TEST_CASE("settings::agents: agent fields round trip") {
    namespace agents = loa::settings::models::agents;
    namespace shown = loa::view::settings::agents;
    const std::string plan =
        "---\nname: plan\n# why: planning only\ndescription: \"Software architect: plans\"\nmodel:\n  - \"@slow\"\n  - "
        "anthropic/claude-opus-5-5\nthinkingLevel: high\nspawns:\n  - explore\ntools: [read, grep]\noutput:\n  properties:\n    plan:\n      "
        "type: string\n---\nPlan first.\n";
    auto def = agents::parse(plan);
    REQUIRE(def);
    const shown::AgentFields fields = app_settings::fields_to_view(*def);
    CHECK(fields.model == std::optional(shown::ModelSpec{std::vector<std::string>{"@slow", "anthropic/claude-opus-5-5"}}));
    CHECK(fields.tools == std::optional(std::vector<std::string>{"read", "grep"}));
    // Unedited fields leave the file byte identical.
    auto same = app_settings::fields_from_view(*def, fields);
    REQUIRE(same);
    CHECK(same->to_text() == plan);

    const shown::AgentFields edited{
        .name = "plan-copy",
        .description = "Plans",
        .model = shown::ModelSpec{std::string("@default")},
        .thinking_level = std::nullopt,
        .tools = std::vector<std::string>{"read", "bash"},
        .spawns = std::nullopt,
        .body = "New body\n",
    };
    auto back = app_settings::fields_from_view(*def, edited);
    REQUIRE(back);
    CHECK(app_settings::fields_to_view(*back) == edited);
    const std::string text = back->to_text();
    INFO("unknown keys and comments survive: " << text);
    CHECK(text.find("# why: planning only") != std::string::npos);
    CHECK(text.find("output:\n  properties:\n") != std::string::npos);

    auto blank = agents::AgentDefinition::create("", "");
    REQUIRE(blank);
    auto blank_back = app_settings::fields_from_view(*blank, shown::AgentFields{});
    REQUIRE(blank_back);
    CHECK(app_settings::fields_to_view(*blank_back) == shown::AgentFields{});
}

TEST_CASE("settings::models: model settings round trip") {
    namespace m = loa::settings::models::settings;
    m::ModelSettings settings{
        .roles = {m::RoleChain{"default", {"anthropic/claude-sonnet-4-5:high", "openai/gpt-5"}}},
        .models = {m::ModelToggle{"openai/gpt-5", true}, m::ModelToggle{"google/gemini", false}},
        .providers = {m::ProviderToggle{"anthropic", "Anthropic", true}},
        .overrides = {m::ModelOverride{
            "openai/gpt-5",
            {
                m::OverrideField{"name", m::FieldValue::text("GPT")},
                m::OverrideField{"contextWindow", m::FieldValue::integer(400'000)},
                m::OverrideField{"temperature", m::FieldValue::real(0.5)},
                m::OverrideField{"reasoning", m::FieldValue::boolean(true)},
            },
        }},
        .custom_providers = {},
        .harnesses = {m::HarnessModel{"codex", "Codex", "~/.codex/config.toml", "o3", "high", {"low", "high"}}},
    };
    int i = 0;
    for (const m::AuthMode auth : {m::AuthMode::ApiKey, m::AuthMode::None, m::AuthMode::OAuth}) {
        settings.custom_providers.push_back(
            m::CustomProvider{"local-" + std::to_string(i++), "http://localhost/v1", "openai-completions", auth, "LOCAL_KEY", {"qwen3"}});
    }
    CHECK(app_settings::settings_from_view(app_settings::settings_to_view(settings)) == settings);
}

TEST_CASE("settings::models: model refs and provider keys round trip") {
    const loa::settings::models::selector::ModelRef model{"openrouter", "vendor/model"};
    CHECK(app_settings::model_ref_from_view(app_settings::model_ref_to_view(model)) == model);
    const loa::settings::models::login::ProviderKey key{"omp", "anthropic"};
    CHECK(app_settings::provider_key_from_view(app_settings::provider_key_to_view(key)) == key);
}

TEST_CASE("settings::projects: project settings round trip") {
    using loa::settings::PinnedFile;
    using loa::settings::ProjectSettings;
    const ProjectSettings settings{
        .instructions = "Use rustls.\nNo unsafe.",
        .pinned = {PinnedFile{"docs/ARCH.md", "the contract"}, PinnedFile{"src/main.rs", std::nullopt}},
        .digest_model = "anthropic/claude-haiku",
        .folder = std::filesystem::path("/work/my-proj"),
        .ideas_in_context = true,
    };
    CHECK(app_settings::settings_from_view(app_settings::settings_to_view(settings)) == settings);
    CHECK(app_settings::settings_from_view(app_settings::settings_to_view(ProjectSettings{})) == ProjectSettings{});
}

TEST_CASE("settings::updates: hold choices map one to one") {
    namespace shown = loa::view::settings::updates;
    using loa::settings::HoldChoice;
    CHECK(app_settings::hold_choice_from_view(shown::HoldChoice::UpdateAnyway) == HoldChoice::UpdateAnyway);
    CHECK(app_settings::hold_choice_from_view(shown::HoldChoice::PinCurrent) == HoldChoice::PinCurrent);
    CHECK(app_settings::hold_choice_from_view(shown::HoldChoice::Wait) == HoldChoice::Wait);
}

TEST_CASE("settings::updates: products keep what the screen shows") {
    namespace d = loa::settings;
    namespace shown = loa::view::settings::updates;
    const auto v = [](std::string_view s) { return d::Version::parse(s).value(); };
    const d::GateReport report{"omp", v("19.0.0"), v("19.1.0"), {d::CheckResult::harmful(d::GateCheck::ToolSchemas, {"- edit: tool removed"})}};
    const d::ProductStatus product{
        .id = "omp",
        .label = "omp",
        .kind = d::ProductKind{d::ProductKind::Kind::Omp, {}},
        .source = "GitHub can1357/oh-my-pi",
        .current = v("19.0.0"),
        .installed = d::InstalledStatus{d::InstalledStatus::Kind::Managed, "/opt/omp", {}},
        .offer = std::nullopt,
        .newest = v("19.1.0"),
        .changelog = "- faster",
        .last_check_unix = 5,
        .staged = {v("19.1.0")},
        .active = std::nullopt,
        .report = report,
        .held = std::nullopt,
        .pending_swap = std::nullopt,
        .pin = v("19.0.0"),
        .waiting_past = std::nullopt,
        .signature = d::SignatureStatus{d::SignatureStatus::Kind::Rejected, "bad key"},
        .restart_required = true,
        .error = "feed down",
    };
    const shown::Product product_view = app_settings::product_to_view(product);
    CHECK(product_view.current == std::optional<std::string>("19.0.0"));
    CHECK(product_view.staged == std::vector<std::string>{"19.1.0"});
    CHECK(product_view.pin == std::optional<std::string>("19.0.0"));
    CHECK(product_view.installed == shown::Installed(shown::installed::Managed{"/opt/omp"}));
    REQUIRE(product_view.report);
    CHECK(product_view.report->to == "19.1.0");
    CHECK(product_view.report->harmful().size() == 1);
}

TEST_CASE("settings::workflows: workflow round trips each rules kind") {
    namespace w = loa::settings::models::workflows;
    using loa::settings::models::selector::Selector;
    const std::vector<w::Rules> kinds{
        w::Rules::none(),
        w::Rules::hand_written("## Mode\nText\n"),
        w::Rules::external("Outside formatting.\n"),
        w::Rules::managed("Review", {"reviewer"}, {"writer"}, "Notes",
                          w::ManagedFields{
                              .description = "Review changes.",
                              .routing = {w::RoutingNote{"reviewer", "Check assumptions."}},
                              .steps = {"Read the changes."},
                              .blocks = {w::RuleBlock{"Reporting", "List findings."}},
                          }),
    };
    for (const auto& rules : kinds) {
        const w::Workflow workflow{"REVIEW", {Selector::parse("provider/model:high").value(), Selector::parse("@default").value()}, rules};
        auto back = app_settings::from_view(app_settings::to_view(workflow));
        REQUIRE(back);
        CHECK(*back == workflow);
    }
}
