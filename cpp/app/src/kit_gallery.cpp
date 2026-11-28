// The `kit` smoke screen: a gallery of the UI kit.

#include <array>
#include <cmath>
#include <format>

#include <QAbstractButton>
#include <QFile>
#include <QGridLayout>
#include <QImage>
#include <QLabel>
#include <QPainter>
#include <QScrollArea>
#include <QVBoxLayout>

#include "loa/ui/app/smoke.hpp"
#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/text.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::app {
namespace {

using namespace loa::ui::kit;
using theme::SpaceRole;
using theme::TextRole;

int spi(SpaceRole r) { return static_cast<int>(std::lround(theme::sp(r))); }

struct Choice {
    std::string name;
    std::optional<Icon> glyph;
    [[nodiscard]] std::string label() const { return name; }
    [[nodiscard]] std::optional<Icon> icon() const { return glyph; }
};

constexpr std::array TONES = {Tone::Normal, Tone::Muted, Tone::Faint, Tone::Ok, Tone::Warn, Tone::Danger, Tone::Accent, Tone::OnAccent};

QWidget* icons_grid() {
    auto* w = new QWidget;
    w->setAccessibleName(QStringLiteral("icons"));
    auto* grid = new QGridLayout(w);
    grid->setSpacing(spi(SpaceRole::S));
    int row = 0;
    for (const Icon i : all_icons()) {
        int col = 0;
        for (const Tone t : TONES) {
            auto* l = new QLabel;
            l->setPixmap(tinted(i, TextRole::Body, t));
            l->setAccessibleName(QString::fromStdString(std::format("{} {}", icon_file(i), name(t))));
            grid->addWidget(l, row % 10, static_cast<int>((row / 10) * TONES.size()) + col++);
        }
        ++row;
    }
    return w;
}

QWidget* buttons_row() {
    auto* w = new QWidget;
    w->setAccessibleName(QStringLiteral("buttons"));
    auto* r = new QHBoxLayout(w);
    r->setSpacing(spi(SpaceRole::S));
    const std::pair<const char*, ButtonKind> kinds[] = {
        {"Primary", ButtonKind::primary()}, {"Accent", ButtonKind::accent()}, {"Secondary", ButtonKind::secondary()},
        {"Ghost", ButtonKind::ghost()},     {"Flat", ButtonKind::flat()},     {"Danger", ButtonKind::danger()},
    };
    for (const auto& [label, kind] : kinds) r->addWidget(widgets::text_button(label, kind, [] {}));
    r->addWidget(widgets::text_button("Disabled", ButtonKind::primary(), {}));
    r->addWidget(widgets::icon_button(Icon::Settings, "Settings", [] {}));
    r->addStretch(1);
    return w;
}

QWidget* virtual_rows() {
    auto* list = new widgets::VirtualList;
    for (int i = 0; i < 1000; ++i) list->heights().push(theme::text_size(TextRole::Body) + theme::sp(SpaceRole::S) * (1 + i % 3));
    list->set_paint_row([](QPainter& p, std::size_t i, const QRect& rect) {
        p.setPen(qt::qcolor(color(i % 2 ? Tone::Muted : Tone::Normal)));
        p.setFont(qt::font(TextRole::Body));
        p.drawText(rect.adjusted(spi(SpaceRole::S), 0, 0, 0), Qt::AlignVCenter, QString::fromStdString(std::format("Row {}", i)));
    });
    list->setMinimumHeight(static_cast<int>(std::lround(theme::width(theme::LayoutRole::LabelColumn))));
    list->refresh();
    list->scroll_to(500);
    return list;
}

} // namespace

