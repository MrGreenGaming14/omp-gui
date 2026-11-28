#pragma once

// The contract every feature area implements, and the context the app hands each one (port of
// `loa-app/src/area.rs`).
//
// An area lives in `src/areas/<area>/` and owns its state, its message type and its behavior.
// The app holds the areas in a `Registry` and calls each through `Area`:
// - its own messages (`msg::Area` with its `id()`) reach `Area::update`
// - its commands are listed in the palette (`Area::commands`), run by id (`Area::run`) and,
//   with a `/` name, typed in the composer (`Area::slash`); ids start with `<id>.`
// - panel tabs it declares (`Area::panel_tabs`) are drawn by `Area::view_tab` and opened
//   with `Ctx::open_tab`
// - a full-center screen is drawn by `Area::view_screen` while `Ctx::screen` names the area
// - every new or resumed session gets `Area::contribute` and `Area::host_tools`
// - every session change arrives at `Area::on_session_event`
//
// `TypedArea` implements the erased calls over a typed message, as Rust's `Typed<A>` does.

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <stop_token>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/area_msg.hpp"
#include "loa/app/message.hpp"
#include "loa/app/services.hpp"
#include "loa/app/task.hpp"
#include "loa/app_runtime/runtime.hpp"
#include "loa/contracts/account.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/contracts/host_tools.hpp"
#include "loa/models/service.hpp"
#include "loa/p2p/protocols/extra.hpp"
#include "loa/projects/digester.hpp"
#include "loa/ui/session/composer/state.hpp"
#include "loa/ui/shell/command_id.hpp"
#include "loa/ui/shell/keymap.hpp"
#include "loa/ui/shell/layout.hpp"
#include "loa/ui/shell/palette.hpp"
#include "loa/view/panel.hpp"
#include "loa/view/session.hpp"
#include "loa/view/theme.hpp"

class QWidget;

// `loa/p2p/node.hpp` declares a member named `emit`, a Qt macro: areas that run a node include it.
namespace loa::p2p {
class Node;
}

namespace loa::app {

using app_runtime::SessionEvent;
using app_runtime::SessionId;

// A right panel tab an area draws.
struct PanelTab {
    // Unique across the app; the built-in tabs are `diff`, `files` and `terminal`.
    std::string id;
    std::string title;
    std::optional<loa::view::theme::Icon> icon;
    // Listed in the panel from the start.
    bool shown = false;

    friend bool operator==(const PanelTab&, const PanelTab&) = default;
};

// The harness a session config is being built for.
struct Target {
    // Harness id, e.g. `omp`.
    std::string_view harness;
    const contracts::Capabilities* capabilities = nullptr;
    // `true` when the session is resumed rather than started.
    bool resume = false;
};

// Tools an area serves to sessions through the harness's host tool mechanism: the specs and
// the handler they route to.
struct HostToolSet {
    std::vector<contracts::HostToolSpec> specs;
    std::shared_ptr<const contracts::HostTools> handler;
};

// App-level actions an area asks for.
namespace request {
// Start a session on `Sessions::harness`, with every area's contribution.
struct NewSession {};
// Show this session (loading it from its journal when needed).
struct OpenSession {
    std::string id;
};
// Show a read-only DOM transcript in its own window without creating a library/runtime session.
struct DetachTranscript {
    std::string key;
    std::string title;
    core::Dom dom;
};
// Send a prompt to the active session (resuming it when needed).
struct Prompt {
    contracts::Prompt prompt;
};
// Release a queued prompt held for this session's role switch.
struct ReleaseRoleQueue {
    std::string id;
};
// Add the panel tab with this id when it isn't listed, and show it.
struct OpenTab {
    std::string id;
};
// Add the panel tab with this id when it isn't listed, without taking the panel from the tab
// it shows; a closed panel opens on it.
struct RevealTab {
    std::string id;
};
// Start a session the area creates in the library itself (fork, rewind, compaction), with
// every area's contribution; it becomes the active session.
struct Spawn {
    app_runtime::Spawn spawn;
};
// Make `project` the active one and, when it has one, its folder `cwd` the active folder.
struct ActivateProject {
    std::string project;
    std::optional<std::filesystem::path> cwd;
};
// `project` was deleted: drop its tab and pin, and stop it being the active project.
struct ForgetProject {
    std::string project;
};
// Run this message through the app's `update`, e.g. to hand a decision answer to the area that owns it.
struct Forward {
    Message message;
};
} // namespace request

using Request = std::variant<request::NewSession, request::OpenSession, request::DetachTranscript, request::Prompt,
                             request::ReleaseRoleQueue, request::OpenTab, request::RevealTab, request::Spawn,
                             request::ActivateProject, request::ForgetProject, request::Forward>;

// The backends areas build on, from `AppParts`.
struct Backends {
    // The model screens' backend.
    models::Backend models;
    // Writes the projects' digests.
    std::shared_ptr<const projects::Summarizers> summarizer;
    // Home directory: native sessions, omp's rules and config.
    std::filesystem::path home;
    // Opens links clicked in the transcript.
    std::shared_ptr<const contracts::UrlOpener> opener;
};

// What every area may read and change: the services, the runtime (sessions, harnesses, active
// project) and the shared UI (shell frame, composer, keymap), plus app requests.
class Ctx {
  public:
    Ctx(Services services, app_runtime::Runtime runtime, Backends backends, ui::shell::layout::State shell,
        ui::session::composer::State composer, ui::shell::Keymap keymap);

