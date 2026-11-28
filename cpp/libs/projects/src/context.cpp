#include "loa/projects/context.hpp"

#include "loa/core/value.hpp"
#include "loa/projects/text.hpp"
#include "loa/runtime/log.hpp"

namespace loa::projects {

using settings::ProjectSettings;

namespace {

std::string render_header(std::string_view project, const ProjectSettings& settings, const std::optional<std::string>& ideas,
                          bool with_digests) {
    std::string out = "# Project context: " + std::string(project) + "\n";
    const auto instructions = text::trim(settings.instructions);
    if (!instructions.empty()) out += "\n## Shared instructions\n\n" + std::string(instructions) + "\n";
    if (ideas)
        out += "\n## Ideas and notes\n\nThe user's own development ideas and notes for this project.\n\n" +
               std::string(text::trim(*ideas)) + "\n";
    if (!settings.pinned.empty()) out += "\n## Pinned files\n\n";
    for (const auto& file : settings.pinned) {
        out += "- `" + file.path + "`";
        if (file.note) {
            const auto note = text::trim(*file.note);
            if (!note.empty()) out += ": " + std::string(note);
        }
        out += "\n";
    }
    if (with_digests)
        out += "\n## Other sessions in this project\n\nDigests of other sessions. Call `project_search` to search "
               "them, and `project_read` with a session id (and a node id from the turns listed) to read the full record.\n";
    return out;
}

} // namespace

Result<PackedContext> build_context(const SessionSource& source, const Access& access, const SettingsStore& store,
                                    const ContextRequest& request) {
    auto settings = store.load(request.project);
    if (!settings) return std::unexpected(settings.error());
    std::optional<std::string> ideas;
    if (settings->ideas_in_context) {
        auto loaded = store.load_ideas(request.project);
        if (!loaded) {
            runtime::log::warn("loa_projects", "leaving unreadable ideas notes out of the project context: project=" +
                                          std::string(request.project) + " error=" + loaded.error().to_string());
        } else if (!text::trim(loaded->text).empty()) {
            ideas = std::move(loaded->text);
        }
    }
    auto visible = visible_sessions(source, access, request.project);
    if (!visible) return std::unexpected(visible.error());
    std::vector<Candidate> candidates;
    for (const auto& info : *visible) {
        if (request.session && *request.session == info.id) continue;
        auto dom = source.load(info.id);
        if (!dom) return std::unexpected(dom.error());
        if (!*dom) continue;
        if (auto c = Candidate::from_dom(**dom)) candidates.push_back(std::move(*c));
    }
    const auto header = estimate_tokens(render_header(request.project, *settings, ideas, true));
    const auto budget = request.budget_tokens > header ? request.budget_tokens - header : 0;
    auto digests = rank_and_pack(std::move(candidates), request.query, RankParams::make(request.now, budget));
    return PackedContext{std::string(request.project), std::move(*settings), std::move(ideas), std::move(digests)};
}

std::string render_digest_section(const Candidate& candidate) {
    const auto& d = candidate.digest.content;
    const std::string title = candidate.title ? *candidate.title : "Untitled session";
    std::string out = "\n### " + title + " (session `" + candidate.session + "`)\n\n";
    if (candidate.digest.stale) out += "_This digest may be out of date._\n\n";
    out += std::string(text::trim(d.summary)) + "\n";
    std::vector<std::string> files;
    for (const auto& f : d.files) files.push_back(f.note.empty() ? "`" + f.path + "`" : "`" + f.path + "`: " + f.note);
    const std::pair<std::string_view, const std::vector<std::string>*> lists[] = {
        {"Decisions", &d.decisions}, {"Files", &files}, {"Open items", &d.open_items}};
    for (const auto& [heading, items] : lists) {
        if (!items->empty()) out += "\n" + std::string(heading) + ":\n";
        for (const auto& item : *items) out += "- " + item + "\n";
    }
    if (!d.pointers.empty()) {
        out += "\nTurns: ";
        for (std::size_t i = 0; i < d.pointers.size(); ++i) out += (i ? ", " : "") + core::to_string(d.pointers[i]);
        out += "\n";
    }
    return out;
}

std::string render_markdown(const PackedContext& context) {
    auto out = render_header(context.project, context.settings, context.ideas, !context.digests.empty());
    for (const auto& d : context.digests) out += d.section;
    return out;
}

} // namespace loa::projects
