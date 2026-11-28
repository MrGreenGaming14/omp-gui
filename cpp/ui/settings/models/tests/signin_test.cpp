// Port of the inline tests of `crates/loa-ui-settings/src/models/signin.rs`.

#include <doctest/doctest.h>

#include <functional>
#include <string>
#include <variant>
#include <vector>

#include "loa/ui/settings/models/view.hpp"
#include "support.hpp"

using namespace loa::ui::settings::models;
using namespace loa::ui::settings::models::signin;
namespace vm = loa::view::settings::models;
namespace vi = vm::signin::intent;
namespace ls = vm::login_state;
namespace lu = vm::login_update;
namespace l = loa::ui::settings::models::signin::local;
using loa::ui::kit::Tone;

namespace {

ProviderKey key(std::string harness) { return {std::move(harness), "anthropic"}; }
ProviderEntry row(std::string harness, vm::Auth auth) { return {key(harness), "anthropic", harness, std::move(auth)}; }
vm::Auth signed_out() { return vm::auth::Login{false, std::nullopt}; }
vm::Auth signed_in() { return vm::auth::Login{true, std::nullopt}; }
LoginUpdate url(std::optional<std::string> launch) { return lu::Url{"https://auth.example", std::move(launch), std::nullopt}; }
Message loc(Local m) { return Message::local(std::move(m)); }
ProviderKey omp() { return key("omp"); }
using Check = std::function<bool(const State&)>;

template <typename T> bool is(const State& s) { return std::holds_alternative<T>(s.login_state(omp()).value); }

Credential credential(std::int64_t id, bool active) { return {id, "account " + std::to_string(id), "OAuth", active}; }

bool done(const State& s, std::string_view text, Tone want) {
    const auto* o = s.sign_out(omp());
    const auto* d = o ? std::get_if<sign_out::Done>(o) : nullptr;
    return d && d->message == text && d->tone == want;
}

} // namespace

TEST_CASE("logins reduce messages and updates") {
    using Step = std::variant<Message, LoginUpdate>;
    struct Case {
        std::vector<Step> steps;
        std::vector<Intent> want;
        Check check;
    };
    const auto start = [] { return Step{loc(l::SignIn{omp()})}; };
    const auto upd = [](LoginUpdate u) { return Step{std::move(u)}; };
    const std::vector<Case> cases{
        {{start(), start()}, {vi::SignIn{omp()}}, [](const State& s) { return s.login_state(omp()) == LoginState{ls::Starting{}}; }},
        {{start(), upd(lu::Progress{"local server"}), upd(url(std::nullopt))}, {vi::SignIn{omp()}}, [](const State& s) {
             const auto* b = std::get_if<ls::Browser>(&s.login_state(omp()).value);
             return b && b->progress == "local server" && b->copy_url == "https://auth.example";
         }},
        {{start(), upd(url("http://localhost/go")), Step{Message::event(vi::CopyUrl{"http://localhost/go"})}, Step{Message::event(vi::OpenUrl{"https://evil.example"})}},
         {vi::SignIn{omp()}, vi::CopyUrl{"http://localhost/go"}}, is<ls::Browser>},
        {{start(), upd(url(std::nullopt)), upd(lu::Succeeded{"me"})}, {vi::SignIn{omp()}},
         [](const State& s) { return s.providers()[0].auth == vm::Auth{vm::auth::Login{true, "me"}} && is<ls::Succeeded>(s); }},
        {{start(), upd(url(std::nullopt)), Step{loc(l::Cancel{omp()})}, upd(lu::Succeeded{std::nullopt}), Step{loc(l::Cancel{omp()})}},
         {vi::SignIn{omp()}, vi::Cancel{omp()}},
         [](const State& s) { return is<ls::Cancelled>(s) && s.providers()[0].auth == signed_out(); }},
        {{upd(url(std::nullopt)), Step{loc(l::SignIn{key("codex")})}}, {}, is<ls::Idle>},
        {{start(), upd(lu::Failed{"denied"}), start()}, {vi::SignIn{omp()}, vi::SignIn{omp()}}, is<ls::Starting>},
        {{start(), Step{loc(l::Dismiss{omp()})}, upd(lu::Failed{"denied"}), Step{loc(l::Dismiss{omp()})}}, {vi::SignIn{omp()}}, is<ls::Idle>},
        {{Step{loc(l::SignIn{key("claude-code")})}}, {vi::SignIn{key("claude-code")}}, is<ls::Idle>},
        {{start(), upd(url(std::nullopt)), upd(lu::Prompt{"Paste the code", std::nullopt}), Step{loc(l::Submit{omp()})}, Step{loc(l::Input{omp(), "abc"})},
          Step{loc(l::Submit{omp()})}, Step{loc(l::Submit{omp()})}},
         {vi::SignIn{omp()}, vi::SubmitInput{omp(), "abc"}},
         [](const State& s) {
             const auto* b = std::get_if<ls::Browser>(&s.login_state(omp()).value);
             return b && !b->prompt;
         }},
        // A prompt before the URL is dropped: the user cannot have a code yet.
        {{start(), upd(lu::Prompt{"Paste the code", std::nullopt}), upd(url(std::nullopt))}, {vi::SignIn{omp()}},
         [](const State& s) {
             const auto* b = std::get_if<ls::Browser>(&s.login_state(omp()).value);
             return b && !b->prompt;
         }},
        {{start(), upd(lu::Failed{"denied"})}, {vi::SignIn{omp()}}, [](const State& s) { return s.login_state(omp()) == LoginState{ls::Failed{"denied"}}; }},
    };
    for (std::size_t i = 0; i < cases.size(); ++i) {
        State s({row("omp", signed_out()), row("claude-code", signed_out()), row("codex", vm::auth::ApiKey{std::nullopt, "Set OPENAI_API_KEY"})});
        std::vector<Intent> events;
        for (const auto& step : cases[i].steps) {
            if (const auto* m = std::get_if<Message>(&step)) {
                if (auto e = s.update(*m)) events.push_back(*e);
            } else {
                s.login_update(omp(), std::get<LoginUpdate>(step));
            }
        }
        CHECK_MESSAGE(events == cases[i].want, "case " << i);
        CHECK_MESSAGE(cases[i].check(s), "case " << i);
    }
}

