#pragma once

// The shared building blocks of the settings screens, port of the crate root
// (`crates/loa-ui-settings/src/lib.rs`): kit-styled actions, fields, check boxes, sections,
// error boxes and the screen scroller, plus the row/column helpers iced's macros stand for.
// Internal to `loa_ui_settings`.

#include <cmath>
#include <functional>
#include <initializer_list>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include <QBoxLayout>
#include <QString>

#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"

class QCheckBox;
class QLineEdit;
class QPlainTextEdit;
class QScrollArea;

namespace loa::ui::settings::sw {

namespace theme = loa::ui::kit::theme;
namespace kw = loa::ui::kit::widgets;
using loa::ui::kit::ButtonKind;
using loa::ui::kit::Surface;
using loa::ui::kit::Tone;
using theme::LayoutRole;
using theme::SpaceRole;
using theme::TextRole;
using Callback = kw::Callback;

inline QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
inline int px(float f) { return static_cast<int>(std::lround(f)); }
inline int spi(SpaceRole r) { return px(theme::sp(r)); }

// A box of `items` (null entries skipped) with `spacing`.
QWidget* box(QBoxLayout::Direction dir, std::initializer_list<QWidget*> items, int spacing);
inline QWidget* hrow(std::initializer_list<QWidget*> items, int spacing = spi(SpaceRole::S)) {
    return box(QBoxLayout::LeftToRight, items, spacing);
}
inline QWidget* vcol(std::initializer_list<QWidget*> items, int spacing = spi(SpaceRole::Xs)) {
    return box(QBoxLayout::TopToBottom, items, spacing);
}
// Appends `w` (skipped when null) to the layout of `parent`.
void add(QWidget* parent, QWidget* w);
// Appends an expanding gap to the layout of `parent` (iced `Space::new().width(Fill)`).
void stretch(QWidget* parent);

// A text button in `kind`, disabled without `on`.
QPushButton* action(std::string_view s, ButtonKind kind, Callback on);
// A text input in the kit look; `on_input` gets every user edit. `name` is its accessible and object name.
QLineEdit* field(std::string_view placeholder, std::string_view value, std::function<void(std::string)> on_input,
                 std::string_view name = {});
// A check box labelled `s`; disabled without `toggle`.
QCheckBox* check(bool on, std::string_view s, std::function<void(bool)> toggle);
// A multi-line editor of `text`, `height` pixels high; read-only without `on_edit`.
QPlainTextEdit* editor(std::string_view text, std::string_view placeholder, std::function<void(std::string)> on_edit,
                       std::string_view name, int height);
// A headed section of a screen.
QWidget* section(std::string_view title, QWidget* body);
// Problems in a danger box, one per line.
QWidget* errors(const std::vector<std::string>& lines);
// `content` in a scroller, padded and at most `ContentWidth` wide when `center`.
QScrollArea* scroll(QWidget* content, bool center);

// Keeps keyboard focus and the cursor across a re-render that rebuilds `root`'s children:
// `capture` before, `restore` after, matching the focused input by object name.
class FocusKeeper {
  public:
    void capture(const QWidget& root);
    void restore(QWidget& root) const;
  private:
    QString name_;
    int cursor_ = -1;
};

// Swaps `old` (may be null) in `host`'s layout for `fresh`, keeping focus on the input that had
// it; returns `fresh`. Views rebuild their content this way on every render.
QWidget* swap_content(QWidget& host, QWidget* old, QWidget* fresh);


} // namespace loa::ui::settings::sw
