#include "loa/ui/app/smoke.hpp"

#include <cstdio>
#include <memory>

#include <QApplication>
#include <QWidget>

#include "loa/ui/shell/view.hpp"

namespace loa::ui::app {

Smoke& Smoke::registry() {
    static Smoke smoke = [] {
        Smoke s;
        s.add("decisions", [] { return decisions_screen(); }, check_decisions_screen);
        s.add("kit", [] { return kit_gallery(); }, check_kit_gallery);
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
