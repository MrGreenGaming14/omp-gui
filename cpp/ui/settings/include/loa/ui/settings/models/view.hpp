#pragma once

// The models screen: sign in (`signin`), the session's model (`picker`) and omp's model
// settings (`settings`) under one page switch. Port of `crates/loa-ui-settings/src/models/mod.rs`
// plus the `view` halves of its pages. Views render `const State&` and report input through
// one `message` signal; they never mutate state.

#include <array>
#include <cstdint>
#include <functional>
#include <string_view>

#include <QWidget>

#include "loa/ui/settings/models/picker.hpp"
#include "loa/ui/settings/models/settings.hpp"
#include "loa/ui/settings/models/signin.hpp"

class QLineEdit;
class QScrollArea;

namespace loa::ui::settings::models {

// The pages of the models screen.
enum class Page : std::uint8_t { SignIn, Model, Settings, Workflows };

} // namespace loa::ui::settings::models

namespace loa::core {
template <> struct StrEnum<loa::ui::settings::models::Page> {
    static constexpr std::array<std::string_view, 4> values{"Sign in", "Model", "Settings", "Workflows"};
};
} // namespace loa::core

namespace loa::ui::settings::models {

// The screen showing `page`: the page switch and close button, then `body` (owned by the frame).
[[nodiscard]] QWidget* frame(Page page, QWidget* body, std::function<void(Page)> select, std::function<void()> close,
                             QWidget* parent = nullptr);

// A centered muted line standing in for a page that is loading or could not be read.
[[nodiscard]] QWidget* status(std::string_view message, QWidget* parent = nullptr);

// The model picker popup. Arrow keys and Escape in the search field map through `picker::State::key`.
class PickerView : public QWidget {
    Q_OBJECT

  public:
    explicit PickerView(QWidget* parent = nullptr);
    void render(const picker::State& state);
    // Runs the focus and snap operations an update asked for.
    void apply(const shell::Task& task);

  signals:
    void message(const loa::ui::settings::models::picker::Message& m);

  protected:
    bool eventFilter(QObject* watched, QEvent* event) override;

  private:
    QWidget* heading_ = nullptr;
    QLineEdit* search_ = nullptr;
    QScrollArea* list_ = nullptr;
};

// The model settings page.
class SettingsView : public QWidget {
    Q_OBJECT

  public:
    explicit SettingsView(QWidget* parent = nullptr);
    void render(const settings::State& state);

  signals:
    void message(const loa::ui::settings::models::settings::Message& m);

  private:
    QScrollArea* scroll_ = nullptr;
};

// The sign in page.
class SignInView : public QWidget {
    Q_OBJECT

  public:
    explicit SignInView(QWidget* parent = nullptr);
    void render(const signin::State& state);

  signals:
    void message(const loa::ui::settings::models::signin::Message& m);

  private:
    QScrollArea* scroll_ = nullptr;
};

} // namespace loa::ui::settings::models
