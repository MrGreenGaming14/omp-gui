#include "loa/settings/models/keys.hpp"

#include <array>
#include <cstdlib>

namespace loa::settings::models::keys {

bool ProcessEnvironment::contains(std::string_view name) const {
    const char* v = std::getenv(std::string(name).c_str());
    return v != nullptr && *v != '\0';
}

KeySource credential_source(const models_yml::ModelsYml& models, std::string_view provider,
                            std::span<const std::string_view> environment_names, const EnvironmentPresence& env) {
    if (auto p = models.provider(provider); p && p->api_key) {
        switch (p->api_key->kind) {
        case models_yml::ApiKeySetting::Kind::EnvVar: return KeySource::config_environment(p->api_key->name);
        case models_yml::ApiKeySetting::Kind::Command: return KeySource::secret_command();
        case models_yml::ApiKeySetting::Kind::Literal: return KeySource::config_entry();
        }
    }
    for (std::string_view name : environment_names) {
        if (env.contains(name)) return KeySource::environment(std::string(name));
    }
    return KeySource::not_configured();
}

std::string key_instructions(std::string_view provider, const KeySource& source,
                             std::span<const std::string_view> environment_names) {
    const std::string p(provider);
    switch (source.kind) {
    case KeySource::Kind::SecretCommand:
        return "Configured through an omp models.yml secret command. Manage the credential with the command's own "
               "secret manager; omp runs it when making a request.";
    case KeySource::Kind::ConfigEnvironment:
        return "Configured through models.yml: set " + source.name +
               " in your shell or an omp .env file before starting omp.";
    case KeySource::Kind::ConfigEntry:
        return "Configured in omp models.yml. For safer storage, replace the inline value with an environment "
               "variable name or a !secret command.";
    case KeySource::Kind::Environment:
        return "Configured through " + source.name +
               ". Manage it in your shell or an omp .env file; restart omp to pick up changes.";
    case KeySource::Kind::NotConfigured: break;
    }
    if (!environment_names.empty()) {
        std::string names;
        for (std::string_view n : environment_names) {
            if (!names.empty()) names += " or ";
            names += n;
        }
        return "Set " + names + " in your shell or an omp .env file, or configure providers." + p +
               ".apiKey in omp models.yml as an environment variable name or !secret command. Never paste a key "
               "into this app.";
    }
    return "Use omp's own login or configure providers." + p +
           ".apiKey in omp models.yml as an environment variable name or !secret command. Never paste a key into "
           "this app.";
}

std::span<const std::string_view> builtin_environment_names(std::string_view provider) noexcept {
    struct Entry {
        std::string_view provider;
        std::array<std::string_view, 2> names;
        std::size_t count;
    };
    static constexpr std::array<Entry, 27> TABLE = {{
        {"anthropic", {"ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"}, 2},
        {"openai", {"OPENAI_API_KEY"}, 1},
        {"openai-codex", {"OPENAI_CODEX_OAUTH_TOKEN"}, 1},
        {"google", {"GEMINI_API_KEY"}, 1},
        {"groq", {"GROQ_API_KEY"}, 1},
        {"openrouter", {"OPENROUTER_API_KEY"}, 1},
        {"mistral", {"MISTRAL_API_KEY"}, 1},
        {"xai", {"XAI_API_KEY"}, 1},
        {"deepseek", {"DEEPSEEK_API_KEY"}, 1},
        {"together", {"TOGETHER_API_KEY"}, 1},
        {"fireworks", {"FIREWORKS_API_KEY"}, 1},
        {"cerebras", {"CEREBRAS_API_KEY"}, 1},
        {"ollama", {"OLLAMA_API_KEY"}, 1},
        {"lm-studio", {"LM_STUDIO_API_KEY"}, 1},
        {"llama.cpp", {"LLAMA_CPP_API_KEY"}, 1},
        {"litellm", {"LITELLM_API_KEY"}, 1},
        {"bifrost", {"BIFROST_API_KEY"}, 1},
        {"typesafe", {"TYPESAFE_API_KEY"}, 1},
        {"azure", {"AZURE_OPENAI_API_KEY"}, 1},
        {"github-copilot", {"COPILOT_GITHUB_TOKEN"}, 1},
        {"cursor", {"CURSOR_ACCESS_TOKEN"}, 1},
        {"zai", {"ZAI_API_KEY"}, 1},
        {"moonshot", {"MOONSHOT_API_KEY", "KIMI_API_KEY"}, 2},
        {"minimax", {"MINIMAX_API_KEY"}, 1},
        {"huggingface", {"HUGGINGFACE_HUB_TOKEN", "HF_TOKEN"}, 2},
        {"amazon-bedrock", {"AWS_BEARER_TOKEN_BEDROCK"}, 1},
        {"google-vertex", {"GOOGLE_CLOUD_API_KEY"}, 1},
    }};
    for (const Entry& e : TABLE) {
        if (e.provider == provider) return {e.names.data(), e.count};
    }
    return {};
}

} // namespace loa::settings::models::keys
