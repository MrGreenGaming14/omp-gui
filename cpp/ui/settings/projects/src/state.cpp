// The project screen's reducer. Port of `crates/loa-ui-settings/src/projects/mod.rs`.

#include "loa/ui/settings/projects/state.hpp"

#include <algorithm>

#include "strings.hpp"

namespace loa::ui::settings::projects {

namespace {
template <class... F> struct overloaded : F... { using F::operator()...; };
template <class... F> overloaded(F...) -> overloaded<F...>;
} // namespace

State::State(std::string project) : project_(std::move(project)) {}

std::optional<Intent> State::update(Message m) {
    if (auto* l = std::get_if<Local>(&m)) return reduce(std::move(*l));
    auto e = std::get<Intent>(std::move(m));
    if (guard(e)) return e;
    return std::nullopt;
}

void State::set_omp_history(std::optional<OmpHistory> history) {
    omp_history_ = std::move(history);
    const bool loading = omp_history_ && std::holds_alternative<vs::omp_history::Loading>(*omp_history_);
    if (!loading && confirm_delete_history_ && !lists_omp_history(*confirm_delete_history_)) confirm_delete_history_.reset();
}

bool State::lists_omp_history(std::string_view id) const {
    if (!omp_history_) return false;
    const auto* ready = std::get_if<vs::omp_history::Ready>(&*omp_history_);
    return ready && std::ranges::any_of(ready->rows, [&](const auto& r) { return r.id == id; });
}

void State::set_overview(ProjectOverview overview, std::int64_t now) {
    const bool keep = is_dirty();
    const bool keep_ideas = ideas_edited();
    now_ = now;
    overview_ = std::move(overview);
    ideas_reloading_ = false;
    if (!keep) reset();
    else if (!keep_ideas) revert_ideas();
}

void State::revert_ideas() {
    const auto* stored = stored_ideas();
    ideas_ = stored ? *stored : std::string();
    ideas_changed_ = false;
}

void State::set_error(std::string error) {
    saving_ = false;
    ideas_reloading_ = false;
    error_ = std::move(error);
}

bool State::ideas_editable() const { return stored_ideas() && !ideas_reloading_; }

void State::saved(std::expected<std::pair<ProjectSettings, std::optional<std::string>>, std::string> result) {
    saving_ = false;
    if (!result) {
        error_ = std::move(result.error());
        return;
    }
    error_.reset();
    if (overview_) {
        overview_->settings = std::move(result->first);
        if (result->second) overview_->ideas = std::move(*result->second);
    }
}

void State::ideas_changed_outside(ProjectSettings settings) {
    saved(std::pair{std::move(settings), std::optional<std::string>()});
    ideas_changed_ = true;
}

ProjectSettings State::draft() const {
    const auto model = str::trim(digest_model_);
    ProjectSettings s;
    s.instructions = std::string(str::trim_end(instructions_));
    s.pinned = pinned_;
    if (!model.empty()) s.digest_model = std::string(model);
    if (overview_) s.folder = overview_->settings.folder;
    s.ideas_in_context = ideas_in_context_;
    return s;
}

const std::string* State::stored_ideas() const {
    if (!overview_ || !overview_->ideas) return nullptr;
    return &*overview_->ideas;
}

std::optional<std::string> State::ideas_edit() const {
    const auto* stored = stored_ideas();
    if (!stored || *stored == ideas_) return std::nullopt;
    return ideas_;
}

bool State::ideas_edited() const { return ideas_edit().has_value(); }

bool State::is_dirty() const { return (overview_ && overview_->settings != draft()) || ideas_edited(); }

void State::reset() {
    const auto settings = overview_ ? overview_->settings : ProjectSettings{};
    instructions_ = settings.instructions;
    pinned_ = settings.pinned;
    digest_model_ = settings.digest_model.value_or(std::string());
    ideas_in_context_ = settings.ideas_in_context;
    revert_ideas();
}

std::optional<Intent> State::reduce(Local m) {
    namespace l = local;
    return std::visit(
        overloaded{
            [&](l::Instructions& a) -> std::optional<Intent> {
                instructions_ = std::move(a.text);
                return std::nullopt;
            },
            [&](l::DigestModel& a) -> std::optional<Intent> {
                digest_model_ = std::move(a.model);
                return std::nullopt;
            },
            [&](l::PinPath& a) -> std::optional<Intent> {
                new_pin_path_ = std::move(a.path);
                return std::nullopt;
            },
            [&](l::PinNote& a) -> std::optional<Intent> {
                new_pin_note_ = std::move(a.note);
                return std::nullopt;
            },
            [&](l::AddPin&) -> std::optional<Intent> {
                const auto path = str::trim(new_pin_path_);
                const auto note = str::trim(new_pin_note_);
                if (!path.empty() && std::ranges::none_of(pinned_, [&](const auto& p) { return p.path == path; })) {
                    PinnedFile pin{std::string(path), std::nullopt};
                    if (!note.empty()) pin.note = std::string(note);
                    pinned_.push_back(std::move(pin));
                    new_pin_path_.clear();
                    new_pin_note_.clear();
                }
                return std::nullopt;
            },
            [&](l::RemovePin& a) -> std::optional<Intent> {
                if (a.index < pinned_.size()) pinned_.erase(pinned_.begin() + static_cast<std::ptrdiff_t>(a.index));
                return std::nullopt;
            },
            [&](l::Ideas& a) -> std::optional<Intent> {
                if (ideas_editable()) ideas_ = std::move(a.text);
                return std::nullopt;
            },
            [&](l::ShareIdeas& a) -> std::optional<Intent> {
                ideas_in_context_ = a.on;
                return std::nullopt;
            },
            [&](l::ReloadIdeas&) -> std::optional<Intent> {
                if (ideas_reloading_) return std::nullopt;
                ideas_changed_ = false;
                ideas_reloading_ = true;
                return Intent{vs::intent::ReloadIdeas{}};
            },
            [&](l::Save&) -> std::optional<Intent> {
                if (!is_dirty() || saving_ || ideas_reloading_) return std::nullopt;
                saving_ = true;
                error_.reset();
                return Intent{vs::intent::SaveSettings{project_, draft(), ideas_edit()}};
            },
            [&](l::Revert&) -> std::optional<Intent> {
                reset();
                error_.reset();
                return std::nullopt;
            },
            [&](l::SetIncluded& a) -> std::optional<Intent> {
                if (!overview_) return std::nullopt;
                auto it = std::ranges::find_if(overview_->sessions,
                                               [&](const auto& s) { return s.id == a.session && s.excluded == a.included; });
                if (it == overview_->sessions.end()) return std::nullopt;
                it->excluded = !a.included;
                return Intent{vs::intent::SetExcluded{std::move(a.session), !a.included}};
            },
            [&](l::Delete&) -> std::optional<Intent> {
                confirm_delete_ = true;
                return std::nullopt;
            },
            [&](l::CancelDelete&) -> std::optional<Intent> {
                confirm_delete_ = false;
                return std::nullopt;
            },
            [&](l::ConfirmDelete&) -> std::optional<Intent> {
                confirm_delete_ = false;
                return Intent{vs::intent::Delete{project_}};
            },
            [&](l::DeleteOmpHistory& a) -> std::optional<Intent> {
                confirm_delete_history_ = std::move(a.id);
                return std::nullopt;
            },
            [&](l::CancelDeleteOmpHistory&) -> std::optional<Intent> {
                confirm_delete_history_.reset();
                return std::nullopt;
            },
            [&](l::ConfirmDeleteOmpHistory&) -> std::optional<Intent> {
                if (!confirm_delete_history_) return std::nullopt;
                auto id = std::move(*confirm_delete_history_);
                confirm_delete_history_.reset();
                if (omp_history_busy_ || !lists_omp_history(id)) return std::nullopt;
                return Intent{vs::intent::DeleteOmpHistory{std::move(id)}};
            },
        },
        m);
}

// Sessions open while listed; a new chat starts in this project and its folder; the folder dialog starts at it.
bool State::guard(const Intent& e) const {
    const std::optional<std::string> cwd = overview_ ? overview_->cwd : std::nullopt;
    return std::visit(overloaded{
                          [&](const vs::intent::OpenOmpHistory& i) { return !omp_history_busy_ && lists_omp_history(i.id); },
                          [&](const vs::intent::OpenSession& i) {
                              return overview_ && std::ranges::any_of(overview_->sessions, [&](const auto& s) { return s.id == i.session; });
                          },
                          [&](const vs::intent::NewChat& i) { return i.project == project_ && i.cwd == cwd; },
                          [&](const vs::intent::ChangeFolder& i) { return i.project == project_ && i.current == cwd; },
                          [](const auto&) { return false; },
                      },
                      e);
}

} // namespace loa::ui::settings::projects
