#include "loa/ui/app/smoke.hpp"

#include <cstdio>
#include <memory>

#include <QApplication>
#include <QWidget>
#include <QVBoxLayout>

#include "loa/ui/shell/view.hpp"
#include "loa/ui/session/composer/view.hpp"

namespace loa::ui::app {

Smoke& Smoke::registry() {
    static Smoke smoke = [] {
        Smoke s;
        s.add("kit", [] { return kit_gallery(); }, check_kit_gallery);
        s.add("composer", [] {
            namespace c = session::composer;
            auto* view = new c::ComposerView;
            c::State state;
            state.set_harnesses({{"omp", "omp", true, std::nullopt}, {"claude", "Claude", true, std::nullopt}}, "omp");
            state.set_models({{"opus", "Claude Opus 5.5", "anthropic", std::nullopt}}, "opus");
            state.set_efforts({"low", "medium", "high"}, "medium");
            state.set_access({{"ask", "Ask", "asks first"}}, "ask");
            state.set_workspace(c::Workspace{"~/omp-gui", "/home/user/omp-gui", "main"});
            state.set_projects({"Example"}, "Example");
            (void)state.set_text("compare");
            (void)state.attach_image(c::pasted("example.png", std::pair{640u, 480u}));
            (void)state.update(c::msg::Submit{});
            state.turn_started();
            state.set_activity(c::Activity::working("Reading the config\u2026", 0));
            (void)state.set_text("queued while it works");
            (void)state.update(c::msg::Submit{});
            (void)state.set_text("please ultrathink");
            view->resize(800, 360);
            view->render(state);
            return view;
        }, [](QWidget& view) {
            if (!view.findChild<QWidget*>(QStringLiteral("loa-composer-editor"))) return std::string("missing editor");
            if (!view.findChild<QWidget*>(QStringLiteral("composer controls"))) return std::string("missing controls");
            if (!view.findChild<QWidget*>(QStringLiteral("composer queue"))) return std::string("missing queue");
            if (!view.findChild<QWidget*>(QStringLiteral("composer working"))) return std::string("missing working indicator");
            if (!view.findChild<QWidget*>(QStringLiteral("composer stop"))) return std::string("missing stop button");
            return std::string();
        });
        // The session's agents panes beside the agent definitions screen of the settings.
        s.add("agents", [] {
            auto* panes = agents_screen();
            if (!panes) return static_cast<QWidget*>(nullptr);
            auto* screen = new QWidget;
            screen->setAccessibleName(QStringLiteral("agents"));
            auto* col = new QVBoxLayout(screen);
            col->addWidget(panes);
            col->addWidget(settings_agents_screen(), 1);
            screen->resize(1200, 1200);
            return screen;
        }, [](QWidget& screen) {
            if (auto e = check_agents_screen(screen); !e.empty()) return e;
            return check_settings_agents_screen(screen);
        });
        s.add("projects", [] { return settings_projects_screen(); }, check_settings_projects_screen);
        s.add("workflows", [] { return settings_workflows_screen(); }, check_settings_workflows_screen);
        s.add("panel", [] { return panel_screen(); }, check_panel_screen);
        s.add("shell", [] {
            auto* frame = new shell::ShellView;
            auto state = shell::layout::State::make({}).first;
            state.sidebar_mut().set_nodes({{"local", "This computer", true, true}});
            state.sidebar_mut().set_projects({"Example"});
            state.set_project_tabs(true);
            state.set_open_projects({"Example"});
            state.set_tabs({{"files", "Files", std::nullopt}});
            frame->resize(960, 640);
            frame->render(state);
            return frame;
        }, [](QWidget& frame) {
            const auto* top = frame.findChild<QWidget*>(QStringLiteral("top bar"));
            if (!top || top->height() != shell::topbar::HEIGHT) return std::string("top bar height");
            if (!frame.findChild<QWidget*>(QStringLiteral("sidebar"))) return std::string("missing sidebar");
            if (!frame.findChild<QWidget*>(QStringLiteral("panel"))) return std::string("missing panel");
            return std::string();
        });
        return s;
    }();
    return smoke;
}

void Smoke::add(std::string name, Build build, Check check) {
    screens_.insert_or_assign(std::move(name), Entry{std::move(build), std::move(check)});
}

std::vector<std::string> Smoke::names() const {
    std::vector<std::string> out;
    for (const auto& [name, _] : screens_) out.push_back(name);
    return out;
}

int Smoke::run(std::string_view screen) const {
    const auto it = screens_.find(screen);
    if (it == screens_.end()) {
        std::string known;
        for (const auto& n : names()) known += (known.empty() ? "" : ", ") + n;
        std::fprintf(stderr, "loa-qt: unknown smoke screen `%.*s` (known: %s)\n", static_cast<int>(screen.size()), screen.data(),
                     known.c_str());
        return 2;
    }
    std::unique_ptr<QWidget> w(it->second.build ? it->second.build() : nullptr);
    if (!w) {
        std::fprintf(stderr, "loa-qt: smoke screen `%.*s` did not build\n", static_cast<int>(screen.size()), screen.data());
        return 1;
    }
    w->show();
    QApplication::processEvents();
    w->repaint();
    QApplication::processEvents();
    if (it->second.check) {
        if (const auto error = it->second.check(*w); !error.empty()) {
            std::fprintf(stderr, "loa-qt: smoke screen `%.*s` failed: %s\n", static_cast<int>(screen.size()), screen.data(), error.c_str());
            return 1;
        }
    }
    w->close();
    std::fprintf(stdout, "loa-qt: smoke `%.*s` ok\n", static_cast<int>(screen.size()), screen.data());
    return 0;
}

} // namespace loa::ui::app
