#include "loa/harness/omp/session.hpp"

#include <asio/co_spawn.hpp>
#include <asio/detached.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>
#include <openssl/evp.h>

#include <algorithm>
#include <array>
#include <cctype>
#include <fstream>
#include <iterator>
#include <map>
#include <mutex>

#include "loa/core/writer.hpp"
#include "loa/harness/kit/ledger.hpp"
#include "loa/harness/kit/mapper.hpp"
#include "loa/harness/omp/mapper.hpp"
#include "loa/harness/omp/native.hpp"
#include "loa/harness/omp/rpc/typed.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/log.hpp"

namespace loa::harness::omp {
namespace {
using namespace loa::contracts;
using loa::core::DomWriter;
using loa::core::NodeId;
using loa::harness::kit::RpcError;

AdapterError rpc_error(const RpcError& error) {
    if (auto* version = error.get<RpcError::Version>()) {
        std::string offered;
        for (auto v : version->offered) { if (!offered.empty()) offered += ", "; offered += std::to_string(v); }
        return AdapterError(AdapterError::Version{HARNESS, "RPC versions [" + offered + "]; supported " +
            std::to_string(version->min) + "..=" + std::to_string(version->max)});
    }
    return error.to_adapter_error();
}
// How long omp may take to accept `RENAME`.
constexpr std::chrono::seconds RENAME_ACK{30};

AdapterError commit_error(loa::core::CommitError error) { return AdapterError(AdapterError::Commit{std::move(error)}); }
std::string base64(const std::string& bytes) {
    std::string out(4 * ((bytes.size() + 2) / 3), '\0');
    EVP_EncodeBlock(reinterpret_cast<unsigned char*>(out.data()),
                    reinterpret_cast<const unsigned char*>(bytes.data()), static_cast<int>(bytes.size()));
    return out;
}
class AppHost final : public rpc::HostHandler {
  public:
    AppHost(std::string id, std::shared_ptr<const HostTools> handler) : id_(std::move(id)), handler_(std::move(handler)) {}
    Awaitable<rpc::HostToolOutput> tool_call(rpc::HostToolCall call) override {
        auto result = co_await handler_->call_content(id_, std::move(call.tool_name), loa::runtime::json::dump(call.arguments));
        if (!result) co_return rpc::HostToolOutput::error(std::move(result.error()));
        std::vector<rpc::ToolContent> blocks;
        for (auto& block : *result) {
            if (auto* text = std::get_if<TextBlock>(&block)) blocks.emplace_back(rpc::TextContent{std::move(text->text)});
            else if (auto* image = std::get_if<ImageBlock>(&block))
                blocks.emplace_back(rpc::ImageContent{base64(std::string(reinterpret_cast<const char*>(image->data.data()), image->data.size())), std::move(image->mime)});
        }
        co_return rpc::HostToolOutput::blocks(std::move(blocks));
    }
  private:
    std::string id_;
    std::shared_ptr<const HostTools> handler_;
};
struct ChildTranscript {
    std::mutex mutex;
    std::uint64_t next_byte = 0;
    std::string records;
    std::optional<loa::core::Dom> dom;
};

std::expected<std::pair<NodeId, std::string>, AdapterError> session_file(const loa::core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return std::unexpected(AdapterError(AdapterError::Format{"missing session root"}));
    auto file = root->str_attr(native::X_SESSION_FILE);
    if (!file) return std::unexpected(AdapterError(AdapterError::Format{"resuming omp needs the native session file in " + std::string(native::X_SESSION_FILE)}));
    return std::pair{root->id, std::string(*file)};
}
std::vector<HarnessModel> harness_models(const rpc::ModelList& list) {
    std::vector<HarnessModel> result;
    result.reserve(list.models.size());
    for (const auto& model : list.models) {
        HarnessModel value{model.selector(), model.name.empty() ? model.id : model.name, model.provider, {}};
        if (model.reasoning) {
            loa::runtime::json::Value extra(model.extra);
            auto* thinking = extra.find("thinking");
            if (thinking) if (auto* efforts = thinking->find("efforts"); efforts && efforts->as_array())
                for (const auto& level : *efforts->as_array()) if (auto* s = level.as_string()) value.efforts.push_back(*s);
        }
        result.push_back(std::move(value));
    }
    return result;
}
} // namespace

struct SessionShared : std::enable_shared_from_this<SessionShared> {
    SessionShared(std::shared_ptr<loa::core::PatchSink> sink, NodeId root, bool titled, SessionState state,
                  rpc::WeakRpcClient client)
        : sink(std::move(sink)), mapper(root), state(state), client(std::move(client)) { mapper.titled(titled); }
    std::shared_ptr<loa::core::PatchSink> sink;
    std::mutex mapper_mutex;
    Mapper mapper;
    loa::harness::kit::StateWatch state;
    std::mutex failure_mutex;
    std::optional<std::string> failure;
    rpc::WeakRpcClient client;
    std::mutex transcripts_mutex;
    std::map<std::string, std::shared_ptr<ChildTranscript>> transcripts;

