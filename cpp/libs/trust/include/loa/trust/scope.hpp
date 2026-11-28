#pragma once

// What a grant covers and what a peer asks for. `KINDS` describes every
// resource kind once; scope checks and the token Datalog are generated from it.

#include <array>
#include <cstdint>
#include <initializer_list>
#include <optional>
#include <set>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/core/value.hpp"
#include "loa/trust/error.hpp"

namespace loa::core {
class Dom;
}

namespace loa::trust {

// One permission; `ResourceKind::actions` says which a kind takes. Ordered as
// in Rust, which is the order sets of actions sort and serialize in.
enum class Action : std::uint8_t {
    view,    // read the session or watch the surface
    suggest, // send prompts or edits the owner applies after review
    drive,   // prompt the session, answer its decisions, use its tools; or send input to a surface
    fork,    // copy the session and run it on the peer's own machine and keys
    read,
    write,
    exec,
};

[[nodiscard]] std::string_view as_str(Action action) noexcept;
[[nodiscard]] std::optional<Action> parse_action(std::string_view text) noexcept;
// Actions granted along with this one.
[[nodiscard]] std::span<const Action> implied(Action action) noexcept;

using ActionSet = std::set<Action>;

// The resources of the issuing node. The alternatives are in `KINDS` order.
namespace resource {
// One session, by its `<session id>`.
struct Session {
    std::string id;
    friend bool operator==(const Session&, const Session&) = default;
};
// One `<turn>` subtree of a session.
struct Prompt {
    std::string session;
    core::NodeId turn;
    friend bool operator==(const Prompt&, const Prompt&) = default;
};
// Every session whose `<session project>` is this project.
struct Project {
    std::string id;
    friend bool operator==(const Project&, const Project&) = default;
};
// Every session the node creates at or after `after_ms`.
struct FutureSessions {
    std::int64_t after_ms = 0;
    friend bool operator==(const FutureSessions&, const FutureSessions&) = default;
};
// One SSH host as named in the Remote hosts list.
struct Ssh {
    std::string host;
    friend bool operator==(const Ssh&, const Ssh&) = default;
};
// One surface, by `surface://server/name` uri.
struct Surface {
    std::string uri;
    friend bool operator==(const Surface&, const Surface&) = default;
};
// Running sandbox tasks on the issuing node.
struct Sandbox {
    friend bool operator==(const Sandbox&, const Sandbox&) = default;
};
// One folder and everything under it, by normalized absolute path.
struct Folder {
    std::string path;
    friend bool operator==(const Folder&, const Folder&) = default;
};
} // namespace resource

using Resource = std::variant<resource::Session, resource::Prompt, resource::Project, resource::FutureSessions,
                              resource::Ssh, resource::Surface, resource::Sandbox, resource::Folder>;

// One kind of `Resource`: its name, the actions it takes and its Datalog.
struct ResourceKind {
    std::string_view name;
    std::span<const Action> actions;
    // Predicate of the right fact in the authority block: `right(values.., action)`.
    std::string_view right;
    // The request facts a right covers; `#i` stands for the right's i-th value.
    std::string_view covers;

    // How many values its right fact carries before the action.
    [[nodiscard]] std::size_t arity() const noexcept;
    // `$v0, $v1, ` for the right's values.
    [[nodiscard]] std::string vars() const;
    // `covers` with `#i` replaced by `value(i)`.
    template <class F>
    [[nodiscard]] std::string covers_with(F&& value) const {
        std::string body(covers);
        for (std::size_t i = 0; i < arity(); ++i) {
            const std::string from = "#" + std::to_string(i);
            const std::string to = value(i);
            for (auto pos = body.find(from); pos != std::string::npos; pos = body.find(from, pos + to.size()))
                body.replace(pos, from.size(), to);
        }
        return body;
    }
    [[nodiscard]] bool takes(Action action) const noexcept;
};

// Every resource kind, in `Resource` alternative order. The fact names are the
// token format: existing grants verify only while they stay the same.
extern const std::array<ResourceKind, 8> KINDS;

// The one value sandbox facts carry: the issuing node's sandbox.
inline constexpr std::string_view SANDBOX = "node";

[[nodiscard]] const ResourceKind& kind_of(const Resource& resource) noexcept;

// A resource and the actions granted on it, implied actions included.
struct Scope {
    Resource resource;
    ActionSet actions;

    // Rust `Scope::new`: checks the actions fit the resource and adds the implied ones it takes.
    [[nodiscard]] static Result<Scope> make(Resource resource, std::span<const Action> actions);
    [[nodiscard]] static Result<Scope> make(Resource resource, std::initializer_list<Action> actions) {
        return make(std::move(resource), std::span<const Action>(actions.begin(), actions.size()));
    }

    // Checks actions, prompt ids (fit an i64) and folder paths (absolute, normalized).
    [[nodiscard]] Result<void> validate() const;

    friend bool operator==(const Scope&, const Scope&) = default;
};

// Every folder from the root down to `path` (`/`, `/a`, `/a/b`); none when
// `path` isn't absolute or has `..` (or a leading `.`).
[[nodiscard]] std::optional<std::vector<std::string>> folder_ancestors(std::string_view path);

// The facts about a session grant checks need, read by the owner from its own
// DOM so a peer can't forge them.
struct SessionFacts {
    std::string id;
    std::int64_t created_ms = 0;
    std::optional<std::string> project;

    // Reads the facts from the `<session>` root; none for an empty DOM.
    [[nodiscard]] static std::optional<SessionFacts> from_dom(const core::Dom& dom);

    friend bool operator==(const SessionFacts&, const SessionFacts&) = default;
};

// The `<turn>` that `node` is in (or is), if any.
[[nodiscard]] std::optional<core::NodeId> turn_of(const core::Dom& dom, core::NodeId node);

// What a request acts on. Views borrow from the caller.
namespace target {
// A session; `turn` is the turn the requested element is in, none for the whole session.
struct Session {
    const SessionFacts* session = nullptr;
    std::optional<core::NodeId> turn;
};
struct Ssh {
    std::string_view host;
};
struct Surface {
    std::string_view uri;
};
struct Sandbox {};
// A folder by normalized absolute path: grants on it or above it cover it.
struct Folder {
    std::string_view path;
};
} // namespace target

using Target = std::variant<target::Session, target::Ssh, target::Surface, target::Sandbox, target::Folder>;

// One thing a peer wants to do.
struct Request {
    Action action;
    Target target;

    [[nodiscard]] static Request session(Action action, const SessionFacts& session) {
        return {action, target::Session{&session, std::nullopt}};
    }
    // `action` on an element inside `turn` of a session.
    [[nodiscard]] static Request turn(Action action, const SessionFacts& session, core::NodeId turn) {
        return {action, target::Session{&session, turn}};
    }
    [[nodiscard]] static Request ssh(Action action, std::string_view host) { return {action, target::Ssh{host}}; }
    [[nodiscard]] static Request surface(Action action, std::string_view uri) { return {action, target::Surface{uri}}; }
    [[nodiscard]] static Request sandbox(Action action) { return {action, target::Sandbox{}}; }
    [[nodiscard]] static Request folder(Action action, std::string_view path) { return {action, target::Folder{path}}; }

    // Rust `Debug`-like text for diagnostics.
    [[nodiscard]] std::string debug() const;
};

} // namespace loa::trust
