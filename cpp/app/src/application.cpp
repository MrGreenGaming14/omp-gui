#include "loa/ui/app/application.hpp"

#include <cmath>
#include <cstdio>

#include <QApplication>
#include <QFont>
#include <optional>
#include <string>
#include <QStyleFactory>
#include <QStyleHints>

#include "loa/runtime/executor.hpp"
#include "loa/ui/app/asio_bridge.hpp"
#include "loa/ui/app/smoke.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/app/host.hpp"

namespace loa::ui::app {
namespace {

namespace theme = kit::theme;

constexpr std::string_view USAGE = "usage: loa-qt [--smoke <screen>]\n";

int px(float v) { return static_cast<int>(std::lround(v)); }

// The lightness the desktop prefers (`system` follows it).
theme::Variant system_variant(const QApplication& app) {
#if QT_VERSION >= QT_VERSION_CHECK(6, 5, 0)
    if (app.styleHints()->colorScheme() == Qt::ColorScheme::Light) return theme::Variant::Light;
    return theme::Variant::Dark;
#else
    const QColor window = app.palette().color(QPalette::Window);
    return window.lightness() > 127 ? theme::Variant::Light : theme::Variant::Dark;
#endif
}

} // namespace

Options parse_args(std::span<const std::string_view> args) {
    Options o;
    for (std::size_t i = 1; i < args.size(); ++i) {
        const auto a = args[i];
        if (a == "--smoke") {
            if (i + 1 >= args.size()) {
                o.error = "--smoke needs a screen name";
                return o;
            }
            o.smoke = std::string(args[++i]);
        } else if (a == "--help" || a == "-h") {
            o.help = true;
        } else if (a.starts_with('-')) {
            o.error = "unknown option " + std::string(a);
            return o;
        }
        // Positional arguments are Qt's (`-platform`, ...) or reserved for later issues.
    }
    return o;
}

Application::Application(int& argc, char** argv)
    : qt_(std::make_unique<QApplication>(argc, argv)),
      executor_(std::make_unique<loa::runtime::Executor>(2)),
      bridge_(std::make_unique<AsioBridge>(*executor_, qt_.get())),
      themes_(theme::ThemeRegistry::builtin()) {
    // Qt strips its own options from argv; keep what is left.
    for (int i = 0; i < argc; ++i) args_.emplace_back(argv[i]);
    QApplication::setApplicationName(QStringLiteral("Loa"));
    QApplication::setStyle(QStyleFactory::create(QStringLiteral("Fusion")));
    QFont font(QStringLiteral("Noto Sans"));
    font.setPixelSize(px(theme::text_size(theme::TextRole::Body)));
    QApplication::setFont(font);
    apply_theme(theme::Appearance{}, system_variant(*qt_));
}

Application::~Application() {
    bridge_.reset();
    executor_->stop();
    executor_.reset();
}

std::vector<theme::ThemeError> Application::apply_theme(const theme::Appearance& a, theme::Variant preferred) {
    auto [set, errors] = themes_.tokens(a, preferred);
    const auto palette = kit::qt::palette(set);
    const auto qss = kit::qt::app_qss(set);
    theme::set_active(std::move(set));
    QApplication::setPalette(palette);
    qt_->setStyleSheet(QString::fromStdString(qss));
    return errors;
}

int Application::run() {
    const auto o = parse_args(args_);
    if (!o.error.empty()) {
        std::fprintf(stderr, "loa-qt: %s\n%.*s", o.error.c_str(), static_cast<int>(USAGE.size()), USAGE.data());
        return 2;
    }
    if (o.help) {
        std::fprintf(stdout, "%.*s", static_cast<int>(USAGE.size()), USAGE.data());
        return 0;
    }
    if (o.smoke) return Smoke::registry().run(*o.smoke);

    // The folder argument: the first positional one Qt left.
    std::optional<std::string> folder;
    for (std::size_t i = 1; i < args_.size(); ++i)
        if (!args_[i].starts_with('-')) {
            folder = std::string(args_[i]);
            break;
        }
    auto made = loa::app::App::make(std::move(folder));
    if (!made) {
        std::fprintf(stderr, "loa-qt: %s\n", made.error().c_str());
        return 1;
    }
    auto [app, task] = std::move(*made);
    auto open = app->open_main();
    loa::app::AppHost host(std::move(app), *bridge_);
    task.append(std::move(open));
    host.start(std::move(task));
    return QApplication::exec();
}

} // namespace loa::ui::app