    AdapterError closed() {
        std::scoped_lock lock(failure_mutex);
        return failure ? AdapterError(AdapterError::Protocol{*failure}) : AdapterError(AdapterError::Closed{});
    }
    void fail(std::string message) {
        { std::scoped_lock lock(failure_mutex); failure = std::move(message); }
        state.set(SessionState::Closed);
    }
    // Has omp title the session once a turn completed (see `Mapper::take_rename`); the
    // mapper keeps the command's output out of the transcript. Gives up after `RENAME_ACK`.
    void rename_if_due(const asio::any_io_executor& executor) {
        { std::scoped_lock lock(mapper_mutex); if (!mapper.take_rename()) return; }
        auto active = client.upgrade();
        if (!active) return;
        asio::co_spawn(executor, [self = shared_from_this(), client = std::move(*active)]() -> Awaitable<void> {
            using namespace asio::experimental::awaitable_operators;
            asio::steady_timer timer(co_await asio::this_coro::executor, RENAME_ACK);
            auto raced = co_await (client.request<rpc::PromptAck>(rpc::command::Prompt{std::string(RENAME), {}, std::nullopt}) ||
                                   timer.async_wait(asio::use_awaitable));
            if (raced.index() == 0 && std::get<0>(raced)) co_return;
            loa::runtime::log::debug("loa::harness::omp", "omp did not title the session");
            std::scoped_lock lock(self->mapper_mutex);
            self->mapper.rename_failed();
        }, asio::detached);
    }
    void sync_state() {
        if (state.get() != SessionState::Closed) state.set(mapper.state());
    }
    // Applies `input` and answers the dialogs the mapper answers itself.
    Awaitable<Result<>> feed(Input input, const rpc::UiResponder* responder = nullptr) {
        Result<std::vector<Reply>> replies;
        {
            std::scoped_lock lock(mapper_mutex);
            replies = mapper.apply(std::move(input), DomWriter(*sink));
            sync_state();
        }
        if (!replies) co_return std::unexpected(replies.error());
        if (responder) for (const auto& reply : *replies) {
            auto sent = responder->respond(reply.response);
            if (!sent) co_return std::unexpected(rpc_error(sent.error()));
        }
        rename_if_due(co_await asio::this_coro::executor);
        co_return Result<>{};
    }
    Result<> write(std::string_view attr, loa::core::Value value) {
        std::scoped_lock lock(mapper_mutex);
        auto result = DomWriter(*sink).set(mapper.session(), attr, std::move(value));
        if (!result) return std::unexpected(commit_error(std::move(result.error())));
        return {};
    }
};

OmpSession::OmpSession(rpc::RpcClient client, std::shared_ptr<SessionShared> shared, std::chrono::milliseconds grace)
    : client_(std::move(client)), shared_(std::move(shared)), shutdown_grace_(grace) {}
OmpSession::~OmpSession() = default;
Result<> OmpSession::open() const {
    if (state() == SessionState::Closed) return std::unexpected(shared_->closed());
    return {};
}
SessionState OmpSession::state() const { return shared_->state.get(); }
Awaitable<Result<>> OmpSession::send(Prompt prompt) const {
    if (auto ok = open(); !ok) co_return ok;
    std::vector<rpc::ImageContent> images;
    for (const auto& attachment : prompt.attachments) {
        if (!attachment.mime || !attachment.mime->starts_with("image/")) continue;
        std::ifstream file(attachment.path, std::ios::binary);
        if (!file) co_return std::unexpected(AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "reading " + attachment.path.string())}));
        std::string bytes(std::istreambuf_iterator<char>{file}, {});
        if (!file.eof() && file.fail()) co_return std::unexpected(AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "reading " + attachment.path.string())}));
        images.push_back({base64(bytes), *attachment.mime});
    }
    auto id = client_.reserve_id();
    if (auto fed = co_await shared_->feed(Sent{id, prompt}); !fed) co_return fed;
    auto ack = co_await client_.request_as<rpc::PromptAck>(id, rpc::command::Prompt{std::move(prompt.text), std::move(images), rpc::StreamingBehavior::FollowUp});
    if (!ack) {
        { std::scoped_lock lock(shared_->mapper_mutex); shared_->mapper.withdraw(id); shared_->sync_state(); }
        // The withdrawn prompt may have been all that kept a due title waiting.
        shared_->rename_if_due(co_await asio::this_coro::executor);
        co_return std::unexpected(rpc_error(ack.error()));
    }
    if (ack->agent_invoked == false) {
        rpc::event::PromptResult settled{id, false, rpc::PromptStatus::Completed, std::nullopt, true};
        if (auto fed = co_await shared_->feed(InputFrame{rpc::Frame{settled}}); !fed) co_return fed;
    }
    co_return Result<>{};
}
Awaitable<Result<>> OmpSession::interrupt() const {
    if (auto ok = open(); !ok) co_return ok;
    if (state() != SessionState::Idle) {
        auto result = co_await client_.request(rpc::command::AbortAndRestoreQueue{});
        if (!result) co_return std::unexpected(rpc_error(result.error()));
    }
    co_return Result<>{};
}
Awaitable<Result<>> OmpSession::answer(NodeId decision, Answer answer) const {
    if (auto ok = open(); !ok) co_return ok;
    Result<std::optional<Reply>> reply;
    { std::scoped_lock lock(shared_->mapper_mutex); reply = shared_->mapper.answer(decision, answer, DomWriter(*shared_->sink)); shared_->state.set(shared_->mapper.state()); }
    if (!reply) co_return std::unexpected(reply.error());
    if (*reply) {
        auto sent = client_.respond_ui((*reply)->response);
        if (!sent) co_return std::unexpected(rpc_error(sent.error()));
        if ((*reply)->steer) { auto steered = co_await client_.request(rpc::command::Steer{*(*reply)->steer});
            if (!steered) co_return std::unexpected(rpc_error(steered.error())); }
    }
    co_return Result<>{};
}
Awaitable<Result<>> OmpSession::set_model(ModelSelection selection) const {
    if (auto ok = open(); !ok) co_return ok;
    if (selection.role) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "model role updates over RPC"}));
    auto parts = split_selector(selection.model);
    if (!parts) co_return std::unexpected(parts.error());
    auto result = co_await client_.request<rpc::ModelInfo>(rpc::command::SetModel{std::string(parts->first), std::string(parts->second)});
    if (!result) co_return std::unexpected(rpc_error(result.error()));
    co_return shared_->write(loa::core::attr::MODEL, result->selector());
}
Awaitable<Result<std::vector<HarnessModel>>> OmpSession::models() const {
    if (auto ok = open(); !ok) co_return std::unexpected(ok.error());
    auto result = co_await client_.request<rpc::ModelList>(rpc::command::GetAvailableModels{});
    if (!result) co_return std::unexpected(rpc_error(result.error()));
    co_return harness_models(*result);
}
Awaitable<Result<>> OmpSession::set_effort(std::string effort) const {
    if (auto ok = open(); !ok) co_return ok;
    auto result = co_await client_.request(rpc::command::SetThinkingLevel{effort});
    if (!result) co_return std::unexpected(rpc_error(result.error()));
    co_return shared_->write(loa::core::attr::EFFORT, std::move(effort));
}
Awaitable<Result<>> OmpSession::compact(std::optional<std::string> instructions) const {
    if (auto ok = open(); !ok) co_return ok;
    auto result = co_await client_.request<rpc::CompactionResult>(rpc::command::Compact{std::move(instructions)});
    if (!result) co_return std::unexpected(rpc_error(result.error()));
    co_return Result<>{};
}
Awaitable<Result<SubagentTranscript>> OmpSession::subagent_transcript(std::string id) const {
    if (auto ok = open(); !ok) co_return std::unexpected(ok.error());
    auto agents = co_await client_.request<rpc::SubagentList>(rpc::command::GetSubagents{});
    if (!agents) co_return std::unexpected(rpc_error(agents.error()));
    bool addressable = std::ranges::any_of(agents->subagents, [&](const auto& agent) {
        return agent.id == id && agent.status != "completed" && agent.status != "failed" &&
            agent.status != "aborted" && agent.status != "cancelled";
    });
    std::shared_ptr<ChildTranscript> child;
    { std::scoped_lock lock(shared_->transcripts_mutex); auto& slot = shared_->transcripts[id]; if (!slot) slot = std::make_shared<ChildTranscript>(); child = slot; }
    std::unique_lock lock(child->mutex);
    auto response = co_await client_.request<rpc::SubagentMessages>(rpc::command::GetSubagentMessages{id, child->next_byte});
    if (!response) co_return std::unexpected(rpc_error(response.error()));
    if (response->reset || !response->entries.empty()) child->dom.reset();
    if (response->reset) child->records.clear();
    for (const auto& entry : response->entries) { child->records += loa::runtime::json::dump(entry); child->records += '\n'; }
    child->next_byte = response->next_byte;
    if (!child->dom) {
        loa::core::MemorySink sink;
        if (!child->records.empty()) {
            auto imported = native::import_str(child->records, sink, "");
            if (!imported) co_return std::unexpected(imported.error());
        }
        child->dom = sink.dom();
    }
    co_return SubagentTranscript{*child->dom, addressable};
}
Awaitable<Result<>> OmpSession::message_subagent(std::string id, std::string text) const {
    if (auto ok = open(); !ok) co_return ok;
    if (std::ranges::all_of(text, [](unsigned char c) { return std::isspace(c); }))
        co_return std::unexpected(AdapterError(AdapterError::Protocol{"empty message"}));
    auto result = co_await client_.request(rpc::command::SteerSubagent{id, std::move(text)});
    std::string error;
    if (result) {
        auto* success = result->find("success");
        if (!success || !success->as_bool() || *success->as_bool()) co_return Result<>{};
        auto* reason = result->find("error");
        error = reason && reason->as_string() ? *reason->as_string() : "steer_subagent failed";
    } else {
        auto mapped = rpc_error(result.error());
        const auto* protocol = mapped.get<AdapterError::Protocol>();
        if (!protocol) co_return std::unexpected(std::move(mapped));
        error = protocol->detail;
    }
    if (error.find("Subagent not running: " + id) != std::string::npos)
        co_return std::unexpected(AdapterError(AdapterError::NotAddressable{id + " has finished, so omp no longer accepts messages for it"}));
    const auto prefix = error.find("Subagent refused the message: ");
    if (prefix != std::string::npos)
        co_return std::unexpected(AdapterError(AdapterError::Rejected{id + " refused the message: " + error.substr(prefix + std::string_view("Subagent refused the message: ").size())}));
    co_return std::unexpected(AdapterError(AdapterError::Protocol{std::move(error)}));
}
Awaitable<Result<>> OmpSession::wait_idle() const {
    if (co_await shared_->state.wait_settled() == SessionState::Idle) co_return Result<>{};
    co_return std::unexpected(shared_->closed());
}
Awaitable<Result<>> OmpSession::shutdown() const {
    auto result = co_await client_.shutdown(shutdown_grace_);
    if (!result) co_return std::unexpected(rpc_error(result.error()));
    shared_->state.set(SessionState::Closed);
    co_return Result<>{};
}

