// Branching a session into a new one: fork, rewind and compaction share one spawn strategy
// (port of `loa-app/src/areas/sessions/branch.rs`).

#include "sessions.hpp"

#include "loa/app/perform.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"

namespace loa::app::areas::sessions {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

std::string_view verb(const Branch& branch) {
    return std::visit(overloaded{[](const branch::Fork&) { return std::string_view("fork"); },
                                 [](const branch::Rewind&) { return std::string_view("rewind"); },
                                 [](const branch::Compact&) { return std::string_view("compact"); }},
                      branch);
}

std::string title(const Branch& branch) {
    return std::visit(overloaded{[](const branch::Fork& b) { return "Fork of " + detail::title_of(b.target); },
                                 [](const branch::Rewind& b) { return detail::title_of(b.current) + " (rewound)"; },
                                 [](const branch::Compact& b) { return detail::title_of(b.source) + " (compacted)"; }},
                      branch);
}

// The harness the branch runs on.
std::optional<std::string> harness_of(const Branch& branch) {
    auto recorded = [](const core::Dom& target) -> std::optional<std::string> {
        const auto* root = target.root();
        if (!root) return std::nullopt;
        auto harness = root->str_attr(core::attr::HARNESS);
        return harness ? std::optional<std::string>(*harness) : std::nullopt;
    };
    return std::visit(overloaded{[&](const branch::Fork& b) { return recorded(b.target); },
                                 [&](const branch::Rewind& b) { return recorded(b.target); },
                                 [](const branch::Compact& b) { return std::optional<std::string>(b.running->harness()); }},
                      branch);
}

// Creates the new session in `library` and starts it with `config`.
contracts::Awaitable<std::expected<contracts::SessionPtr, std::string>> run(const Branch& branch,
                                                                            const contracts::Adapter& adapter,
                                                                            const session::Library& library,
                                                                            contracts::SessionConfig config) {
    auto text = [](session::Result<contracts::SessionPtr> started) -> std::expected<contracts::SessionPtr, std::string> {
        if (!started) return std::unexpected(started.error().to_string());
        return std::move(*started);
    };
    if (const auto* fork = std::get_if<branch::Fork>(&branch))
        co_return text(co_await session::fork(adapter, library, fork->target, std::move(config)));
    if (const auto* back = std::get_if<branch::Rewind>(&branch))
        co_return text(co_await session::rewind(adapter, library, back->current, back->target, std::move(config)));
    const auto& compact = std::get<branch::Compact>(branch);
    session::Compaction request{*compact.running, compact.source,    *compact.writer, std::move(config),
                                std::string(SMALL_MODEL), compact.instructions};
    auto summarizer = compact.small->on(SMALL_MODEL).first;
    auto compacted = co_await session::compact(adapter, library, *summarizer, std::move(request));
    if (!compacted) co_return std::unexpected(compacted.error().to_string());
    if (!*compacted) co_return std::unexpected(std::string("the harness compacted the session itself"));
    co_return std::move(**compacted);
}

// Titles the new session `id`.
std::expected<void, std::string> name(const session::Library& library, const std::string& id, const std::string& title) {
    auto live = library.live(id);
    if (!live) return std::unexpected("session " + id + " isn't open");
    auto root = live->writer->with_dom([](const core::Dom& dom) -> std::optional<NodeId> {
        const auto* node = dom.root();
        return node ? std::optional(node->id) : std::nullopt;
    });
    if (!root) return std::unexpected("session " + id + " has no root");
    auto committed = live->writer->commit(core::Draft::set(*root, std::string(core::attr::TITLE), title));
    if (!committed) return std::unexpected(core::to_string(committed.error()));
    return {};
}

contracts::Awaitable<std::expected<app_runtime::HarnessPtr, std::string>> start(
    std::shared_ptr<contracts::Adapter> adapter, std::shared_ptr<session::Library> library,
    std::shared_ptr<const Branch> branch, std::string title, std::string id, contracts::SessionConfig config) {
    config.named = true;
    auto running = co_await run(*branch, *adapter, *library, std::move(config));
    if (!running) co_return std::unexpected(running.error());
    if (auto named = name(*library, id, title); !named) co_return std::unexpected(named.error());
    co_return app_runtime::HarnessPtr(std::move(*running));
}

} // namespace

