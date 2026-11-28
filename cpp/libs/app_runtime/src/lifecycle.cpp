#include "loa/app_runtime/runtime.hpp"

#include <algorithm>
#include <format>
#include <utility>

#include <asio/awaitable.hpp>

#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"

namespace loa::app_runtime {

namespace {

template <class... F>
struct Overloaded : F... {
    using F::operator()...;
};

void give_back(std::string notice, std::optional<contracts::Prompt> prompt, Transition& out) {
    out.events.emplace_back(runtime_event::Notice{std::move(notice)});
    if (prompt) out.events.emplace_back(runtime_event::PromptReturned{std::move(*prompt)});
}

// Each coroutine takes its captures by value, rather than closing over an
// effect lambda whose lifetime ends when the app calls it.
contracts::Awaitable<Completion> start_effect(OperationId operation, SessionId id, std::shared_ptr<contracts::Adapter> adapter,
                                               std::shared_ptr<bus::SessionWriter> writer, contracts::SessionConfig config,
                                               std::optional<contracts::Prompt> prompt) {
    std::shared_ptr<core::PatchSink> sink = std::move(writer);
    auto result = co_await adapter->start(std::move(config), std::move(sink));
    if (result)
        co_return completion::Started{operation, std::move(id), HarnessPtr(std::move(*result)), std::move(prompt)};
    co_return completion::Started{operation, std::move(id), std::unexpected(result.error().message()), std::move(prompt)};
}

contracts::Awaitable<Completion> resume_effect(OperationId operation, SessionId id, std::shared_ptr<contracts::Adapter> adapter,
                                                core::Dom dom, std::shared_ptr<core::PatchSink> sink,
                                                contracts::SessionConfig config, contracts::Prompt prompt) {
    auto result = co_await adapter->resume(std::move(config), dom, std::move(sink));
    if (result) co_return completion::Resumed{operation, std::move(id), HarnessPtr(std::move(*result)), std::move(prompt)};
    co_return completion::Resumed{operation, std::move(id), std::unexpected(ResumeError::from(result.error())), std::move(prompt)};
}

contracts::Awaitable<Completion> spawn_effect(OperationId operation, SessionId id, SpawnStart start,
                                               contracts::SessionConfig config, std::optional<contracts::Prompt> prompt) {
    auto result = co_await start(std::move(config));
    co_return completion::Spawned{operation, std::move(id), std::move(result), std::move(prompt)};
}

contracts::Awaitable<Completion> send_effect(HarnessPtr harness, contracts::Prompt prompt) {
    auto result = co_await harness->send(std::move(prompt));
    if (result) co_return completion::Done{std::expected<void, std::string>{}};
    co_return completion::Done{std::unexpected(result.error().message())};
}

contracts::Awaitable<Completion> interrupt_effect(HarnessPtr harness) {
    auto result = co_await harness->interrupt();
    if (result) co_return completion::Done{std::expected<void, std::string>{}};
    co_return completion::Done{std::unexpected(result.error().message())};
}

contracts::Awaitable<Completion> stop_effect(HarnessPtr harness, bool release) {
    auto result = co_await harness->shutdown();
    std::expected<void, std::string> done = result ? std::expected<void, std::string>{}
                                                    : std::unexpected(result.error().message());
    if (release) co_return completion::ShutDown{std::move(done)};
    co_return completion::Done{std::move(done)};
}

Effect shutdown(HarnessPtr harness) {
    return [harness = std::move(harness)]() mutable { return stop_effect(std::move(harness), false); };
}

Effect release(HarnessPtr harness) {
    return [harness = std::move(harness)]() mutable { return stop_effect(std::move(harness), true); };
}

} // namespace

std::string_view access_label(std::string_view id) {
    auto it = std::ranges::find(ACCESS, id, &AccessMode::id);
    return it == ACCESS.end() ? std::string_view() : it->label;
}

ResumeError ResumeError::from(const contracts::AdapterError& error) {
    if (error.get<contracts::AdapterError::Unsupported>() || error.get<contracts::AdapterError::Format>())
        return {Kind::Unsupported, error.message()};
    return {Kind::Failed, error.message()};
}

Transition Runtime::dispatch(Command command) {
    Transition out;
    std::visit(Overloaded{
                   [&](command::Start& c) { start(std::move(c.prompt), out); },
                   [&](command::Open& c) { open(std::move(c.id), out); },
                   [&](command::Send& c) { send(std::move(c.prompt), out); },
                   [&](command::Spawn& c) { spawn(std::move(c), out); },
                   [&](command::Interrupt&) { interrupt(out); },
                   [&](command::Close& c) { close(c.id, c.delete_, out); },
                   [&](command::SetAccess& c) { change_access(std::move(c.id), out); },
                   [&](command::Prepared& c) {
                       auto it = operations_.pending.find(c.operation);
                       if (it != operations_.pending.end() && it->second.launch) {
                           Launch launch = std::move(it->second.launch);
                           out.effects.push_back(launch(c.operation, std::move(c.config)));
                       }
                   },
                   [&](command::Completed& c) { complete(std::move(c.completion), out); },
               },
               command);
    return out;
}

contracts::SessionConfig Runtime::base_config(std::string_view session_id, std::string_view harness,
                                               const core::Dom* dom) const {
    const core::Node* root = dom ? dom->root() : nullptr;
    auto attr = [&](std::string_view name) -> std::optional<std::string_view> {
        return root ? root->str_attr(name) : std::nullopt;
    };
    contracts::SessionConfig config;
    config.session_id = session_id;
    config.node = node_;
    const auto cwd = attr(core::attr::CWD);
    config.cwd = cwd ? std::filesystem::path(*cwd) : cwd_;
    const auto project = attr(core::attr::PROJECT);
    config.project = project ? std::optional(std::string(*project)) : project_;
    const auto mode = attr(core::attr::MODE);
    config.mode = mode ? core::parse<core::SessionMode>(*mode).value_or(core::SessionMode::Normal)
                       : core::SessionMode::Normal;
    const Harness* entry = harnesses_.get(harness);
    config.executable = entry ? entry->executable : std::nullopt;
    config.approval_mode = access_;
    return config;
}

void Runtime::prepare(SessionId session, std::string harness, bool resume, contracts::SessionConfig config, Launch launch,
                      Transition& out) {
    const OperationId operation = operations_.next++;
    operations_.pending.insert_or_assign(operation, Pending{std::move(session), std::move(launch)});
    out.events.emplace_back(runtime_event::PrepareSession{operation, std::move(config), std::move(harness), resume});
}

void Runtime::start(std::optional<contracts::Prompt> prompt, Transition& out) {
    if (sessions_.starting()) return give_back("A session is already starting", std::move(prompt), out);
    if (!sessions_.harness) return give_back("No harness is available on this machine", std::move(prompt), out);
    std::string harness = *sessions_.harness;
    auto adapter = harnesses_.runnable(harness);
    if (!adapter) return give_back(std::format("{} is unavailable", harnesses_.label(harness)), std::move(prompt), out);
    auto created = create_session();
    if (!created) return give_back(created.error(), std::move(prompt), out);
    auto [id, writer] = std::move(*created);
    sessions_.set_starting(id);
    auto config = base_config(id.as_str(), harness, nullptr);
    SessionId session = id;
    Launch launch = [adapter = std::move(adapter), writer = std::move(writer), id = std::move(id),
                     prompt = std::move(prompt)](OperationId operation, contracts::SessionConfig config) mutable -> Effect {
        return [adapter = std::move(adapter), writer = std::move(writer), id = std::move(id),
                config = std::move(config), prompt = std::move(prompt), operation]() mutable {
            return start_effect(operation, std::move(id), std::move(adapter), std::move(writer), std::move(config),
                                std::move(prompt));
        };
    };
    prepare(std::move(session), std::move(harness), false, std::move(config), std::move(launch), out);
}

void Runtime::resume(contracts::Prompt prompt, Transition& out) {
    if (sessions_.starting()) return give_back("A session is already starting", std::move(prompt), out);
    const Session* session = sessions_.active();
    if (!session) return;
    SessionId id = session->id();
    const auto harness_id = session->harness_id();
    auto adapter = harness_id ? harnesses_.runnable(*harness_id) : nullptr;
    if (!harness_id || !adapter) {
        out.events.emplace_back(runtime_event::Notice{"This session's harness isn't available here; continuing in a new session"});
        return start(std::move(prompt), out);
    }
    std::string harness(*harness_id);
    core::Dom dom = session->dom();
    std::shared_ptr<core::PatchSink> sink = session->writer();
    auto config = base_config(id.as_str(), harness, &dom);
    sessions_.set_starting(id);
    sessions_.harness = harness;
    Launch launch = [adapter = std::move(adapter), dom = std::move(dom), sink = std::move(sink), id = id,
                     prompt = std::move(prompt)](OperationId operation, contracts::SessionConfig config) mutable -> Effect {
        return [adapter = std::move(adapter), dom = std::move(dom), sink = std::move(sink), id = std::move(id),
                config = std::move(config), prompt = std::move(prompt), operation]() mutable {
            return resume_effect(operation, std::move(id), std::move(adapter), std::move(dom), std::move(sink),
                                 std::move(config), std::move(prompt));
        };
    };
    prepare(std::move(id), std::move(harness), true, std::move(config), std::move(launch), out);
}

void Runtime::continue_in_new_session(contracts::Prompt prompt, Transition& out) const {
    std::optional<std::filesystem::path> folder;
    if (const Session* session = sessions_.active()) {
        folder = session_cwd(session->dom());
        if (folder && (!std::filesystem::is_directory(*folder) || *folder == cwd_)) folder.reset();
    }
    out.events.emplace_back(runtime_event::ContinueRequested{std::move(folder), std::move(prompt)});
}

void Runtime::send(contracts::Prompt prompt, Transition& out) {
    Session* session = sessions_.active_mut();
    if (!session) return;
    auto harness = session->harness();
    if (!harness) return resume(std::move(prompt), out);
    session->set_busy(true);
    out.events.emplace_back(runtime_event::TurnStarted{session->id()});
    out.effects.push_back([harness = std::move(harness), prompt = std::move(prompt)]() mutable {
        return send_effect(std::move(harness), std::move(prompt));
    });
}

void Runtime::spawn(Spawn spawn, Transition& out) {
    if (sessions_.starting()) {
        out.events.emplace_back(runtime_event::Notice{"A session is already starting"});
        return;
    }
    auto config = base_config(spawn.id.as_str(), spawn.harness, nullptr);
    sessions_.set_starting(spawn.id);
    Launch launch = [id = spawn.id, start = std::move(spawn.start), prompt = std::move(spawn.prompt)](
                        OperationId operation, contracts::SessionConfig config) mutable -> Effect {
        return [id = std::move(id), start = std::move(start), prompt = std::move(prompt), config = std::move(config),
                operation]() mutable { return spawn_effect(operation, std::move(id), std::move(start), std::move(config),
                                                          std::move(prompt)); };
    };
    prepare(std::move(spawn.id), std::move(spawn.harness), false, std::move(config), std::move(launch), out);
}

void Runtime::interrupt(Transition& out) {
    const Session* session = sessions_.active();
    if (!session || !session->is_busy() || !session->harness()) return;
    out.effects.push_back([harness = session->harness()]() mutable { return interrupt_effect(std::move(harness)); });
}

void Runtime::open(SessionId id, Transition& out) {
    if (sessions_.is_active(id.as_str())) return;
    if (auto loaded = load_session(id.as_str()); !loaded) {
        out.events.emplace_back(runtime_event::Notice{loaded.error()});
        return;
    }
    sessions_.set_active(id);
    out.events.emplace_back(runtime_event::Session{session_event::Activated{std::move(id)}});
}

void Runtime::close(const SessionId& id, bool delete_, Transition& out) {
    std::erase_if(operations_.pending, [&](const auto& entry) { return entry.second.session == id; });
    const bool was_active = sessions_.is_active(id.as_str());
    auto [session, error] = close_session(id.as_str(), delete_);
    if (error) out.events.emplace_back(runtime_event::Notice{std::move(*error)});
    if (!session) return;
    if (was_active) out.events.emplace_back(runtime_event::Session{session_event::Activated{std::nullopt}});
    out.events.emplace_back(runtime_event::Session{session_event::Closed{session->id()}});
    if (auto harness = session->harness()) out.effects.push_back(release(std::move(harness)));
}

FeedBatch Runtime::begin_feed(SessionId id, Feed feed) const {
    FeedBatch batch;
    batch.id_ = std::move(id);
    batch.active_ = sessions_.is_active(batch.id_.as_str());
    const Session* active = sessions_.active();
    batch.was_busy_ = batch.active_ && active && active->is_busy();
    std::visit(Overloaded{
                   [&](feed::Synced& synced) { batch.synced_ = std::move(synced.dom); },
                   [&](feed::Events& events) { batch.events_ = std::move(events.events); },
                   [&](feed::Ended&) { batch.finished_ = true; },
               },
               feed);
    return batch;
}

std::optional<Transition> Runtime::advance_feed(FeedBatch& batch) {
    if (batch.finished_) return std::nullopt;
    Transition out;
    if (batch.synced_) {
        changed(batch.id_, sessions_.sync(batch.id_.as_str(), std::exchange(batch.synced_, nullptr)), out);
    } else if (batch.index_ < batch.events_.size()) {
        changed(batch.id_, sessions_.apply(batch.id_.as_str(), std::move(batch.events_[batch.index_++])), out);
    } else {
        batch.finished_ = true;
        const Session* active = sessions_.active();
        const bool now_busy = batch.active_ && active && active->is_busy();
        if (now_busy && !batch.was_busy_) out.events.emplace_back(runtime_event::TurnStarted{batch.id_});
        if (batch.was_busy_ && !now_busy) {
            if (restart_for_access_) restart(out);
            out.events.emplace_back(runtime_event::TurnEnded{batch.id_});
        }
    }
    return out;
}

void Runtime::changed(const SessionId& id, Change change, Transition& out) const {
    std::visit(Overloaded{
                   [](change::None&) {},
                   [&](change::Loaded&) { out.events.emplace_back(runtime_event::Session{session_event::Loaded{id}}); },
                   [&](change::Patch& patch) {
                       if (patch.summary_changed) out.events.emplace_back(runtime_event::SummaryChanged{id});
                       if (patch.turn_changed) out.events.emplace_back(runtime_event::StatusChanged{id});
                       out.events.emplace_back(runtime_event::Session{session_event::Patch{id, std::move(patch.patch)}});
                       if (patch.derived) out.events.emplace_back(runtime_event::Session{std::move(*patch.derived)});
                   },
               },
               change);
}

void Runtime::complete(Completion completion, Transition& out) {
    if (auto* done = std::get_if<completion::Done>(&completion)) {
        if (!done->result) {
            out.events.emplace_back(runtime_event::Notice{done->result.error()});
            // The failed call may have been the send of the active session's turn: it ends.
            Session* session = sessions_.active_mut();
            if (session && session->is_busy()) {
                session->set_busy(false);
                out.events.emplace_back(runtime_event::TurnEnded{session->id()});
            }
        }
        return;
    }
    if (auto* shut = std::get_if<completion::ShutDown>(&completion)) {
        if (!shut->result) out.events.emplace_back(runtime_event::Notice{shut->result.error()});
        return;
    }
    OperationId operation = std::visit(Overloaded{
                                           [](const completion::Started& c) { return c.operation; },
                                           [](const completion::Resumed& c) { return c.operation; },
                                           [](const completion::Spawned& c) { return c.operation; },
                                           [](const auto&) -> OperationId { return 0; },
                                       },
                                       completion);
    if (operations_.pending.erase(operation) == 0) {
        // Its session closed meanwhile: stop what started for it.
        auto harness = std::visit(Overloaded{
                                      [](const completion::Started& c) { return c.result ? *c.result : HarnessPtr{}; },
                                      [](const completion::Resumed& c) { return c.result ? *c.result : HarnessPtr{}; },
                                      [](const completion::Spawned& c) { return c.result ? *c.result : HarnessPtr{}; },
                                      [](const auto&) { return HarnessPtr{}; },
                                  },
                                  completion);
        if (harness) out.effects.push_back(release(std::move(harness)));
        return;
    }
    std::visit(Overloaded{
                   [&](completion::Started& c) {
                       if (c.result) return started(std::move(c.id), std::move(*c.result), std::move(c.prompt), out);
                       sessions_.set_starting(std::nullopt);
                       // Nothing ran: drop the journal it was given.
                       auto [discarded, closed] = close_session(c.id.as_str(), true);
                       (void)discarded;
                       out.events.emplace_back(runtime_event::Notice{
                           closed ? std::format("{}; {}", c.result.error(), *closed) : c.result.error()});
                       out.events.emplace_back(runtime_event::StartFailed{std::move(c.id)});
                   },
                   [&](completion::Spawned& c) {
                       if (c.result) {
                           if (auto loaded = load_session(c.id.as_str()); loaded)
                               return started(std::move(c.id), std::move(*c.result), std::move(c.prompt), out);
                           else {
                               sessions_.set_starting(std::nullopt);
                               out.events.emplace_back(runtime_event::Notice{loaded.error()});
                               out.effects.push_back(release(std::move(*c.result)));
                           }
                       } else {
                           sessions_.set_starting(std::nullopt);
                           // Leave no half created session behind; it may not exist.
                           (void)library_->remove(c.id.as_str());
                           out.events.emplace_back(runtime_event::Notice{c.result.error()});
                       }
                   },
                   [&](completion::Resumed& c) {
                       sessions_.set_starting(std::nullopt);
                       if (c.result) {
                           Session* session = sessions_.get_mut(c.id.as_str());
                           if (!session) {
                               out.effects.push_back(release(std::move(*c.result)));
                               return;
                           }
                           session->attach(std::move(*c.result));
                           open(c.id, out);
                           out.events.emplace_back(runtime_event::SendRequested{std::move(c.id), std::move(c.prompt)});
                           return;
                       }
                       const ResumeError& error = c.result.error();
                       if (error.kind == ResumeError::Kind::Unsupported) {
                           out.events.emplace_back(runtime_event::Notice{
                               std::format("Can't resume this session ({}); continuing in a new session", error.message)});
                           continue_in_new_session(std::move(c.prompt), out);
                       } else if (error.message.find("not found") != std::string::npos) {
                           // The harness no longer has the session's own file.
                           out.events.emplace_back(runtime_event::Notice{
                               "This session's harness history is gone; continuing in a new session"});
                           continue_in_new_session(std::move(c.prompt), out);
                       } else {
                           out.events.emplace_back(runtime_event::Notice{error.message});
                           out.events.emplace_back(runtime_event::ResumeFailed{std::move(c.id)});
                       }
                   },
                   [](auto&) {},
               },
               completion);
}

void Runtime::started(SessionId id, HarnessPtr harness, std::optional<contracts::Prompt> prompt, Transition& out) {
    sessions_.set_starting(std::nullopt);
    // Discarded while starting.
    Session* session = sessions_.get_mut(id.as_str());
    if (!session) {
        out.effects.push_back(release(std::move(harness)));
        return;
    }
    session->attach(std::move(harness));
    sessions_.set_active(id);
    out.events.emplace_back(runtime_event::Session{session_event::Activated{id}});
    out.events.emplace_back(runtime_event::SessionStarted{id});
    if (prompt) out.events.emplace_back(runtime_event::SendRequested{std::move(id), std::move(*prompt)});
}

void Runtime::change_access(std::string id, Transition& out) {
    if (id == (access_ ? std::string_view(*access_) : ACCESS_DEFAULT)) return;
    const auto label = access_label(id);
    access_ = std::move(id);
    const Session* active = sessions_.active();
    const bool running = active && active->harness();
    if (running && active->is_busy()) {
        restart_for_access_ = true;
        out.events.emplace_back(runtime_event::Notice{std::format("{}: applies once this turn ends", label)});
        return;
    }
    out.events.emplace_back(runtime_event::Notice{std::format("{}: applies from your next message", label)});
    if (running) restart(out);
}

void Runtime::restart(Transition& out) {
    restart_for_access_ = false;
    Session* session = sessions_.active_mut();
    if (!session) return;
    if (auto harness = session->detach()) out.effects.push_back(shutdown(std::move(harness)));
}

} // namespace loa::app_runtime