    Services services;
    app_runtime::Runtime runtime;
    Backends backends;
    ui::shell::layout::State shell;
    ui::session::composer::State composer;
    ui::shell::Keymap keymap;
    // App tools areas serve to sessions, as the Tools panel lists them.
    std::vector<loa::view::panel::ToolInfo> app_tools;
    // The running peer node, set by the peers area each time it opens one; copies of the slot
    // see the current node.
    p2p::NodeSlot<p2p::Node> node;
    // The app's `/` commands, from every area's commands.
    std::vector<loa::view::session::composer::Command> slash;

    // Shows a one line notice under the center content.
    void notify(std::string text) { notice_ = std::move(text); }
    void clear_notice() { notice_.reset(); }
    [[nodiscard]] const std::optional<std::string>& notice() const noexcept { return notice_; }

    // The value of `result`; on failure a `{what}: {error}` notice and `nullopt`.
    template <typename T, typename E>
    std::optional<T> report(std::string_view what, std::expected<T, E> result) {
        if (result) return std::move(*result);
        notify(std::string(what) + ": " + describe(result.error()));
        return std::nullopt;
    }

    // The central config; on failure a `Can't read the {what}` notice and `nullopt`.
    std::optional<settings::app::Config> read_config(std::string_view what);

    // Edits the central config; on failure a `Can't save the {what}` notice and `false`.
    template <typename F>
    bool edit_config(std::string_view what, F&& change) {
        auto result = services.storage.config.edit([&](settings::app::Config& config) {
            change(config);
            return true;
        });
        if (result) return true;
        notify("Can't save the " + std::string(what) + ": " + describe(result.error()));
        return false;
    }

    // Lists `info` among the app tools, replacing the entry of the same name. An unchanged
    // entry leaves the revision alone.
    void set_app_tool(loa::view::panel::ToolInfo info);
    // Changes whenever `app_tools` does.
    [[nodiscard]] std::uint64_t app_tools_rev() const noexcept { return app_tools_rev_; }

    void request(Request request) { requests_.push_back(std::move(request)); }
    // Adds the panel tab `id` when it isn't listed, opens the panel and shows the tab.
    void open_tab(std::string id) { requests_.emplace_back(request::OpenTab{std::move(id)}); }
    // Lists the panel tab `id` because something relevant to it just started, without taking
    // the panel from the tab the user reads; a closed panel opens on it.
    void reveal_tab(std::string id) { requests_.emplace_back(request::RevealTab{std::move(id)}); }

    // Replaces the chat with the `Area::view_screen` of area `area` until `close_screen`, Esc,
    // or another session is opened.
    void show_screen(std::string_view area) { screen_ = area; }
    void close_screen() { screen_.reset(); }
    // The area whose screen fills the center.
    [[nodiscard]] std::optional<std::string_view> screen() const noexcept { return screen_; }

    // Points the composer's harness picker and new sessions at the active session's harness.
    void show_harness();
    // Closes session `id`, deletes its journal and removes it from the sidebar.
    void discard_session(std::string_view id) { end_session(id, true); }
    // Closes session `id` because `successor` takes its place (`nullopt`: the next session that
    // starts), deleting its journal when `delete_` is set: its unsent composer text moves on to
    // the successor instead of being dropped.
    void replace_session(std::string_view id, std::optional<SessionId> successor, bool delete_);