Awaitable<Availability> OmpAdapter::availability() const { co_return Availability::probe(config.executable, {"--version"}); }
Capabilities OmpAdapter::capabilities() const {
    constexpr std::array features{Feature::Rewind, Feature::Compaction, Feature::Subagents, Feature::SubagentMessages,
                                  Feature::OmpTools, Feature::AdvisorRules};
    constexpr std::array decisions{loa::core::DecisionKind::Approval, loa::core::DecisionKind::Question,
                                   loa::core::DecisionKind::Plan, loa::core::DecisionKind::Form};
    constexpr std::array context{ContextDelivery::SystemPrompt};
    return Capabilities::builder().schema(loa::core::SCHEMA_VERSION).features(features)
        .model_switch(ModelSwitch::PerCall).decisions(decisions).context(context)
        .host_tools().queue_mid_turn().effort().import().export_().build();
}
Awaitable<Result<std::unique_ptr<OmpSession>>> OmpAdapter::launch(SessionConfig session, Target target,
                                                                    std::shared_ptr<loa::core::PatchSink> sink,
                                                                    NodeId root, bool titled) const {
    auto options = spawn_options(config, session, target);
    if (!options) co_return std::unexpected(options.error());
    if (!session.host_tools.empty()) {
        if (!session.host_tool_handler) co_return std::unexpected(AdapterError(AdapterError::Protocol{"host tools require a handler"}));
        options->host = std::make_shared<AppHost>(session.session_id, session.host_tool_handler);
    }
    rpc::RpcResult<rpc::Spawned> spawned;
    for (;;) {
        spawned = co_await rpc::RpcClient::spawn(*options);
        if (spawned || !spawned.error().get<RpcError::Exited>() ||
            !drop_unavailable_tools(*options, spawned.error().get<RpcError::Exited>()->stderr)) break;
    }
    if (!spawned) co_return std::unexpected(rpc_error(spawned.error()));
    auto client = std::move(spawned->client);
    auto events = std::move(spawned->events);
    const std::array<rpc::Command, 3> setup{
        rpc::command::SetEventFilter{std::nullopt, rpc::MessageUpdates::Delta},
        rpc::command::SetSubagentSubscription{rpc::SubagentLevel::Events},
        rpc::command::SetAskDialog{true}};
    for (const auto& command : setup) {
        auto done = co_await client.request(command);
        if (!done) { (void)co_await client.shutdown(config.shutdown_grace); co_return std::unexpected(rpc_error(done.error())); }
    }
    if (!session.host_tools.empty()) {
        std::vector<rpc::HostToolDefinition> tools;
        for (const auto& tool : session.host_tools) {
            auto schema = loa::runtime::json::parse(tool.input_schema);
            if (!schema) { (void)co_await client.shutdown(config.shutdown_grace);
                co_return std::unexpected(AdapterError(AdapterError::Protocol{schema.error().message()})); }
            tools.push_back({tool.name, std::nullopt, tool.description, std::move(*schema), std::nullopt, "essential"});
        }
        auto done = co_await client.request(rpc::command::SetHostTools{std::move(tools)});
        if (!done) { (void)co_await client.shutdown(config.shutdown_grace); co_return std::unexpected(rpc_error(done.error())); }
    }
    auto current = co_await client.request<rpc::SessionState>(rpc::command::GetState{});
    if (!current) { (void)co_await client.shutdown(config.shutdown_grace); co_return std::unexpected(rpc_error(current.error())); }
    DomWriter writer(*sink);
    auto set = [&](std::string_view name, loa::core::Value value) -> Result<> {
        auto result = writer.set(root, name, std::move(value));
        if (!result) return std::unexpected(commit_error(std::move(result.error())));
        return {};
    };
    if (!current->session_id.empty()) if (auto r = set(loa::core::attr::NATIVE_ID, current->session_id); !r) co_return std::unexpected(r.error());
    if (current->session_file) if (auto r = set(native::X_SESSION_FILE, *current->session_file); !r) co_return std::unexpected(r.error());
    if (current->model) if (auto r = set(loa::core::attr::MODEL, current->model->selector()); !r) co_return std::unexpected(r.error());
    auto shared = std::make_shared<SessionShared>(sink, root, titled || session.named,
        current->is_streaming ? SessionState::Running : SessionState::Idle, client.downgrade());
    auto responder = client.ui_responder();
    auto executor = co_await asio::this_coro::executor;
    asio::co_spawn(executor, [shared, events = std::move(events), responder]() mutable -> Awaitable<void> {
        while (auto incoming = co_await events.recv()) {
            if (auto* frame = std::get_if<rpc::incoming::Event>(&*incoming)) {
                if (auto applied = co_await shared->feed(InputFrame{rpc::Frame{frame->event}}, &responder); !applied) {
                    shared->fail(applied.error().message()); co_return;
                }
            } else if (auto* ui = std::get_if<rpc::incoming::Ui>(&*incoming)) {
                if (auto applied = co_await shared->feed(InputFrame{rpc::Frame{ui->request}}, &responder); !applied) {
                    shared->fail(applied.error().message()); co_return;
                }
            } else if (auto* exited = std::get_if<rpc::incoming::Exited>(&*incoming)) {
                shared->fail("omp exited (" + (exited->status ? exited->status->to_string() : "unknown") + "): " + exited->stderr);
                co_return;
            }
        }
    }, asio::detached);
    co_return std::make_unique<OmpSession>(std::move(client), std::move(shared), config.shutdown_grace);
}
Awaitable<Result<SessionPtr>> OmpAdapter::start(SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const {
    auto root = loa::harness::kit::insert_root(DomWriter(*sink), HARNESS, config);
    if (!root) co_return std::unexpected(root.error());
    auto launched = co_await launch(std::move(config), Target{}, std::move(sink), *root, false);
    if (!launched) co_return std::unexpected(launched.error());
    co_return std::move(*launched);
}
Awaitable<Result<SessionPtr>> OmpAdapter::resume(SessionConfig config, const loa::core::Dom& dom,
                                                  std::shared_ptr<loa::core::PatchSink> sink) const {
    auto file = session_file(dom);
    if (!file) co_return std::unexpected(file.error());
    auto title = dom.root()->str_attr(loa::core::attr::TITLE);
    auto launched = co_await launch(std::move(config), Target::resume(file->second), std::move(sink), file->first,
                                    title && !title->empty());
    if (!launched) co_return std::unexpected(launched.error());
    co_return std::move(*launched);
}
Awaitable<Result<SessionPtr>> OmpAdapter::fork(SessionConfig config, const loa::core::Dom& dom,
                                                std::shared_ptr<loa::core::PatchSink> sink) const {
    auto file = session_file(dom);
    if (!file) co_return std::unexpected(file.error());
    auto title = dom.root()->str_attr(loa::core::attr::TITLE);
    auto launched = co_await launch(std::move(config), Target::resume(file->second), std::move(sink), file->first,
                                    title && !title->empty());
    if (!launched) co_return std::unexpected(launched.error());
    auto result = co_await (*launched)->client().request<rpc::CancellationResult>(rpc::command::Fork{});
    if (!result) co_return std::unexpected(rpc_error(result.error()));
    if (result->cancelled) co_return std::unexpected(AdapterError(AdapterError::Protocol{"omp cancelled fork"}));
    auto current = co_await (*launched)->client().request<rpc::SessionState>(rpc::command::GetState{});
    if (!current) co_return std::unexpected(rpc_error(current.error()));
    if (current->session_file) {
        auto set = DomWriter(*(*launched)->shared()->sink).set(file->first, native::X_SESSION_FILE, *current->session_file);
        if (!set) co_return std::unexpected(commit_error(std::move(set.error())));
    }
    co_return std::move(*launched);
}
Result<> OmpAdapter::import(const std::filesystem::path& path, loa::core::PatchSink& sink) const {
    auto result = native::import(path, sink, config.node);
    if (!result) return result;
    auto set = DomWriter(sink).set(NodeId{1}, native::X_SESSION_FILE, path.string());
    if (!set) return std::unexpected(commit_error(std::move(set.error())));
    return {};
}
Result<> OmpAdapter::export_(const loa::core::Dom& dom, const std::filesystem::path& out) const {
    auto text = native::export_string(dom);
    if (!text) return std::unexpected(text.error());
    std::ofstream file(out, std::ios::binary | std::ios::trunc);
    if (!file || !(file << *text)) return std::unexpected(AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "writing " + out.string())}));
    return {};
}

} // namespace loa::harness::omp