TEST_CASE("refreshed rows keep running logins") {
    State s({row("omp", signed_out()), row("claude-code", signed_out())});
    for (const auto& k : {omp(), key("claude-code")}) (void)s.update(loc(l::SignIn{k}));
    s.set_providers({row("omp", signed_out())});
    CHECK(s.login_state(omp()).is_running());
    CHECK(s.login_state(key("claude-code")) == LoginState{ls::Idle{}});
}

TEST_CASE("the view signs in and opens the login url") {
    State s({row("omp", signed_out())});
    (void)test::app();
    SignInView view;
    auto events = [&](std::string_view label) {
        std::vector<Intent> out;
        for (auto m : test::click<SignInView, State, Message>(view, s, label))
            if (auto e = s.update(m)) out.push_back(*e);
        return out;
    };
    CHECK(events("Sign in") == std::vector<Intent>{vi::SignIn{omp()}});
    s.login_update(omp(), url(std::nullopt));
    CHECK(events("Open browser") == std::vector<Intent>{vi::OpenUrl{"https://auth.example"}});
}

TEST_CASE("sign outs reduce messages and results") {
    struct M { Local m; };
    struct Update { LoginUpdate u; };
    struct Listed { std::expected<std::vector<Credential>, std::string> r; };
    struct Removed { std::expected<std::optional<std::string>, std::string> r; };
    struct Refresh {};
    using Step = std::variant<M, Update, Listed, Removed, Refresh>;
    struct Case {
        std::vector<Step> steps;
        std::vector<Intent> want;
        Check check;
    };
    const auto out = [] { return Step{M{l::SignOut{omp()}}}; };
    const auto listed = [] { return Step{Listed{std::vector<Credential>{credential(7, true), credential(9, false)}}}; };
    const auto remove = [](std::int64_t id) { return Step{M{l::Remove{omp(), id}}}; };
    const auto removed = [](std::int64_t id) { return Intent{vi::RemoveCredential{omp(), id}}; };
    const auto err = [](std::string e) { return std::unexpected(std::move(e)); };
    const auto none = [](const State& s) { return !s.has_sign_outs(); };
    const std::vector<Case> cases{
        {{Step{M{l::SignOut{key("claude-code")}}}, Step{M{l::SignOut{key("codex")}}}}, {}, none},
        {{out(), out(), listed(), remove(5), remove(9), remove(9), Step{Removed{std::optional<std::string>("ANTHROPIC_API_KEY")}}},
         {vi::SignOut{omp()}, removed(9)},
         [](const State& s) { return done(s, "Sign in removed; still signed in through ANTHROPIC_API_KEY", Tone::Warn); }},
        {{out(), listed(), Step{M{l::CancelSignOut{omp()}}}, remove(7), listed()}, {vi::SignOut{omp()}}, none},
        {{out(), Step{M{l::CancelSignOut{omp()}}}, listed()}, {vi::SignOut{omp()}}, none},
        {{out(), listed(), remove(7), Step{M{l::CancelSignOut{omp()}}}, Step{M{l::SignIn{omp()}}}, Step{Removed{std::optional<std::string>{}}}},
         {vi::SignOut{omp()}, removed(7)},
         [](const State& s) { return done(s, "Sign in removed", Tone::Ok); }},
        {{out(), Step{Listed{std::vector<Credential>{}}}}, {vi::SignOut{omp()}}, [](const State& s) { return done(s, "No stored sign in to remove", Tone::Muted); }},
        {{out(), Step{Listed{err("omp RPC connection failed")}}}, {vi::SignOut{omp()}},
         [](const State& s) { return done(s, "Can't read the stored sign ins: omp RPC connection failed", Tone::Danger); }},
        {{out(), listed(), remove(7), Step{Removed{err("Credential 7 is not stored for anthropic")}}, Step{M{l::Dismiss{omp()}}}, out()},
         {vi::SignOut{omp()}, removed(7), vi::SignOut{omp()}},
         [](const State& s) { const auto* o = s.sign_out(omp()); return o && std::holds_alternative<sign_out::Reading>(*o); }},
        {{out(), listed(), remove(7), Step{Removed{err("Credential 7 is not stored for anthropic")}}}, {vi::SignOut{omp()}, removed(7)},
         [](const State& s) { return done(s, "Sign out failed: Credential 7 is not stored for anthropic", Tone::Danger); }},
        // A running login and a sign out of one row exclude each other; a new one clears the other's outcome.
        {{Step{M{l::SignIn{omp()}}}, out()}, {vi::SignIn{omp()}}, none},
        {{out(), Step{M{l::SignIn{omp()}}}}, {vi::SignOut{omp()}}, is<ls::Idle>},
        {{Step{M{l::SignIn{omp()}}}, Step{Update{lu::Failed{"denied"}}}, out()}, {vi::SignIn{omp()}, vi::SignOut{omp()}}, is<ls::Idle>},
        {{out(), Step{Listed{std::vector<Credential>{}}}, Step{M{l::SignIn{omp()}}}}, {vi::SignOut{omp()}, vi::SignIn{omp()}}, none},
        // A refresh drops listed sign ins only: reads, removals and outcomes go on.
        {{out(), listed(), Step{Refresh{}}, remove(7)}, {vi::SignOut{omp()}}, none},
        {{out(), Step{Refresh{}}, listed()}, {vi::SignOut{omp()}},
         [](const State& s) {
             const auto* o = s.sign_out(omp());
             const auto* c = o ? std::get_if<sign_out::Confirm>(o) : nullptr;
             return c && c->credentials.size() == 2;
         }},
        {{out(), listed(), remove(7), Step{Refresh{}}, Step{Removed{std::optional<std::string>{}}}, Step{Refresh{}}}, {vi::SignOut{omp()}, removed(7)},
         [](const State& s) { return done(s, "Sign in removed", Tone::Ok); }},
    };
    const auto rows = [] { return std::vector<ProviderEntry>{row("omp", signed_in()), row("claude-code", signed_out()), row("codex", vm::auth::Unavailable{"not installed"})}; };
    for (std::size_t i = 0; i < cases.size(); ++i) {
        State s(rows());
        std::vector<Intent> events;
        for (const auto& step : cases[i].steps) {
            std::visit(
                [&](const auto& st) {
                    using T = std::decay_t<decltype(st)>;
                    if constexpr (std::is_same_v<T, M>) {
                        if (auto e = s.update(loc(st.m))) events.push_back(*e);
                    } else if constexpr (std::is_same_v<T, Update>) s.login_update(omp(), st.u);
                    else if constexpr (std::is_same_v<T, Listed>) s.credentials(omp(), st.r);
                    else if constexpr (std::is_same_v<T, Removed>) s.signed_out(omp(), st.r);
                    else s.set_providers(rows());
                },
                step);
        }
        CHECK_MESSAGE(events == cases[i].want, "case " << i);
        CHECK_MESSAGE(cases[i].check(s), "case " << i);
    }
}