    // The requests, transitions and successors the app drains (`App::settle`, the session
    // controller).
    std::vector<Request>& requests() noexcept { return requests_; }
    std::vector<app_runtime::Transition>& transitions() noexcept { return transitions_; }
    std::map<SessionId, std::optional<SessionId>, std::less<>>& successors() noexcept { return successors_; }

  private:
    template <typename E>
    static std::string describe(const E& error) {
        if constexpr (requires { error.message(); }) return std::string(error.message());
        else if constexpr (requires { error.to_string(); }) return error.to_string();
        else return std::string(error);
    }
    // Closes session `id` in the runtime; the session events and harness shutdown it causes run
    // once the area returns.
    void end_session(std::string_view id, bool delete_);

    std::uint64_t app_tools_rev_ = 0;
    std::optional<std::string_view> screen_;
    std::optional<std::string> notice_;
    std::vector<Request> requests_;
    // What closing sessions asks of the app, applied before the requests.
    std::vector<app_runtime::Transition> transitions_;
    // Sessions closing because another takes their place, with the one that takes over their
    // unsent composer text: `nullopt` for the next session that starts.
    std::map<SessionId, std::optional<SessionId>, std::less<>> successors_;
};

// A long-running feed of messages (iced `Subscription`). The host keeps one running per key
// while `subscription()` lists it and stops it when it goes.
namespace subscription {
// `message` every `period`.
struct Every {
    std::string key;
    std::chrono::milliseconds period{0};
    Message message;
};
// Work that emits messages until it ends or `stop` is requested; it spawns its coroutine on
// the executor given.
struct Stream {
    std::string key;
    std::function<void(Task::Emit, std::stop_token, loa::runtime::Executor&)> run;
};
} // namespace subscription
using Subscription = std::variant<subscription::Every, subscription::Stream>;
using Subscriptions = std::vector<Subscription>;

// Draws widgets whose user input becomes app messages.
using Emit = Task::Emit;

// A feature area as the app holds it, every call with its messages erased to `Message`
// (Rust `AreaDyn`). See the file comment for how the app drives it.
class Area {
  public:
    virtual ~Area() = default;

    // The area's directory name: the prefix of its command ids and the key of its messages
    // and screen.
    [[nodiscard]] virtual std::string_view id() const = 0;
    virtual Task update(AreaMsg message, Ctx& ctx) = 0;
    // The palette rows of the area's commands. Collected each time the palette opens.
    [[nodiscard]] virtual std::vector<ui::shell::palette::Command> commands(const Ctx& ctx) const;
    // Runs the command `id` names; `nullopt` when it names none of this area's.
    virtual std::optional<Task> run(std::string_view id, Ctx& ctx);
    // Runs the `/` command `text` types; `nullopt` when it isn't this area's.
    virtual std::optional<Task> slash(std::string_view text, Ctx& ctx);
    [[nodiscard]] virtual std::vector<loa::view::session::composer::Command> slash_rows() const;
    // The panel tabs this area draws.
    [[nodiscard]] virtual std::vector<PanelTab> panel_tabs() const { return {}; }
    // Draws tab `id`; null when it isn't this area's.
    [[nodiscard]] virtual QWidget* view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const;
    // The full-center screen, drawn while `Ctx::screen` names this area.
    [[nodiscard]] virtual QWidget* view_screen(const Ctx& ctx, const Emit& send) const;
    // Adds to the config of every session started or resumed on `target`: MCP servers,
    // context, disabled tools, env.
    virtual void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const;
    // Host tools for a session on `target`.
    [[nodiscard]] virtual std::optional<HostToolSet> host_tools(const contracts::SessionConfig& config,
                                                                const Target& target, const Ctx& ctx) const;
    // Called for every area right before `prompt` goes to the running harness of session
    // `session`; areas may rewrite it.
    virtual void before_send(const SessionId& session, contracts::Prompt& prompt, Ctx& ctx);
    // Called for every change of every open session.
    virtual Task on_session_event(const SessionEvent& event, Ctx& ctx);
    // Called for every area after `project` became the active project; `cwd` is the active
    // folder (`request::ActivateProject`).
    virtual Task project_activated(std::string_view project, const std::filesystem::path& cwd, Ctx& ctx);
    [[nodiscard]] virtual Subscriptions subscription() const { return {}; }
};

// When a command shows in the palette.
template <typename S>
struct Listed {
    enum class Kind : std::uint8_t { Always, Session, Never, When, Each };
    Kind kind = Kind::Always;
    // `When`: under the title returned, when there is one.
    std::optional<std::string_view> (*when)(const S&, const Ctx&) = nullptr;
    // `Each`: once per `(argument, title)`; the command's id ends in `.` and the argument follows it.
    std::vector<std::pair<std::string, std::string>> (*each)(const S&, const Ctx&) = nullptr;

