#pragma once

// Qt views of the project screen and the New project screen. Each renders its `const State&` and
// reports input through one `message` signal; neither mutates the state.

#include <QWidget>

#include "loa/ui/settings/projects/create.hpp"
#include "loa/ui/settings/projects/state.hpp"

namespace loa::ui::settings::projects {

class ProjectView : public QWidget {
    Q_OBJECT

  public:
    explicit ProjectView(QWidget* parent = nullptr);
    void render(const State& state);

  signals:
    void message(const loa::ui::settings::projects::Message& m);

  private:
    QWidget* content_ = nullptr;
};

class CreateView : public QWidget {
    Q_OBJECT

  public:
    explicit CreateView(QWidget* parent = nullptr);
    void render(const create::State& state);

  signals:
    void message(const loa::ui::settings::projects::create::Message& m);

  private:
    QWidget* content_ = nullptr;
    bool focused_ = false;
};

} // namespace loa::ui::settings::projects