void State::spawn_branch(Ctx& ctx, const std::string& source, Branch branch) {
    const auto what = verb(branch);
    auto harness = harness_of(branch);
    if (!harness) {
        ctx.notify("Can't " + std::string(what) + " a session that never started");
        return;
    }
    auto adapter = ctx.runtime.harnesses().runnable(*harness);
    if (!adapter) {
        ctx.notify("Can't " + std::string(what) + ": " + std::string(ctx.runtime.harnesses().label(*harness)) +
                   " is unavailable");
        return;
    }
    auto id = app_runtime::new_session_id();
    if (std::holds_alternative<branch::Compact>(branch)) {
        compacted.insert_or_assign(id, source);
        ctx.notify("Summarizing the session with the small model…");
    } else {
        if (const auto* session = ctx.runtime.sessions().get(source); session && session->is_busy())
            ctx.notify("The earlier branch keeps running; stop it from its own session if it should end");
        branching_ = id;
    }
    auto shared = std::make_shared<const Branch>(std::move(branch));
    app_runtime::SpawnStart starter = [adapter, library = ctx.runtime.library(), shared, title = title(*shared),
                                       id](contracts::SessionConfig config) {
        return start(adapter, library, shared, title, id, std::move(config));
    };
    ctx.request(request::Spawn{app_runtime::Spawn{SessionId(id), std::move(*harness), std::move(starter), std::nullopt}});
}

void State::fork_active(Ctx& ctx) {
    const auto& active = ctx.runtime.sessions().active_id();
    if (!active) {
        ctx.notify("Open a session to fork it");
        return;
    }
    const std::string id = active->str();
    auto target = ctx.runtime.library()->load(id);
    if (!target) {
        ctx.notify("Can't fork this session: " + target.error().to_string());
        return;
    }
    spawn_branch(ctx, id, branch::Fork{std::move(*target)});
}

void State::fork_at(Ctx& ctx, const std::string& id, NodeId turn) {
    auto target = ctx.runtime.library()->through_turn(id, turn);
    if (!target) {
        ctx.notify("Can't fork this session: " + target.error().to_string());
        return;
    }
    spawn_branch(ctx, id, branch::Fork{std::move(*target)});
}

void State::rewind(Ctx& ctx, const std::string& id, NodeId node) {
    const auto& library = *ctx.runtime.library();
    auto current = library.load(id);
    if (!current) {
        ctx.notify("Can't rewind this session: " + current.error().to_string());
        return;
    }
    auto target = library.before_turn(id, node);
    if (!target) {
        ctx.notify("Can't rewind this session: " + target.error().to_string());
        return;
    }
    spawn_branch(ctx, id, branch::Rewind{std::move(*current), std::move(*target)});
}

Task State::compact(Ctx& ctx, std::string_view instructions) {
    std::optional<std::string> wanted;
    if (!instructions.empty()) wanted = std::string(instructions);
    const auto* session = ctx.runtime.sessions().active();
    if (!session) {
        ctx.notify("Open a session to compact it");
        return Task::none();
    }
    auto running = session->harness();
    if (!running) {
        ctx.notify("Compaction needs the session running: send a message to resume it first");
        return Task::none();
    }
    if (session->is_busy()) {
        ctx.notify("Wait for the turn to end before compacting");
        return Task::none();
    }
    if (detail::capabilities(ctx, running->harness()).has(contracts::Feature::Compaction)) {
        ctx.notify("Compacting…");
        return perform(
            [running, wanted]() -> contracts::Awaitable<std::expected<void, std::string>> {
                return [](app_runtime::HarnessPtr harness,
                          std::optional<std::string> what) -> contracts::Awaitable<std::expected<void, std::string>> {
                    auto result = co_await harness->compact(std::move(what));
                    if (!result) co_return std::unexpected(result.error().message());
                    co_return std::expected<void, std::string>{};
                }(running, wanted);
            },
            [](std::expected<void, std::string> result) -> loa::app::Message { return message(msg::Done{std::move(result)}); });
    }
    if (!small) {
        ctx.notify("Compaction needs a small model call through omp, and omp isn't installed");
        return Task::none();
    }
    auto writer = session->writer();
    auto source = writer->with_dom([](const core::Dom& dom) { return dom; });
    const std::string id = session->id().str();
    spawn_branch(ctx, id, branch::Compact{std::move(running), std::move(source), std::move(writer), small, std::move(wanted)});
    return Task::none();
}

} // namespace loa::app::areas::sessions
