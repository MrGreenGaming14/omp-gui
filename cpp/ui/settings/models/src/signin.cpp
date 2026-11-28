// Port of the state half of `crates/loa-ui-settings/src/models/signin.rs`.

#include "loa/ui/settings/models/signin.hpp"

#include <algorithm>
#include <format>

namespace loa::ui::settings::models::signin {

namespace vm = loa::view::settings::models;
namespace ls = vm::login_state;
namespace lu = vm::login_update;
namespace vi = vm::signin::intent;
using kit::Tone;

namespace {

const LoginState IDLE{ls::Idle{}};

bool blank(std::string_view s) { return s.find_first_not_of(" \t\n\r\f\v") == std::string_view::npos; }

template <typename V> auto find(V& v, const ProviderKey& key) {
    return std::ranges::find_if(v, [&](const auto& p) { return p.first == key; });
}

} // namespace

const LoginState& State::login_state(const ProviderKey& key) const {
    const auto it = find(logins_, key);
    return it == logins_.end() ? IDLE : it->second;
}

const SignOut* State::sign_out(const ProviderKey& key) const {
    const auto it = find(sign_outs_, key);
    return it == sign_outs_.end() ? nullptr : &it->second;
}

LoginState* State::login_mut(const ProviderKey& key) {
    const auto it = find(logins_, key);
    return it == logins_.end() ? nullptr : &it->second;
}

SignOut* State::sign_out_mut(const ProviderKey& key) {
    const auto it = find(sign_outs_, key);
    return it == sign_outs_.end() ? nullptr : &it->second;
}

void State::erase_login(const ProviderKey& key) { std::erase_if(logins_, [&](const auto& p) { return p.first == key; }); }
void State::erase_sign_out(const ProviderKey& key) { std::erase_if(sign_outs_, [&](const auto& p) { return p.first == key; }); }

void State::put_login(const ProviderKey& key, LoginState s) {
    if (auto* l = login_mut(key)) *l = std::move(s);
    else logins_.emplace_back(key, std::move(s));
}

void State::put_sign_out(const ProviderKey& key, SignOut s) {
    if (auto* o = sign_out_mut(key)) *o = std::move(s);
    else sign_outs_.emplace_back(key, std::move(s));
}

bool State::sign_out_open(const ProviderKey& key) const {
    const auto* s = sign_out(key);
    return s && !std::holds_alternative<sign_out::Done>(*s);
}

void State::set_providers(std::vector<ProviderEntry> providers) {
    const auto present = [&](const ProviderKey& key) { return std::ranges::any_of(providers, [&](const ProviderEntry& e) { return e.key == key; }); };
    std::erase_if(logins_, [&](const auto& p) { return !present(p.first); });
    std::erase_if(sign_outs_, [&](const auto& p) { return !present(p.first) || std::holds_alternative<sign_out::Confirm>(p.second); });
    providers_ = std::move(providers);
}

void State::credentials(const ProviderKey& key, std::expected<std::vector<Credential>, std::string> result) {
    auto* s = sign_out_mut(key);
    if (!s || !std::holds_alternative<sign_out::Reading>(*s)) return;
    if (!result) *s = sign_out::Done{std::format("Can't read the stored sign ins: {}", result.error()), Tone::Danger};
    else if (result->empty()) *s = sign_out::Done{"No stored sign in to remove", Tone::Muted};
    else *s = sign_out::Confirm{std::move(*result)};
}

void State::signed_out(const ProviderKey& key, std::expected<std::optional<std::string>, std::string> result) {
    auto* s = sign_out_mut(key);
    if (!s || !std::holds_alternative<sign_out::Removing>(*s)) return;
    if (!result) *s = sign_out::Done{std::format("Sign out failed: {}", result.error()), Tone::Danger};
    else if (!*result) *s = sign_out::Done{"Sign in removed", Tone::Ok};
    else *s = sign_out::Done{std::format("Sign in removed; still signed in through {}", **result), Tone::Warn};
}

void State::login_update(const ProviderKey& key, LoginUpdate update) {
    auto* state = login_mut(key);
    if (!state || !state->is_running()) return;
    // A running login starts or waits in the browser.
    auto* starting = std::get_if<ls::Starting>(&state->value);
    auto* browser = std::get_if<ls::Browser>(&state->value);
    auto& progress = starting ? starting->progress : browser->progress;
    std::visit(
        [&](auto&& u) {
            using T = std::decay_t<decltype(u)>;
            if constexpr (std::is_same_v<T, lu::Url>) {
                auto kept_progress = std::exchange(progress, std::nullopt);
                std::optional<vm::LoginPrompt> kept_prompt = browser ? std::exchange(browser->prompt, std::nullopt) : std::nullopt;
                auto copy_url = u.launch_url ? std::move(*u.launch_url) : u.url;
                state->value = ls::Browser{std::move(u.url), std::move(copy_url), std::move(u.instructions), std::move(kept_progress),
                                           std::move(kept_prompt)};
            } else if constexpr (std::is_same_v<T, lu::Progress>) {
                progress = std::move(u.message);
            } else if constexpr (std::is_same_v<T, lu::Prompt>) {
                // The user cannot have a code before visiting the URL.
                if (browser) browser->prompt = vm::LoginPrompt{std::move(u.message), std::move(u.placeholder), {}};
            } else if constexpr (std::is_same_v<T, lu::Succeeded>) {
                state->value = ls::Succeeded{u.account};
                for (auto& e : providers_) {
                    if (e.key == key && std::holds_alternative<vm::auth::Login>(e.auth)) {
                        e.auth = vm::auth::Login{true, std::move(u.account)};
                        break;
                    }
                }
            } else if constexpr (std::is_same_v<T, lu::Failed>) {
                state->value = ls::Failed{std::move(u.message)};
            } else {
                state->value = ls::Cancelled{};
            }
        },
        std::move(update));
}

std::optional<Intent> State::reduce(Local m) {
    return std::visit(
        [&](auto&& l) -> std::optional<Intent> {
            using T = std::decay_t<decltype(l)>;
            const auto& key = l.key;
            if constexpr (std::is_same_v<T, local::SignIn>) {
                const bool login = std::ranges::any_of(providers_, [&](const ProviderEntry& e) { return e.key == key && std::holds_alternative<vm::auth::Login>(e.auth); });
                if (!login || login_state(key).is_running() || sign_out_open(key)) return std::nullopt;
                erase_sign_out(key);
                put_login(key, LoginState{ls::Starting{}});
                return vi::SignIn{key};
            } else if constexpr (std::is_same_v<T, local::Cancel>) {
                if (!login_state(key).is_running()) return std::nullopt;
                put_login(key, LoginState{ls::Cancelled{}});
                return vi::Cancel{key};
            } else if constexpr (std::is_same_v<T, local::Input>) {
                if (auto* s = login_mut(key))
                    if (auto* b = std::get_if<ls::Browser>(&s->value); b && b->prompt) b->prompt->input = std::move(l.value);
                return std::nullopt;
            } else if constexpr (std::is_same_v<T, local::Submit>) {
                auto* s = login_mut(key);
                auto* b = s ? std::get_if<ls::Browser>(&s->value) : nullptr;
                if (!b || !b->prompt || blank(b->prompt->input)) return std::nullopt;
                auto text = std::move(b->prompt->input);
                b->prompt.reset();
                return vi::SubmitInput{key, std::move(text)};
            } else if constexpr (std::is_same_v<T, local::Dismiss>) {
                if (!login_state(key).is_running()) erase_login(key);
                if (!sign_out_open(key)) erase_sign_out(key);
                return std::nullopt;
            } else if constexpr (std::is_same_v<T, local::SignOut>) {
                const bool signed_in = std::ranges::any_of(providers_, [&](const ProviderEntry& e) {
                    const auto* a = std::get_if<vm::auth::Login>(&e.auth);
                    return e.key == key && a && a->signed_in;
                });
                if (!signed_in || login_state(key).is_running() || sign_out_open(key)) return std::nullopt;
                erase_login(key);
                put_sign_out(key, sign_out::Reading{});
                return vi::SignOut{key};
            } else if constexpr (std::is_same_v<T, local::Remove>) {
                const auto* s = sign_out(key);
                const auto* c = s ? std::get_if<sign_out::Confirm>(s) : nullptr;
                if (!c || std::ranges::none_of(c->credentials, [&](const Credential& x) { return x.id == l.credential; })) return std::nullopt;
                put_sign_out(key, sign_out::Removing{});
                return vi::RemoveCredential{key, l.credential};
            } else {
                const auto* s = sign_out(key);
                if (s && (std::holds_alternative<sign_out::Reading>(*s) || std::holds_alternative<sign_out::Confirm>(*s))) erase_sign_out(key);
                return std::nullopt;
            }
        },
        std::move(m));
}

bool State::guard(const Intent& e) const {
    if (const auto* open = std::get_if<vi::OpenUrl>(&e))
        return std::ranges::any_of(logins_, [&](const auto& p) {
            const auto* b = std::get_if<ls::Browser>(&p.second.value);
            return b && b->url == open->url;
        });
    if (const auto* copy = std::get_if<vi::CopyUrl>(&e))
        return std::ranges::any_of(logins_, [&](const auto& p) {
            const auto* b = std::get_if<ls::Browser>(&p.second.value);
            return b && b->copy_url == copy->url;
        });
    return false;
}

} // namespace loa::ui::settings::models::signin
