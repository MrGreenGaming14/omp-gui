#pragma once

// Qt views of the diff, files and session tree panes, and the `PanelPane` descriptors the shell's
// right panel tabs use. Each view renders `const State&` and reports input through one `message`
// signal; none mutates its state.

#include <string_view>

#include <QWidget>

#include "loa/ui/panel/diff.hpp"
#include "loa/ui/panel/files.hpp"
#include "loa/ui/panel/session_tree.hpp"
#include "loa/view/theme.hpp"

class QAbstractScrollArea;
class QScrollArea;
class QVBoxLayout;

namespace loa::ui::panel {

// The kit `PanelPane`: a tab's id, title and icon.
struct PanelPane {
    std::string_view id;
    std::string_view title;
    loa::view::theme::Icon icon;
    friend bool operator==(const PanelPane&, const PanelPane&) = default;
};

inline constexpr PanelPane DIFF_PANE{"diff", "Diff", loa::view::theme::Icon::GitCompare};
inline constexpr PanelPane FILES_PANE{"files", "Files", loa::view::theme::Icon::Folder};
inline constexpr PanelPane SESSION_PANE{"session", "Session tree", loa::view::theme::Icon::GitFork};

[[nodiscard]] constexpr const PanelPane& pane(const diff::State&) noexcept { return DIFF_PANE; }
[[nodiscard]] constexpr const PanelPane& pane(const files::State&) noexcept { return FILES_PANE; }
[[nodiscard]] constexpr const PanelPane& pane(const session_tree::State&) noexcept { return SESSION_PANE; }

// One row of the files, session tree and agents lists.
[[nodiscard]] float list_row_height();
// Indentation per tree level.
[[nodiscard]] float indent();

class DiffBody;

class DiffView : public QWidget {
    Q_OBJECT

  public:
    explicit DiffView(QWidget* parent = nullptr);
    void render(const diff::State& state);
    // Runs the widget operations of an update (snap the body to the top).
    void apply(const diff::Task& task);

  signals:
    void message(const loa::ui::panel::diff::Message& m);

  private:
    QVBoxLayout* layout_ = nullptr;
    QWidget* head_ = nullptr;
    DiffBody* body_ = nullptr;
};

class FilesView : public QWidget {
    Q_OBJECT

  public:
    explicit FilesView(QWidget* parent = nullptr);
    void render(const files::State& state);

  signals:
    void message(const loa::ui::panel::files::Message& m);

  private:
    QScrollArea* scroll_ = nullptr;
};

class SessionTreeView : public QWidget {
    Q_OBJECT

  public:
    explicit SessionTreeView(QWidget* parent = nullptr);
    void render(const session_tree::State& state);

  signals:
    void message(const loa::ui::panel::session_tree::Message& m);

  private:
    QScrollArea* scroll_ = nullptr;
};

} // namespace loa::ui::panel
