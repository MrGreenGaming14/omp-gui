#include "loa/app_runtime/runtime.hpp"

#include <format>

namespace loa::app_runtime {

Runtime::Runtime(RuntimeParts parts)
    : library_(std::move(parts.library)), harnesses_(std::move(parts.harnesses)), node_(std::move(parts.node)),
      cwd_(std::move(parts.cwd)), project_(std::move(parts.project)), access_(std::move(parts.access)) {}

void Runtime::set_active(std::string project, std::optional<std::filesystem::path> cwd) {
    project_ = std::move(project);
    if (cwd) cwd_ = std::move(*cwd);
}

std::expected<std::pair<SessionId, std::shared_ptr<bus::SessionWriter>>, std::string> Runtime::create_session() {
    return sessions_.create(*library_);
}

std::expected<void, std::string> Runtime::load_session(std::string_view id) { return sessions_.load(*library_, id); }

std::pair<std::optional<Session>, std::optional<std::string>> Runtime::close_session(std::string_view id, bool delete_) {
    return sessions_.close(*library_, id, delete_);
}

std::vector<std::string> Runtime::delete_empty(std::vector<std::string> empty) const {
    std::vector<std::string> problems;
    for (const auto& id : empty) {
        if (sessions_.get(id)) continue;
        auto removed = library_->remove(id);
        if (!removed) problems.push_back(std::format("Can't delete empty session {}: {}", id, removed.error().to_string()));
    }
    return problems;
}

} // namespace loa::app_runtime
