#include "loa/contracts/harness.hpp"

#include <asio.hpp>

#include <format>

namespace loa::contracts {

namespace {

template <class... F>
struct Overload : F... {
    using F::operator()...;
};

AdapterError unsupported(std::string_view harness, std::string_view what) {
    return AdapterError::Unsupported{harness, what};
}

} // namespace

std::string AdapterError::message() const {
    return std::visit(
        Overload{
            [](const Unsupported& e) { return std::format("{} doesn't support {}", e.harness, e.what); },
            [](const Busy&) { return std::string("a turn is already running"); },
            [](const NotInstalled& e) { return std::format("the harness executable `{}` wasn't found", e.executable); },
            [](const Version& e) { return std::format("{} version mismatch: {}", e.harness, e.detail); },
            [](const Closed&) { return std::string("the session is closed"); },
            [](const UnknownDecision& e) { return std::format("unknown decision {}", loa::core::to_string(e.decision)); },
            [](const Protocol& e) { return "harness protocol error: " + e.detail; },
            [](const NotAddressable& e) { return e.reason; },
            [](const Rejected& e) { return e.reason; },
            [](const Format& e) { return "malformed native session: " + e.detail; },
            [](const Commit& e) { return loa::core::to_string(e.error); },
            [](const Io& e) { return e.error.message(); },
        },
        kind_);
}

Awaitable<std::expected<std::vector<OutputBlock>, std::string>> HostTools::call_content(
    std::string session_id, std::string tool, std::string args) const {
    auto text = co_await call(std::move(session_id), std::move(tool), std::move(args));
    if (!text) co_return std::unexpected(std::move(text.error()));
    co_return std::vector<OutputBlock>{TextBlock{std::move(*text)}};
}

Awaitable<Result<>> HarnessSession::set_model(ModelSelection) const { co_return std::unexpected(unsupported(harness(), "set_model")); }

Awaitable<Result<>> HarnessSession::set_mode(loa::core::SessionMode) const { co_return std::unexpected(unsupported(harness(), "set_mode")); }

Awaitable<Result<std::vector<HarnessModel>>> HarnessSession::models() const {
    co_return std::unexpected(unsupported(harness(), "listing models"));
}

Awaitable<Result<>> HarnessSession::set_effort(std::string) const { co_return std::unexpected(unsupported(harness(), "set_effort")); }

Awaitable<Result<>> HarnessSession::compact(std::optional<std::string>) const {
    co_return std::unexpected(unsupported(harness(), "compact"));
}

Awaitable<Result<SubagentTranscript>> HarnessSession::subagent_transcript(std::string) const {
    co_return std::unexpected(unsupported(harness(), "reading subagent conversations"));
}

Awaitable<Result<>> HarnessSession::message_subagent(std::string, std::string) const {
    co_return std::unexpected(unsupported(harness(), "messaging subagents"));
}

Awaitable<Availability> Adapter::availability() const {
    co_return Availability::unavailable(AvailabilityStatus::Unknown, std::format("{} doesn't report availability", harness()));
}

Awaitable<Result<SessionPtr>> Adapter::fork(SessionConfig, const loa::core::Dom&, std::shared_ptr<loa::core::PatchSink>) const {
    co_return std::unexpected(unsupported(harness(), "fork"));
}

Result<> Adapter::import(const std::filesystem::path&, loa::core::PatchSink&) const {
    return std::unexpected(unsupported(harness(), "import"));
}

Result<> Adapter::export_(const loa::core::Dom&, const std::filesystem::path&) const {
    return std::unexpected(unsupported(harness(), "export"));
}

} // namespace loa::contracts