TEST_CASE("the view signs out through the picked sign in") {
    (void)test::app();
    SignInView view;
    State s({row("omp", signed_out())});
    auto click = [&](std::string_view label) {
        std::vector<Intent> out;
        for (auto m : test::click<SignInView, State, Message>(view, s, label))
            if (auto e = s.update(m)) out.push_back(*e);
        return out;
    };
    CHECK_MESSAGE(click("Sign out").empty(), "nothing to sign out of");
    s.set_providers({row("omp", signed_in())});
    CHECK(click("Sign out") == std::vector<Intent>{vi::SignOut{omp()}});
    CHECK_MESSAGE(click("Sign in again").empty(), "the sign out is open");
    s.credentials(omp(), std::vector<Credential>{credential(7, true)});
    CHECK(click("Remove") == std::vector<Intent>{vi::RemoveCredential{omp(), 7}});
    s.signed_out(omp(), std::optional<std::string>{});
    CHECK(click("Dismiss").empty());
    CHECK_FALSE(s.has_sign_outs());
    CHECK(click("Sign in again") == std::vector<Intent>{vi::SignIn{omp()}});
    // A running login hides both buttons.
    view.render(s);
    QApplication::processEvents();
    CHECK_FALSE(loa::ui::kit::testkit::click_label(view, "Sign out"));
}
