#include "loa/ui/kit/qt.hpp"

#include <atomic>
#include <cmath>
#include <format>

#include <QColor>
#include <QFont>
#include <QFontDatabase>
#include <QPalette>
#include <QWidget>

namespace loa::ui::kit::qt {
namespace {

using theme::ColorRole;
using theme::TokenSet;

int px(float v) { return static_cast<int>(std::lround(v)); }

std::string rule(const std::string& sel, std::optional<theme::Color> bg, std::optional<theme::Color> fg,
                 const theme::Border& border, const std::string& extra = {}) {
    std::string body;
    body += "background: " + (bg ? css(*bg) : std::string("transparent")) + "; ";
    if (fg) body += "color: " + css(*fg) + "; ";
    body += std::format("border: {}px solid {}; border-radius: {}px; ", px(border.width), css(border.color), px(border.radius));
    return sel + " { " + body + extra + "}\n";
}

std::string scoped(QWidget& w) {
    static std::atomic<unsigned> next{0};
    if (w.objectName().isEmpty()) w.setObjectName(QString::fromStdString(std::format("loa_kit_{}", next++)));
    return "#" + w.objectName().toStdString();
}

} // namespace

QColor qcolor(theme::Color c) { return QColor::fromRgbF(c.r, c.g, c.b, c.a); }

std::string css(theme::Color c) {
    auto b = [](float v) { return std::clamp(px(v * 255.0F), 0, 255); };
    return std::format("rgba({}, {}, {}, {})", b(c.r), b(c.g), b(c.b), b(c.a));
}

QFont font(theme::TextRole r, const TokenSet& t) {
    QFont f = t.font(r) == theme::Font::Monospace ? QFontDatabase::systemFont(QFontDatabase::FixedFont) : QFont();
    f.setPixelSize(std::max(1, px(t.text_size(r))));
    return f;
}

QFont font(theme::TextRole r) { return font(r, *theme::tokens()); }

QPalette palette(const TokenSet& t) {
    QPalette p;
    auto set = [&p](QPalette::ColorRole r, theme::Color c) { p.setColor(r, qcolor(c)); };
    set(QPalette::Window, t.color(ColorRole::Ground));
    set(QPalette::WindowText, t.color(ColorRole::Text));
    set(QPalette::Base, t.color(ColorRole::Side));
    set(QPalette::AlternateBase, t.color(ColorRole::Card));
    set(QPalette::ToolTipBase, t.color(ColorRole::Raised));
    set(QPalette::ToolTipText, t.color(ColorRole::Text));
    set(QPalette::PlaceholderText, t.color(ColorRole::Text3));
    set(QPalette::Text, t.color(ColorRole::Text));
    set(QPalette::Button, t.color(ColorRole::Card));
    set(QPalette::ButtonText, t.color(ColorRole::Text));
    set(QPalette::BrightText, t.color(ColorRole::OnAccent));
    set(QPalette::Highlight, t.color(ColorRole::Accent));
    set(QPalette::HighlightedText, t.color(ColorRole::OnAccent));
    set(QPalette::Link, t.color(ColorRole::Accent));
    set(QPalette::Light, t.color(ColorRole::Raised));
    set(QPalette::Midlight, t.color(ColorRole::Hover));
    set(QPalette::Mid, t.color(ColorRole::Line));
    set(QPalette::Dark, t.color(ColorRole::LineStrong));
    set(QPalette::Shadow, t.color(ColorRole::Shadow));
    const auto disabled = t.color(ColorRole::Text).scale_alpha(t.alpha(theme::AlphaRole::Disabled));
    for (auto r : {QPalette::WindowText, QPalette::Text, QPalette::ButtonText})
        p.setColor(QPalette::Disabled, r, qcolor(disabled));
    return p;
}

std::string button_qss(ButtonKind k, const std::string& sel, const TokenSet& t) {
    const auto pad = std::format("padding: {}px {}px; ", px(t.spacing(theme::SpaceRole::Xs)), px(t.spacing(theme::SpaceRole::S)));
    auto at = [&](ButtonStatus s, const std::string& suffix, const std::string& extra) {
        const auto b = button(k, s, t);
        return rule(sel + suffix, b.background, b.text_color, b.border, extra);
    };
    return at(ButtonStatus::Active, "", pad) + at(ButtonStatus::Hovered, ":hover", {}) +
           at(ButtonStatus::Pressed, ":pressed", {}) + at(ButtonStatus::Disabled, ":disabled", {});
}

std::string container_qss(Surface s, const std::string& sel, const TokenSet& t) {
    const auto c = container(s, t);
    return rule(sel, c.background, c.text_color, c.border);
}

std::string app_qss(const TokenSet& t) {
    const auto chip = px(t.radius(theme::RadiusRole::Chip));
    const auto side = css(t.color(ColorRole::Side));
    const auto text = css(t.color(ColorRole::Text));
    const auto line = css(t.color(ColorRole::Line));
    const auto strong = css(t.color(ColorRole::LineStrong));
    const auto accent = css(t.color(ColorRole::Accent));
    const auto faint = css(t.color(ColorRole::Text3));
    const auto selection = css(t.color(ColorRole::Accent).scale_alpha(t.alpha(theme::AlphaRole::Tint)));
    const auto pad = px(t.spacing(theme::SpaceRole::Xs));
    std::string q;
    q += std::format("QLineEdit, QTextEdit, QPlainTextEdit, QComboBox {{ background: {}; color: {}; border: 1px solid {}; "
                     "border-radius: {}px; padding: {}px; selection-background-color: {}; }}\n",
                     side, text, line, chip, pad, selection);
    q += std::format("QLineEdit:hover, QTextEdit:hover, QPlainTextEdit:hover, QComboBox:hover {{ border-color: {}; }}\n", strong);
    q += std::format("QLineEdit:focus, QTextEdit:focus, QPlainTextEdit:focus, QComboBox:on {{ border-color: {}; }}\n", accent);
    q += std::format("QCheckBox::indicator {{ border: 1px solid {}; border-radius: {}px; background: transparent; }}\n", strong, chip);
    q += std::format("QCheckBox::indicator:hover {{ border-color: {}; }}\n", faint);
    q += std::format("QCheckBox::indicator:checked {{ border-color: {0}; background: {0}; }}\n", accent);
    q += std::format("QScrollBar {{ background: transparent; border: none; width: {0}px; height: {0}px; }}\n", px(t.spacing(theme::SpaceRole::S)));
    q += std::format("QScrollBar::handle {{ background: {}; border-radius: 4px; }}\n", strong);
    q += std::format("QScrollBar::handle:hover, QScrollBar::handle:pressed {{ background: {}; }}\n", faint);
    q += "QScrollBar::add-line, QScrollBar::sub-line, QScrollBar::add-page, QScrollBar::sub-page { background: none; border: none; }\n";
    q += container_qss(Surface::Tooltip, "QToolTip", t);
    q += std::format("QSplitter::handle {{ background: {}; }}\n", line);
    return q;
}

void style_button(QWidget& w, ButtonKind k) {
    w.setAttribute(Qt::WA_Hover);
    w.setStyleSheet(QString::fromStdString(button_qss(k, scoped(w), *theme::tokens())));
}

void style_container(QWidget& w, Surface s) {
    w.setAttribute(Qt::WA_StyledBackground);
    w.setStyleSheet(QString::fromStdString(container_qss(s, scoped(w), *theme::tokens())));
}

} // namespace loa::ui::kit::qt
