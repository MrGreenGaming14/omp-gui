#include "loa/app/view/settings/projects.hpp"

#include <utility>

namespace loa::app::view::settings {

namespace {

namespace domain = ::loa::settings;
namespace shown = ::loa::view::settings::projects;

shown::DigestState digest_state_to_view(domain::DigestState state) {
    switch (state) {
    case domain::DigestState::Missing: return shown::DigestState::Missing;
    case domain::DigestState::Fresh: return shown::DigestState::Fresh;
    case domain::DigestState::Stale: return shown::DigestState::Stale;
    }
    return shown::DigestState::Missing;
}

shown::SessionRow session_to_view(const domain::SessionRow& s) {
    std::optional<shown::Digest> digest;
    if (s.digest) digest = shown::Digest{s.digest->summary, s.digest->decisions};
    return shown::SessionRow{
        .id = s.id,
        .title = s.title,
        .harness = s.harness,
        .last_active = s.last_active,
        .turns = s.turns,
        .excluded = s.excluded,
        .digest_state = digest_state_to_view(s.digest_state),
        .digest = std::move(digest),
        .digest_model = s.digest_model,
    };
}

} // namespace

shown::ProjectSettings settings_to_view(const domain::ProjectSettings& s) {
    shown::ProjectSettings out{.instructions = s.instructions, .digest_model = s.digest_model, .ideas_in_context = s.ideas_in_context};
    out.pinned.reserve(s.pinned.size());
    for (const auto& p : s.pinned) out.pinned.push_back(shown::PinnedFile{p.path, p.note});
    if (s.folder) out.folder = s.folder->string();
    return out;
}

domain::ProjectSettings settings_from_view(shown::ProjectSettings s) {
    domain::ProjectSettings out{.instructions = std::move(s.instructions), .digest_model = std::move(s.digest_model), .ideas_in_context = s.ideas_in_context};
    out.pinned.reserve(s.pinned.size());
    for (auto& p : s.pinned) out.pinned.push_back(domain::PinnedFile{std::move(p.path), std::move(p.note)});
    if (s.folder) out.folder = std::filesystem::path(std::move(*s.folder));
    return out;
}

shown::ProjectOverview overview_to_view(const domain::ProjectOverview& o) {
    shown::ProjectOverview out{.cwd = o.cwd, .settings = settings_to_view(o.settings), .ideas = std::unexpected(std::string())};
    if (o.ideas) out.ideas = o.ideas->text;
    else out.ideas = std::unexpected(o.ideas.error());
    out.sessions.reserve(o.sessions.size());
    for (const auto& s : o.sessions) out.sessions.push_back(session_to_view(s));
    return out;
}

} // namespace loa::app::view::settings
