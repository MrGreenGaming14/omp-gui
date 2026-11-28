#include "loa/ui/kit/icons.hpp"

#include <cmath>
#include <memory>
#include <mutex>

#include <QIcon>
#include <QImage>
#include <QPainter>
#include <QPixmap>
#include <QSvgRenderer>

#include "loa/ui/kit/qt.hpp"

namespace loa::ui::kit {
namespace {

struct Entry {
    Icon icon;
    std::string_view file;
    std::string_view resource;
};

#define LOA_ICON(Name, file) Entry{Icon::Name, file, ":/loa/icons/" file ".svg"}

// The SVG of every `Icon`, in declaration order (checked by the tests).
constexpr std::array<Entry, ICON_COUNT> ICONS = {
    LOA_ICON(ArrowUp, "arrow-up"), LOA_ICON(Bot, "bot"), LOA_ICON(Brain, "brain"), LOA_ICON(Check, "check"),
    LOA_ICON(ChevronDown, "chevron-down"), LOA_ICON(ChevronLeft, "chevron-left"), LOA_ICON(ChevronRight, "chevron-right"),
    LOA_ICON(ChevronUp, "chevron-up"), LOA_ICON(CircleDot, "circle-dot"), LOA_ICON(Cpu, "cpu"),
    LOA_ICON(ExternalLink, "external-link"), LOA_ICON(FileText, "file-text"), LOA_ICON(Files, "files"),
    LOA_ICON(Folder, "folder"), LOA_ICON(Gauge, "gauge"), LOA_ICON(GitCompare, "git-compare"),
    LOA_ICON(GitFork, "git-fork"), LOA_ICON(Globe, "globe"), LOA_ICON(Image, "image"),
    LOA_ICON(LoaderCircle, "loader-circle"), LOA_ICON(Maximize, "maximize-2"), LOA_ICON(Minimize, "minimize-2"),
    LOA_ICON(Monitor, "monitor"), LOA_ICON(PanelRight, "panel-right"), LOA_ICON(PanelRightClose, "panel-right-close"),
    LOA_ICON(Pin, "pin"), LOA_ICON(Plus, "plus"), LOA_ICON(Search, "search"), LOA_ICON(Settings, "settings-2"),
    LOA_ICON(Shield, "shield"), LOA_ICON(SlidersHorizontal, "sliders-horizontal"), LOA_ICON(Sparkles, "sparkles"),
    LOA_ICON(Square, "square"), LOA_ICON(Terminal, "terminal"), LOA_ICON(TriangleAlert, "triangle-alert"),
    LOA_ICON(Wrench, "wrench"), LOA_ICON(X, "x"),
};

#undef LOA_ICON

constexpr bool in_order() {
    for (std::size_t i = 0; i < ICONS.size(); ++i)
        if (static_cast<std::size_t>(ICONS[i].icon) != i) return false;
    return true;
}
static_assert(in_order(), "ICONS must follow loa::view::theme::Icon's order");

// Renderers built once, indexed by discriminant; leaked so they outlive the application.
QSvgRenderer& renderer(Icon i) {
    static std::once_flag once;
    static std::array<QSvgRenderer*, ICON_COUNT>* table = nullptr;
    std::call_once(once, [] {
        table = new std::array<QSvgRenderer*, ICON_COUNT>{};
        for (const auto& e : ICONS) (*table)[static_cast<std::size_t>(e.icon)] = new QSvgRenderer(QString::fromUtf8(e.resource.data(), static_cast<qsizetype>(e.resource.size())));
    });
    return *(*table)[static_cast<std::size_t>(i)];
}

int pixels(float size) { return std::max(1, static_cast<int>(std::lround(size))); }

} // namespace

const std::array<Icon, ICON_COUNT>& all_icons() {
    static const auto all = [] {
        std::array<Icon, ICON_COUNT> a{};
        for (std::size_t i = 0; i < a.size(); ++i) a[i] = ICONS[i].icon;
        return a;
    }();
    return all;
}

std::string_view icon_file(Icon i) { return ICONS[static_cast<std::size_t>(i)].file; }
std::string_view icon_resource(Icon i) { return ICONS[static_cast<std::size_t>(i)].resource; }

QImage render_icon(Icon icon, int size, theme::Color color) {
    QImage img(size, size, QImage::Format_ARGB32_Premultiplied);
    img.fill(Qt::transparent);
    QPainter p(&img);
    p.setRenderHint(QPainter::Antialiasing);
    renderer(icon).render(&p, QRectF(0, 0, size, size));
    // Lucide strokes `currentColor`: keep the rendered coverage, replace the colour.
    p.setCompositionMode(QPainter::CompositionMode_SourceIn);
    p.fillRect(img.rect(), qt::qcolor(color));
    p.end();
    return img;
}

QPixmap colored(Icon i, float size, theme::Color color) { return QPixmap::fromImage(render_icon(i, pixels(size), color)); }

QPixmap tinted(Icon i, theme::TextRole size, Tone tone) { return colored(i, theme::text_size(size), kit::color(tone)); }

QPixmap icon(Icon i, float size) { return colored(i, size, kit::color(Tone::Normal)); }

QPixmap muted(Icon i, float size) {
    const auto t = theme::tokens();
    return colored(i, size, t->color(role(Tone::Normal)).scale_alpha(t->alpha(theme::AlphaRole::Muted)));
}

QIcon qicon(Icon i, theme::TextRole size, Tone tone) {
    const auto t = theme::tokens();
    const float s = t->text_size(size);
    const auto c = t->color(role(tone));
    QIcon out;
    out.addPixmap(colored(i, s, c), QIcon::Normal);
    out.addPixmap(colored(i, s, c.scale_alpha(t->alpha(theme::AlphaRole::Disabled))), QIcon::Disabled);
    return out;
}

} // namespace loa::ui::kit