    [[nodiscard]] static constexpr Listed always() { return {Kind::Always}; }
    // While a session is active.
    [[nodiscard]] static constexpr Listed session() { return {Kind::Session}; }
    // Run by id, key or `/` name only.
    [[nodiscard]] static constexpr Listed never() { return {Kind::Never}; }
    [[nodiscard]] static constexpr Listed when_(decltype(when) f) { return {Kind::When, f, nullptr}; }
    [[nodiscard]] static constexpr Listed each_(decltype(each) f) { return {Kind::Each, nullptr, f}; }
};

// A `/` command's name and its description in the composer's list.
struct Slash {
    std::string_view name;
    std::string_view description;
};

// One command: listed in the palette, run by its id (an id ending in `.` takes the rest as its
// argument) and, with a `slash` name, typed as `/<name> <argument>` in the composer.
template <typename S, typename Run>
struct CommandSpec {
    std::string_view id;
    std::string_view title;
    std::string_view section;
    std::optional<Slash> slash;
    Listed<S> listed;
    Run run;

    [[nodiscard]] constexpr CommandSpec with_listed(Listed<S> l) const {
        auto c = *this;
        c.listed = l;
        return c;
    }
    // Also typed as `/<name>` in the composer, listed with `description`.
    [[nodiscard]] constexpr CommandSpec with_slash(std::string_view name, std::string_view description) const {
        auto c = *this;
        c.slash = Slash{name, description};
        return c;
    }

    // The argument when `id` names this command.
    [[nodiscard]] std::optional<std::string_view> matches(std::string_view id_) const {
        if (id.ends_with('.')) {
            if (id_.starts_with(id)) return id_.substr(id.size());
            return std::nullopt;
        }
        if (id_ == id) return std::string_view{};
        return std::nullopt;
    }

