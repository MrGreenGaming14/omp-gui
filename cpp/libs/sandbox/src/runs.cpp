#include "loa/sandbox/runs.hpp"

#include <algorithm>

namespace loa::sandbox {

std::optional<std::string_view> RunEntry::pending_patch() const {
    const auto* finished = std::get_if<run_state::Finished>(&state);
    if (finished && finished->patch && !finished->applied && !finished->patch->empty()) return *finished->patch;
    return std::nullopt;
}

RunEntry* RunLog::find(std::uint64_t run) {
    auto it = std::ranges::find_if(entries_, [&](const RunEntry& e) { return e.run.run == run; });
    return it == entries_.end() ? nullptr : &*it;
}

const RunEntry* RunLog::get(std::uint64_t run) const { return const_cast<RunLog*>(this)->find(run); }

void RunLog::record(SandboxEvent event) {
    if (auto* started = std::get_if<event::Started>(&event)) {
        entries_.push_back(RunEntry{std::move(started->run), run_state::Running{}});
    } else if (auto* finished = std::get_if<event::Finished>(&event)) {
        if (auto* entry = find(finished->run)) {
            auto files = finished->patch ? patch_files(*finished->patch) : std::vector<PatchFile>{};
            entry->state = run_state::Finished{std::move(finished->final_message), std::move(finished->patch), std::move(files), false};
        }
    } else if (auto* failed = std::get_if<event::Failed>(&event)) {
        if (auto* entry = find(failed->run)) entry->state = run_state::Failed{std::move(failed->error)};
    }
}

std::size_t RunLog::running() const {
    return static_cast<std::size_t>(std::ranges::count_if(entries_, [](const RunEntry& e) { return std::holds_alternative<run_state::Running>(e.state); }));
}

void RunLog::mark_applied(std::uint64_t run) {
    if (auto* entry = find(run)) {
        if (auto* finished = std::get_if<run_state::Finished>(&entry->state)) finished->applied = true;
    }
}

} // namespace loa::sandbox
