#include "loa/core/sink.hpp"

#include <format>

namespace loa::core {

namespace {
template <class... F>
struct overloaded : F... {
    using F::operator()...;
};
} // namespace

std::string to_string(const CommitError& e) {
    using namespace commit_error;
    return std::visit(overloaded{
                          [](const Apply& v) { return to_string(v.error); },
                          [](const NotWriter&) { return std::string("only the owner node writes this session"); },
                          [](const Closed&) { return std::string("the session is closed"); },
                          [](const Journal& v) { return std::format("journal write failed: {}", v.message); },
                      },
                      e);
}

loa::Error to_error(const CommitError& e) {
    using namespace commit_error;
    return std::visit(overloaded{
                          [](const Apply& v) { return to_error(v.error); },
                          [&](const NotWriter&) { return loa::Error(ErrorCode::permission_denied, to_string(e)); },
                          [&](const Closed&) { return loa::Error(ErrorCode::cancelled, to_string(e)); },
                          [&](const Journal&) { return loa::Error(ErrorCode::io, to_string(e)); },
                      },
                      e);
}

Dom MemorySink::dom() const {
    std::scoped_lock lock(mutex_);
    return dom_;
}

std::vector<Patch> MemorySink::patches() const {
    std::scoped_lock lock(mutex_);
    return patches_;
}

std::expected<Patch, CommitError> MemorySink::commit_at(Draft draft, std::int64_t ts) {
    std::scoped_lock lock(mutex_);
    auto patch = dom_.commit(std::move(draft), ts);
    if (!patch) return std::unexpected(commit_error::Apply{std::move(patch.error())});
    patches_.push_back(*patch);
    return std::move(*patch);
}

} // namespace loa::core