    // The palette rows of this command.
    [[nodiscard]] std::vector<ui::shell::palette::Command> palette(const S& state, const Ctx& ctx) const;
};

// An area's command: runs on the area with the argument.
template <typename A>
using AreaCommand = CommandSpec<A, Task (*)(A&, std::string_view, Ctx&)>;

// A `/` command typed in the composer: its name and its trimmed arguments.
[[nodiscard]] std::optional<std::pair<std::string_view, std::string_view>> split_slash(std::string_view text);

// Whether `ctx` has an active session (`Listed::Session`).
[[nodiscard]] bool has_active_session(const Ctx& ctx);

template <typename S, typename Run>
std::vector<ui::shell::palette::Command> CommandSpec<S, Run>::palette(const S& state, const Ctx& ctx) const {
    using ui::shell::palette::Command;
    using ui::shell::palette::CommandId;
    auto fixed = [&](std::string_view t) {
        std::vector<Command> rows;
        rows.emplace_back(CommandId(id), std::string(t), std::string(section));
        return rows;
    };
    switch (listed.kind) {
    case Listed<S>::Kind::Always: return fixed(title);
    case Listed<S>::Kind::Session: return has_active_session(ctx) ? fixed(title) : std::vector<Command>{};
    case Listed<S>::Kind::Never: return {};
    case Listed<S>::Kind::When: {
        auto t = listed.when ? listed.when(state, ctx) : std::nullopt;
        return t ? fixed(*t) : std::vector<Command>{};
    }
    case Listed<S>::Kind::Each: {
        std::vector<Command> rows;
        if (!listed.each) return rows;
        for (auto& [arg, t] : listed.each(state, ctx))
            rows.emplace_back(CommandId(std::string(id) + arg), std::move(t), std::string(section));
        return rows;
    }
    }
    return {};
}

// The command of `specs` that `id` names, with its argument: the one with exactly this id,
// else the first whose prefix it starts with.
template <typename S, typename Run>
[[nodiscard]] std::optional<std::pair<const CommandSpec<S, Run>*, std::string_view>> find_command(
    std::span<const CommandSpec<S, Run>> specs, std::string_view id) {
    for (const auto& spec : specs)
        if (spec.id == id) return std::pair{&spec, std::string_view{}};
    for (const auto& spec : specs)
        if (auto arg = spec.matches(id)) return std::pair{&spec, *arg};
    return std::nullopt;
}

// The first of `specs` whose `/` name `text` types, with its arguments.
template <typename S, typename Run>
[[nodiscard]] std::optional<std::pair<const CommandSpec<S, Run>*, std::string_view>> find_slash(
    std::span<const CommandSpec<S, Run>> specs, std::string_view text) {
    auto split = split_slash(text);
    if (!split) return std::nullopt;
    for (const auto& spec : specs)
        if (spec.slash && spec.slash->name == split->first) return std::pair{&spec, split->second};
    return std::nullopt;
}

// The composer rows of the `/` commands among `specs`.
template <typename S, typename Run>
[[nodiscard]] std::vector<loa::view::session::composer::Command> slash_rows(std::span<const CommandSpec<S, Run>> specs) {
    std::vector<loa::view::session::composer::Command> rows;
    for (const auto& spec : specs)
        if (spec.slash) rows.push_back({std::string(spec.slash->name), std::string(spec.slash->description)});
    return rows;
}

// `message` for area `id` (Rust `Message::area`).
template <typename M>
[[nodiscard]] Message area_message(std::string_view id, M message) {
    return msg::Area{id, AreaMsg::make(std::move(message))};
}

namespace detail {
template <typename T>
struct is_variant : std::false_type {};
template <typename... Ts>
struct is_variant<std::variant<Ts...>> : std::true_type {};

// `message` as an `M`: the type itself, or (for a variant `M`) one of its alternatives.
template <typename M>
std::optional<M> take_as(AreaMsg& message) {
    if (auto exact = std::move(message).template downcast<M>()) return exact;
    if constexpr (is_variant<M>::value) {
        std::optional<M> out;
        [&]<std::size_t... I>(std::index_sequence<I...>) {
            ((out ? void() : [&] {
                using Alt = std::variant_alternative_t<I, M>;
                if (auto alt = std::move(message).template downcast<Alt>()) out.emplace(std::in_place_index<I>, std::move(*alt));
            }()),
             ...);
        }(std::make_index_sequence<std::variant_size_v<M>>{});
        return out;
    }
    return std::nullopt;
}
} // namespace detail

// `Area` over a derived state with a typed message (Rust `Typed<A>`): `Derived` declares
// `static constexpr std::string_view ID`, `using Message = …` (a variant accepts any of its
// alternatives too), `Task update_typed(Message, Ctx&)` and, optionally,
// `static std::span<const AreaCommand<Derived>> COMMANDS()`.
template <typename Derived>
class TypedArea : public Area {
  public:
    [[nodiscard]] std::string_view id() const final { return Derived::ID; }

    // `message` for this area.
    template <typename M>
    [[nodiscard]] static Message message(M m) {
        return area_message(Derived::ID, std::move(m));
    }

    Task update(AreaMsg message, Ctx& ctx) final {
        auto typed = detail::take_as<typename Derived::Message>(message);
        if (!typed) {
            ctx.notify("The " + std::string(Derived::ID) + " area can't handle " + message.type_name());
            return Task::none();
        }
        return self().update_typed(std::move(*typed), ctx);
    }

    [[nodiscard]] std::vector<ui::shell::palette::Command> commands(const Ctx& ctx) const override {
        std::vector<ui::shell::palette::Command> rows;
        for (const auto& spec : specs()) {
            auto each = spec.palette(self(), ctx);
            rows.insert(rows.end(), std::make_move_iterator(each.begin()), std::make_move_iterator(each.end()));
        }
        return rows;
    }

    std::optional<Task> run(std::string_view id_, Ctx& ctx) override {
        auto found = find_command(specs(), id_);
        if (!found) return std::nullopt;
        return found->first->run(self(), found->second, ctx);
    }

    std::optional<Task> slash(std::string_view text, Ctx& ctx) override {
        auto found = find_slash(specs(), text);
        if (!found) return std::nullopt;
        return found->first->run(self(), found->second, ctx);
    }

    [[nodiscard]] std::vector<loa::view::session::composer::Command> slash_rows() const override {
        return loa::app::slash_rows(specs());
    }

  private:
    static std::span<const AreaCommand<Derived>> specs() {
        if constexpr (requires { Derived::COMMANDS(); }) return Derived::COMMANDS();
        else return {};
    }
    Derived& self() { return static_cast<Derived&>(*this); }
    const Derived& self() const { return static_cast<const Derived&>(*this); }
};

} // namespace loa::app