QWidget* kit_gallery() {
    auto* scroll = new QScrollArea;
    scroll->setAccessibleName(QStringLiteral("kit gallery"));
    scroll->setWidgetResizable(true);
    auto* page = new QWidget;
    page->setAccessibleName(QStringLiteral("kit page"));
    qt::style_container(*page, Surface::Ground);
    auto* col = new QVBoxLayout(page);
    const int m = spi(SpaceRole::L);
    col->setContentsMargins(m, m, m, m);
    col->setSpacing(spi(SpaceRole::M));

    col->addWidget(widgets::section_header("UI kit", {widgets::icon_button(Icon::Search, "Search", [] {})}));
    for (const TextRole r : theme::all<TextRole>())
        col->addWidget(widgets::label(std::format("Text role {}", theme::name(r)), r, Tone::Normal));
    auto* tones = new QHBoxLayout;
    for (const Tone t : TONES) tones->addWidget(widgets::badge(name(t), t));
    tones->addStretch(1);
    col->addLayout(tones);
    col->addWidget(buttons_row());
    col->addWidget(widgets::tab_strip({{"Chat", Icon::Bot, true, [] {}, nullptr}, {"Files", Icon::Files, false, [] {}, nullptr}}));
    col->addWidget(widgets::card(widgets::two_line_row("Card row", "A faint detail line", false, [] {})));
    col->addWidget(widgets::menu_row(Icon::Folder, "Selected menu row", true, [] {}));
    col->addWidget(widgets::status_row(Icon::Cpu, "Harness", "ready", Tone::Ok, std::optional<std::string_view>("omp")));
    col->addWidget(widgets::meter(0.65F));
    col->addWidget(widgets::mono_block(text::ellipsize_text("~/.omp/agent/OMGUI-worktrees/issue-388-qt-app-skeleton",
                                                            theme::width(theme::LayoutRole::LabelColumn) * 2, theme::text_size(TextRole::Mono),
                                                            theme::Font::Monospace, text::EllipsisMode::Path)));
    widgets::Picker<Choice> picker({{"Dark", Icon::Monitor}, {"Light", std::nullopt}}, 0);
    picker.open = true;
    col->addWidget(picker.view("Theme", [](widgets::PickerMsg) {}));
    col->addWidget(widgets::collapsible("Details", true, [] {}, widgets::label("Collapsible body", TextRole::Label, Tone::Muted)));
    col->addWidget(virtual_rows());
    for (const Surface s : {Surface::Side, Surface::Raised, Surface::Attention, Surface::ErrorBox}) {
        auto* c = new QWidget;
        c->setAccessibleName(QString::fromStdString(std::string(name(s))));
        auto* l = new QVBoxLayout(c);
        l->addWidget(widgets::label(std::format("Surface {}", name(s)), TextRole::Label, Tone::Normal));
        qt::style_container(*c, s);
        col->addWidget(c);
    }
    col->addWidget(icons_grid());
    col->addWidget(widgets::empty_state(Icon::Sparkles, "Nothing here yet", "Start a session to see it here"));
    scroll->setWidget(page);
    scroll->resize(static_cast<int>(std::lround(theme::width(theme::LayoutRole::MainWidth))),
                   static_cast<int>(std::lround(theme::width(theme::LayoutRole::MainHeight))));
    return scroll;
}

std::string check_kit_gallery(QWidget& gallery) {
    const auto tint = color(Tone::Accent);
    for (const Icon i : all_icons()) {
        const auto resource = icon_resource(i);
        if (!QFile::exists(QString::fromUtf8(resource.data(), static_cast<qsizetype>(resource.size()))))
            return std::format("icon resource {} is missing", resource);
        const QImage img = render_icon(i, 24, tint);
        bool drawn = false;
        for (int y = 0; y < img.height() && !drawn; ++y)
            for (int x = 0; x < img.width() && !drawn; ++x) drawn = qAlpha(img.pixel(x, y)) > 0;
        if (!drawn) return std::format("icon {} rendered nothing", icon_file(i));
    }
    for (auto* b : gallery.findChildren<QAbstractButton*>())
        if (b->text().isEmpty() && b->accessibleName().isEmpty()) return "a button has neither text nor an accessible name";
    if (gallery.findChildren<QLabel*>().size() < static_cast<qsizetype>(ICON_COUNT * TONES.size()))
        return "the icon grid is incomplete";
    return {};
}

} // namespace loa::ui::app
