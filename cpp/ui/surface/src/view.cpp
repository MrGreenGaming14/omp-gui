#include "loa/ui/surface/view.hpp"

#include <cmath>
#include <format>

#include <QFocusEvent>
#include <QHBoxLayout>
#include <QImage>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QMouseEvent>
#include <QPainter>
#include <QPainterPath>
#include <QPixmap>
#include <QPushButton>
#include <QScrollBar>
#include <QStackedWidget>
#include <QVBoxLayout>
#include <QWheelEvent>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/surface/theme.hpp"

namespace loa::ui::surface {

namespace {

namespace th = kit::theme;
using kit::ButtonKind;
using kit::Tone;
using Icon = loa::view::theme::Icon;

// An object, not a function, so argument-dependent lookup can't pick `kit::theme::sp` instead.
constexpr auto sp = [](th::SpaceRole r) { return static_cast<int>(std::lround(th::sp(r))); };

QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }

void clear(QLayout* layout) {
    while (auto* item = layout->takeAt(0)) {
        if (auto* w = item->widget()) w->deleteLater();
        if (auto* l = item->layout()) clear(l);
        delete item;
    }
}

QLabel* body(std::string_view s) { return kit::widgets::label(s, th::TextRole::Label, Tone::Normal); }
QLabel* hint(std::string_view s) {
    auto* l = kit::widgets::label(s, th::TextRole::Caption, Tone::Muted);
    l->setWordWrap(true);
    return l;
}
QLabel* warning(std::string_view s) {
    auto* l = kit::widgets::label(s, th::TextRole::Caption, Tone::Warn);
    l->setWordWrap(true);
    return l;
}

QLabel* icon_label(Icon icon, th::TextRole role = th::TextRole::Body, Tone tone = Tone::Muted) {
    auto* l = new QLabel;
    l->setPixmap(kit::tinted(icon, role, tone));
    return l;
}

QLineEdit* field(std::string_view placeholder) {
    auto* e = new QLineEdit;
    e->setPlaceholderText(qs(placeholder));
    e->setAccessibleName(qs(placeholder));
    e->setFont(kit::qt::font(th::TextRole::Label));
    return e;
}

void set_text(QLineEdit* e, const std::string& s) {
    const QString q = qs(s);
    if (e->text() != q) e->setText(q);
}

// A launcher section: the kind's icon and title over `content`.
QWidget* section(SurfaceKindStyle kind, QWidget* content) {
    auto* w = new QWidget;
    auto* col = new QVBoxLayout(w);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(sp(th::SpaceRole::S));
    auto* title = new QHBoxLayout;
    title->setSpacing(sp(th::SpaceRole::S));
    title->addWidget(icon_label(kind.icon));
    title->addWidget(body(kind.title));
    title->addStretch();
    col->addLayout(title);
    col->addWidget(content);
    return w;
}

QWidget* column(std::initializer_list<QWidget*> items, int spacing) {
    auto* w = new QWidget;
    auto* col = new QVBoxLayout(w);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(spacing);
    for (auto* i : items) col->addWidget(i);
    return w;
}

QImage rgba_image(std::uint32_t w, std::uint32_t h, const std::uint8_t* data) {
    return QImage(data, static_cast<int>(w), static_cast<int>(h), static_cast<qsizetype>(w) * 4, QImage::Format_RGBA8888);
}

} // namespace

// ---- Viewer ----------------------------------------------------------------------------------

Viewer::Viewer(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("surface picture"));
    setMouseTracking(true);
    setFocusPolicy(Qt::StrongFocus);
    setMinimumSize(1, 1);
}

void Viewer::render(const PixelMode* mode) {
    mode_ = mode;
    const bool drawn = mode_ && mode_->cursor() && mode_->cursor()->on_surface;
    // The cursor is drawn here; the system one would double it.
    setCursor(drawn ? Qt::BlankCursor : Qt::ArrowCursor);
    update();
}

Size Viewer::panel_size() const { return {static_cast<float>(width()), static_cast<float>(height())}; }

void Viewer::pointer(PointerInput input) { emit message(msg::Pointer{panel_size(), std::move(input)}); }

void Viewer::paintEvent(QPaintEvent*) {
    if (!mode_) return;
    QPainter p(this);
    p.setRenderHint(QPainter::SmoothPixmapTransform);
    p.setRenderHint(QPainter::Antialiasing);
    if (const auto* fb = mode_->framebuffer(); fb && mode_->shown_seq()) {
        if (auto placed = fit(fb->width(), fb->height(), panel_size())) {
            const QImage image = rgba_image(fb->width(), fb->height(), fb->pixels().data());
            p.drawImage(QRectF(placed->rect.x, placed->rect.y, placed->rect.width, placed->rect.height), image);
        }
    }
    const auto local = mode_->cursor();
    if (!local || !local->on_surface) return;
    // A ring the size of the medium space around the tip, filled while a button is held.
    const auto t = th::tokens();
    const auto ring = t->color(th::ColorRole::CursorRing);
    const float line = t->spacing(th::SpaceRole::Xs) / 2.0F;
    const float radius = t->spacing(th::SpaceRole::M);
    const QPointF tip(local->at.x, local->at.y);
    if (local->pressed) {
        p.setPen(Qt::NoPen);
        p.setBrush(kit::qt::qcolor(ring.scale_alpha(t->alpha(th::AlphaRole::Tint))));
        p.drawEllipse(tip, radius, radius);
    }
    p.setBrush(Qt::NoBrush);
    p.setPen(QPen(kit::qt::qcolor(ring.scale_alpha(t->alpha(th::AlphaRole::Muted))), line));
    p.drawEllipse(tip, radius, radius);
    // An arrow pointer with its tip at the pointer.
    static constexpr std::array<std::pair<float, float>, 6> OUTLINE{
        {{0.0F, 16.0F}, {4.5F, 12.0F}, {7.5F, 18.5F}, {10.0F, 17.5F}, {7.0F, 11.0F}, {12.0F, 11.0F}}};
    QPainterPath arrow(tip);
    for (const auto& [x, y] : OUTLINE) arrow.lineTo(tip.x() + x, tip.y() + y);
    arrow.closeSubpath();
    p.fillPath(arrow, kit::qt::qcolor(t->color(th::ColorRole::Text)));
    p.strokePath(arrow, QPen(kit::qt::qcolor(t->color(th::ColorRole::Ground)), line / 2.0F));
}

void Viewer::resizeEvent(QResizeEvent* e) {
    QWidget::resizeEvent(e);
    if (!mode_ || panel_size() != mode_->panel()) emit message(msg::Panel{panel_size()});
}

void Viewer::mouseMoveEvent(QMouseEvent* e) {
    const QPointF pos = e->position();
    const bool over = rect().contains(pos.toPoint());
    const bool was_inside = std::exchange(inside_, over);
    if (over || dragging_ > 0) {
        pointer(pointer::Moved{{static_cast<float>(pos.x()), static_cast<float>(pos.y())}});
    } else if (was_inside) {
        pointer(pointer::Left{});
    }
}

void Viewer::leaveEvent(QEvent* e) {
    QWidget::leaveEvent(e);
    const bool was_inside = std::exchange(inside_, false);
    if (was_inside && dragging_ == 0) pointer(pointer::Left{});
}

void Viewer::mousePressEvent(QMouseEvent* e) {
    auto button = KeyMap::button(static_cast<int>(e->button()));
    if (!button) return;
    ++dragging_;
    const QPointF pos = e->position();
    pointer(pointer::Button{{static_cast<float>(pos.x()), static_cast<float>(pos.y())}, *button, true});
    e->accept();
}

void Viewer::mouseReleaseEvent(QMouseEvent* e) {
    auto button = KeyMap::button(static_cast<int>(e->button()));
    if (!button || dragging_ == 0) return;
    --dragging_;
    const QPointF pos = e->position();
    pointer(pointer::Button{{static_cast<float>(pos.x()), static_cast<float>(pos.y())}, *button, false});
    e->accept();
}

void Viewer::wheelEvent(QWheelEvent* e) {
    const QPointF pos = e->position();
    if (!rect().contains(pos.toPoint())) return;
    ScrollDelta delta = scroll_delta::Lines{static_cast<float>(e->angleDelta().x()) / 120.0F,
                                            static_cast<float>(e->angleDelta().y()) / 120.0F};
    if (!e->pixelDelta().isNull())
        delta = scroll_delta::Pixels{static_cast<float>(e->pixelDelta().x()), static_cast<float>(e->pixelDelta().y())};
    const auto [dx, dy] = KeyMap::scroll_pixels(delta);
    pointer(pointer::Scroll{{static_cast<float>(pos.x()), static_cast<float>(pos.y())}, dx, dy});
    e->accept();
}

void Viewer::key(QKeyEvent* e, bool pressed) {
    const auto modifiers = KeyMap::modifiers(static_cast<int>(e->modifiers()));
    if (modifiers_ != modifiers) {
        modifiers_ = modifiers;
        emit message(msg::Modifiers{modifiers});
    }
    if (!mode_ || !mode_->focused()) {
        e->ignore();
        return;
    }
    const QByteArray text = e->text().toUtf8();
    emit message(msg::Key{KeyMap::from_qt(e->key(), e->nativeScanCode(), static_cast<int>(e->modifiers()),
                                          std::string_view(text.constData(), static_cast<std::size_t>(text.size())), pressed)});
    e->accept();
}

void Viewer::keyPressEvent(QKeyEvent* e) { key(e, true); }
void Viewer::keyReleaseEvent(QKeyEvent* e) { key(e, false); }

void Viewer::focusOutEvent(QFocusEvent* e) {
    QWidget::focusOutEvent(e);
    // A click landed outside the surface.
    if (e->reason() == Qt::MouseFocusReason && mode_ && mode_->focused()) emit message(msg::Blur{});
}

// ---- Document view ---------------------------------------------------------------------------

DocumentView::DocumentView(QWidget* parent) : QScrollArea(parent) {
    setAccessibleName(QStringLiteral("document pages"));
    setWidgetResizable(true);
    setFrameShape(QFrame::NoFrame);
    content_ = new QWidget;
    pages_ = new QVBoxLayout(content_);
    pages_->setAlignment(Qt::AlignHCenter | Qt::AlignTop);
    setWidget(content_);
    connect(verticalScrollBar(), &QScrollBar::valueChanged, this, [this] { scrolled(); });
}

void DocumentView::scrolled() {
    const QSize v = viewport()->size();
    emit message(msg::DocumentScrolled{static_cast<float>(verticalScrollBar()->value()),
                                       Size{static_cast<float>(v.width()), static_cast<float>(v.height())}});
}

void DocumentView::render(const DocumentMode& mode) {
    clear(pages_);
    pages_->setSpacing(static_cast<int>(std::lround(page_gap())));
    const float zoom = mode.zoom();
    const auto zoomed = [zoom](Size s) { return QSize(static_cast<int>(s.width * zoom), static_cast<int>(s.height * zoom)); };
    if (mode.loading()) {
        pages_->addWidget(hint("Loading"));
        return;
    }
    if (const auto* failure = mode.failure()) {
        pages_->addWidget(warning(*failure));
        return;
    }
    if (const auto* image = mode.image()) {
        QImage decoded;
        const auto& data = image->document->data;
        (void)decoded.loadFromData(data.data(), static_cast<int>(data.size()));
        auto* l = new QLabel;
        l->setAccessibleName(QStringLiteral("document image"));
        l->setFixedSize(zoomed(image->size));
        l->setScaledContents(true);
        if (!decoded.isNull()) l->setPixmap(QPixmap::fromImage(decoded));
        pages_->addWidget(l);
        return;
    }
    const auto* pages = mode.pages();
    if (!pages) return;
    for (std::size_t i = 0; i < pages->sizes().size(); ++i) {
        const QSize size = zoomed(pages->sizes()[i]);
        if (const auto* rendered = pages->rendered(i)) {
            const auto& page = rendered->second;
            auto* l = new QLabel;
            l->setAccessibleName(QStringLiteral("document page %1").arg(i + 1));
            l->setFixedSize(size);
            l->setScaledContents(true);
            l->setPixmap(QPixmap::fromImage(rgba_image(page.width, page.height, page.data.data()).copy()));
            pages_->addWidget(l);
        } else {
            auto* well = new QWidget;
            well->setObjectName(QStringLiteral("loa-surface-page-%1").arg(i));
            well->setAccessibleName(QStringLiteral("document page %1").arg(i + 1));
            well->setFixedSize(size);
            kit::qt::style_container(*well, kit::Surface::Well);
            pages_->addWidget(well);
        }
    }
}

// ---- Tab view --------------------------------------------------------------------------------

TabView::TabView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("surface tab"));
    auto* col = new QVBoxLayout(this);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(0);
    auto* header = new QHBoxLayout;
    header->setSpacing(sp(th::SpaceRole::S));
    header->setContentsMargins(sp(th::SpaceRole::M), sp(th::SpaceRole::Xs), sp(th::SpaceRole::M), sp(th::SpaceRole::Xs));
    name_ = body("");
    // Long names (a data: URL without a title) are clipped to one line so the chips keep their width.
    name_->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    notice_ = warning("");
    notice_->setWordWrap(false);
    chips_ = new QHBoxLayout;
    chips_->setSpacing(sp(th::SpaceRole::S));
    header->addWidget(name_, 1);
    header->addWidget(notice_);
    header->addLayout(chips_);
    col->addLayout(header);
    body_ = new QStackedWidget;
    viewer_ = new Viewer;
    document_ = new DocumentView;
    body_->addWidget(viewer_);
    body_->addWidget(document_);
    col->addWidget(body_, 1);
    connect(viewer_, &Viewer::message, this, &TabView::message);
    connect(document_, &DocumentView::message, this, &TabView::message);
}

void TabView::render(const State& state) {
    name_->setText(qs(state.info().name));
    const auto notice = state.notice();
    notice_->setVisible(notice.has_value());
    notice_->setText(notice ? qs(*notice) : QString());
    clear(chips_);
    if (const auto* pixel = state.pixel()) {
        if (auto stream = state.stream()) chips_->addWidget(kit::widgets::badge(stream_label(*stream), Tone::Faint));
        const auto reading = pixel->latency();
        const Tone tone = reading && reading->warning ? Tone::Warn : Tone::Faint;
        chips_->addWidget(kit::widgets::badge("Latency " + latency_label(reading), tone));
        viewer_->render(pixel);
        body_->setCurrentWidget(viewer_);
    } else if (const auto* doc = state.document()) {
        viewer_->render(nullptr);
        const auto zoom = [this](std::string_view label, Message m) {
            auto* b = kit::widgets::text_button(label, ButtonKind::row(false), [this, m] { emit message(m); });
            b->setAccessibleName(qs(label));
            return b;
        };
        chips_->addWidget(zoom("\u2212", msg::ZoomOut{}));
        chips_->addWidget(zoom(std::format("{:.0f}%", doc->zoom() * 100.0F), msg::ZoomReset{}));
        chips_->addWidget(zoom("+", msg::ZoomIn{}));
        document_->render(*doc);
        body_->setCurrentWidget(document_);
    }
}

// ---- Picker view -----------------------------------------------------------------------------

PickerView::PickerView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("file picker"));
    auto* col = new QVBoxLayout(this);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(sp(th::SpaceRole::Xs));
    auto* row = new QHBoxLayout;
    row->setSpacing(sp(th::SpaceRole::Xs));
    up_ = kit::widgets::icon_button(Icon::ChevronUp, "Up", [this] { emit message(picker::msg::Up{}); });
    up_->setAccessibleName(QStringLiteral("Up"));
    path_ = field("Folder or file path");
    connect(path_, &QLineEdit::textEdited, this, [this](const QString& s) { emit message(picker::msg::PathChanged{s.toStdString()}); });
    connect(path_, &QLineEdit::returnPressed, this, [this] { emit message(picker::msg::PathSubmitted{}); });
    row->addWidget(up_);
    row->addWidget(path_, 1);
    col->addLayout(row);
    auto* scroll = new QScrollArea;
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    auto* listed = new QWidget;
    list_ = new QVBoxLayout(listed);
    list_->setContentsMargins(0, 0, 0, 0);
    list_->setSpacing(0);
    list_->setAlignment(Qt::AlignTop);
    scroll->setWidget(listed);
    // About seven rows of entries show before the list scrolls.
    scroll->setFixedHeight(static_cast<int>(7.0F * (th::text_size(th::TextRole::Label) + 3.0F * th::sp(th::SpaceRole::Xs))));
    auto* card = kit::widgets::card(scroll);
    col->addWidget(card);
    error_ = warning("");
    col->addWidget(error_);
}

void PickerView::render(const FilePicker& picker) {
    up_->setEnabled(picker.has_parent());
    set_text(path_, picker.path());
    clear(list_);
    if (picker.entries().empty()) list_->addWidget(hint("No folders or documents here"));
    for (const auto& entry : picker.entries()) {
        picker::Message m = entry.is_dir ? picker::Message{picker::msg::Enter{entry.path}} : picker::Message{picker::msg::Choose{entry.path}};
        list_->addWidget(kit::widgets::menu_row(entry.is_dir ? Icon::Folder : Icon::FileText, entry.name, false,
                                                [this, m] { emit message(m); }));
    }
    error_->setVisible(picker.error().has_value());
    error_->setText(picker.error() ? qs(*picker.error()) : QString());
}

// ---- Panel view ------------------------------------------------------------------------------

PanelView::PanelView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("surfaces"));
    auto* col = new QVBoxLayout(this);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(0);
    auto* strip = new QHBoxLayout;
    strip->setSpacing(sp(th::SpaceRole::Xs));
    strip->setContentsMargins(sp(th::SpaceRole::S), sp(th::SpaceRole::Xs), sp(th::SpaceRole::S), sp(th::SpaceRole::Xs));
    tabs_ = new QHBoxLayout;
    tabs_->setContentsMargins(0, 0, 0, 0);
    strip->addLayout(tabs_, 1);
    launcher_button_ = new QPushButton;
    launcher_button_->setObjectName(QStringLiteral("loa-surface-launcher-button"));
    launcher_button_->setAccessibleName(QStringLiteral("Open a surface"));
    launcher_button_->setIcon(kit::qicon(Icon::Plus, th::TextRole::Body, Tone::Muted));
    connect(launcher_button_, &QPushButton::clicked, this, [this] { emit message(panel::msg::Launcher{}); });
    strip->addWidget(launcher_button_);
    col->addLayout(strip);
    auto* rule = new QFrame;
    rule->setFrameShape(QFrame::HLine);
    rule->setFixedHeight(1);
    col->addWidget(rule);
    body_ = new QStackedWidget;
    tab_ = new TabView;
    connect(tab_, &TabView::message, this, [this](const Message& m) {
        if (shown_tab_) emit message(panel::msg::Surface{*shown_tab_, m});
    });
    launcher_ = new QScrollArea;
    launcher_->setAccessibleName(QStringLiteral("surface launcher"));
    launcher_->setWidgetResizable(true);
    launcher_->setFrameShape(QFrame::NoFrame);
    launcher_->setWidget(build_launcher());
    body_->addWidget(launcher_);
    body_->addWidget(tab_);
    col->addWidget(body_, 1);
}

QWidget* PanelView::build_launcher() {
    namespace msg = loa::ui::surface::panel::msg;
    auto* content = new QWidget;
    auto* col = new QVBoxLayout(content);
    col->setSpacing(sp(th::SpaceRole::L));
    col->setContentsMargins(sp(th::SpaceRole::L), sp(th::SpaceRole::M), sp(th::SpaceRole::L), sp(th::SpaceRole::M));
    status_ = new QWidget;
    new QVBoxLayout(status_);
    status_->layout()->setContentsMargins(0, 0, 0, 0);
    col->addWidget(status_);

    // Browser.
    url_ = field("Search or enter a URL");
    connect(url_, &QLineEdit::textEdited, this, [this](const QString& s) { emit message(msg::Url{s.toStdString()}); });
    connect(url_, &QLineEdit::returnPressed, this, [this] { emit message(msg::OpenUrl{}); });
    open_ = kit::widgets::text_button("Open", ButtonKind::accent(), [this] { emit message(msg::OpenUrl{}); });
    auto* url_row = new QWidget;
    auto* ur = new QHBoxLayout(url_row);
    ur->setContentsMargins(0, 0, 0, 0);
    ur->setSpacing(sp(th::SpaceRole::Xs));
    ur->addWidget(url_, 1);
    ur->addWidget(open_);
    browser_note_ = hint("");
    screencast_ = kit::widgets::text_button("Headless screencast", ButtonKind::row(false),
                                            [this] { emit message(msg::Capture{Capture::Screencast}); });
    virtual_display_ = kit::widgets::text_button("Headed on a virtual display", ButtonKind::row(false),
                                                 [this] { emit message(msg::Capture{Capture::VirtualDisplay}); });
    auto* choices = new QWidget;
    auto* cr = new QHBoxLayout(choices);
    cr->setContentsMargins(0, 0, 0, 0);
    cr->setSpacing(sp(th::SpaceRole::Xs));
    cr->addWidget(hint("Capture"));
    cr->addWidget(screencast_);
    cr->addWidget(virtual_display_);
    cr->addStretch();
    capture_note_ = hint("");
    col->addWidget(section(SurfaceKindStyle::of(BROWSER, loa::view::surface::SurfaceMode::Pixel),
                           column({url_row, browser_note_, choices, capture_note_}, sp(th::SpaceRole::Xs))));

#if defined(LOA_REMOTE_VIDEO)
    // Live video: the on/off toggle and the encoder in use.
    video_on_ = kit::widgets::text_button("On", ButtonKind::row(false), [this] { emit message(msg::Video{true}); });
    video_off_ = kit::widgets::text_button("Off", ButtonKind::row(false), [this] { emit message(msg::Video{false}); });
    auto* toggle = new QWidget;
    auto* tr = new QHBoxLayout(toggle);
    tr->setContentsMargins(0, 0, 0, 0);
    tr->setSpacing(sp(th::SpaceRole::Xs));
    tr->addWidget(hint("Encode browser and VM frames as video for peers"));
    tr->addStretch();
    tr->addWidget(video_on_);
    tr->addWidget(video_off_);
    video_encoder_ = hint("");
    video_hardware_ = hint("");
    col->addWidget(section(SurfaceKindStyle{Icon::Monitor, "Live video"},
                           column({toggle, video_encoder_, hint("Tabs here show the source frames: no encode or decode in the way"),
                                   video_hardware_},
                                  sp(th::SpaceRole::Xs))));
#endif

    // Document.
    picker_ = new PickerView;
    connect(picker_, &PickerView::message, this, [this](const picker::Message& m) { emit message(msg::Picker{m}); });
    col->addWidget(section(SurfaceKindStyle::of(loa::view::surface::DOCUMENT_SERVER, loa::view::surface::SurfaceMode::Document),
                           column({picker_, hint("PDF, XLSX, DOCX, PPTX, PNG, JPEG, GIF, WebP and BMP. Pages render here, so "
                                                 "scrolling and zoom stay local.")},
                                  sp(th::SpaceRole::Xs))));

    // VM.
    address_ = field("host:port, e.g. localhost:5900");
    connect(address_, &QLineEdit::textEdited, this, [this](const QString& s) { emit message(msg::Address{s.toStdString()}); });
    connect(address_, &QLineEdit::returnPressed, this, [this] { emit message(msg::Connect{}); });
    password_ = field("Password (if the server asks)");
    password_->setEchoMode(QLineEdit::Password);
    connect(password_, &QLineEdit::textEdited, this, [this](const QString& s) { emit message(msg::Password{s.toStdString()}); });
    connect(password_, &QLineEdit::returnPressed, this, [this] { emit message(msg::Connect{}); });
    connect_ = kit::widgets::text_button("Connect", ButtonKind::accent(), [this] { emit message(msg::Connect{}); });
    auto* pw_row = new QWidget;
    auto* pr = new QHBoxLayout(pw_row);
    pr->setContentsMargins(0, 0, 0, 0);
    pr->setSpacing(sp(th::SpaceRole::Xs));
    pr->addWidget(password_, 1);
    pr->addWidget(connect_);
    auto* vms = new QWidget;
    vms_ = new QVBoxLayout(vms);
    vms_->setContentsMargins(0, 0, 0, 0);
    vms_->setSpacing(0);
    col->addWidget(section(SurfaceKindStyle::of(VM, loa::view::surface::SurfaceMode::Pixel),
                           column({address_, pw_row, vms}, sp(th::SpaceRole::Xs))));

    // Declared by MCP servers.
    auto* declared = new QWidget;
    declared_ = new QVBoxLayout(declared);
    declared_->setContentsMargins(0, 0, 0, 0);
    declared_->setSpacing(0);
    col->addWidget(section(SurfaceKindStyle::of(MCP, loa::view::surface::SurfaceMode::Pixel), declared));
    col->addStretch();
    return content;
}

void PanelView::render_launcher(const Panel& panel) {
    namespace msg = loa::ui::surface::panel::msg;
    // What is being opened, or why opening failed.
    clear(status_->layout());
    if (const auto& error = panel.error()) {
        auto* box = new QWidget;
        box->setObjectName(QStringLiteral("loa-surface-error"));
        auto* r = new QHBoxLayout(box);
        r->setSpacing(sp(th::SpaceRole::S));
        r->addWidget(icon_label(Icon::TriangleAlert, th::TextRole::Body, Tone::Danger));
        auto* text = kit::widgets::label(*error, th::TextRole::Label, Tone::Danger);
        text->setWordWrap(true);
        r->addWidget(text, 1);
        kit::qt::style_container(*box, kit::Surface::ErrorBox);
        status_->layout()->addWidget(box);
    } else if (const auto& pending = panel.pending()) {
        status_->layout()->addWidget(hint(*pending + "\u2026"));
    }
    status_->setVisible(panel.error() || panel.pending());

    const auto* ready = std::get_if<loa::view::surface::browser_status::Ready>(&panel.browser());
    set_text(url_, panel.url());
    open_->setEnabled(ready && panel.url().find_first_not_of(" \t\r\n") != std::string::npos);
    const auto& capture = panel.capture();
    if (ready) {
        const std::string_view sandbox = ready->sandboxed ? "" : ", without Chrome's sandbox (this kernel restricts user namespaces)";
        const std::string_view kind = capture.selected == Capture::VirtualDisplay ? "Headed" : "Headless";
        browser_note_->setText(qs(std::format("{} {}{}", kind, browser_name(ready->executable), sandbox)));
        browser_note_->setStyleSheet(qs("color: " + kit::qt::css(kit::color(Tone::Muted))));
    } else {
        browser_note_->setText(QStringLiteral("No Chrome or Chromium installed. Install one, or run `npx playwright install chromium`."));
        browser_note_->setStyleSheet(qs("color: " + kit::qt::css(kit::color(Tone::Warn))));
    }
    const auto& missing = capture.virtual_display_missing;
    kit::qt::style_button(*screencast_, ButtonKind::row(capture.selected == Capture::Screencast));
    kit::qt::style_button(*virtual_display_, ButtonKind::row(capture.selected == Capture::VirtualDisplay));
    virtual_display_->setEnabled(!missing);
    std::optional<std::string> note;
    if (missing) {
        note = "Virtual display unavailable: " + *missing;
    } else if (capture.pending) {
        note = "The open browser tabs keep the current capture; new tabs use this one once they close.";
    } else if (capture.selected == Capture::VirtualDisplay) {
        note = "Each tab runs headed Chrome on its own Xvfb display, captured raw.";
    }
    capture_note_->setVisible(note.has_value());
    capture_note_->setText(note ? qs(*note) : QString());

#if defined(LOA_REMOTE_VIDEO)
    const auto& video = panel.video();
    kit::qt::style_button(*video_on_, ButtonKind::row(video.enabled));
    kit::qt::style_button(*video_off_, ButtonKind::row(!video.enabled));
    std::string encoder;
    if (!video.enabled) {
        encoder = "Off: peers get frames as their source makes them (raw pixels, JPEG screencast)";
    } else if (!video.encoder) {
        encoder = "Trying this machine's encoders\u2026";
    } else if (video.encoder->hardware) {
        encoder = std::format("{} with {}, hardware encoded", encoding_name(video.encoder->encoding), video.encoder->encoder);
    } else {
        encoder = std::format("No hardware encoder works here: {} with {}, software encoded", encoding_name(video.encoder->encoding),
                              video.encoder->encoder);
    }
    video_encoder_->setText(qs(encoder));
    std::string hardware;
    for (const auto& h : video.hardware) hardware += (hardware.empty() ? "" : ", ") + h;
    video_hardware_->setVisible(video.hardware.size() > 1);
    video_hardware_->setText(qs("Hardware encoders: " + hardware));
#endif

    picker_->render(panel.picker());
    set_text(address_, panel.address());
    set_text(password_, panel.password());
    connect_->setEnabled(panel.address().find_first_not_of(" \t\r\n") != std::string::npos);

    const auto rows = [this](QVBoxLayout* into, const std::vector<const AvailableSurface*>& available) {
        clear(into);
        for (const auto* a : available) {
            const auto& info = a->info;
            const std::string size = info.width > 0 ? std::format("{}\u00d7{}", info.width, info.height)
                                                    : std::string(loa::core::as_str(info.mode));
            auto* row = new QPushButton;
            row->setObjectName(QStringLiteral("loa-surface-available"));
            row->setAccessibleName(qs(info.name));
            kit::qt::style_button(*row, ButtonKind::row(false));
            auto* r = new QHBoxLayout(row);
            r->setSpacing(sp(th::SpaceRole::S));
            r->addWidget(body(info.name));
            r->addWidget(kit::widgets::badge(a->server, Tone::Faint));
            r->addStretch();
            r->addWidget(hint(size));
            row->setMinimumHeight(r->sizeHint().height());
            connect(row, &QPushButton::clicked, this, [this, uri = info.uri] { emit message(msg::OpenAvailable{uri}); });
            into->addWidget(row);
        }
    };
    std::vector<const AvailableSurface*> vms;
    std::vector<const AvailableSurface*> declared;
    for (const auto& a : panel.available()) (a.server == VM ? vms : declared).push_back(&a);
    rows(vms_, vms);
    rows(declared_, declared);
    if (declared.empty()) declared_->addWidget(hint("No MCP server declares a surface. Surface servers added in Tools show up here."));
}

void PanelView::render(const Panel& panel) {
    // One tab per open surface, then the launcher button.
    clear(tabs_);
    std::vector<kit::widgets::Tab> tabs;
    tabs.reserve(panel.tabs().size());
    for (const auto& t : panel.tabs()) {
        const auto& info = t.state.info();
        auto* close = kit::widgets::icon_button(Icon::X, "Close", [this, uri = info.uri] { emit message(surface::panel::msg::Close{uri}); });
        close->setAccessibleName(QStringLiteral("Close %1").arg(qs(info.name)));
        tabs.push_back(kit::widgets::Tab{t.label, SurfaceKindStyle::of(t.server, info.mode).icon,
                                         panel.active() && *panel.active() == info.uri,
                                         [this, uri = info.uri] { emit message(surface::panel::msg::Select{uri}); }, close});
    }
    tabs_->addWidget(kit::widgets::tab_strip(tabs), 1);
    kit::qt::style_button(*launcher_button_, ButtonKind::tab(panel.active() == nullptr));

    const State* state = panel.active() ? panel.surface(*panel.active()) : nullptr;
    if (state) {
        shown_tab_ = state->info().uri;
        tab_->render(*state);
        body_->setCurrentWidget(tab_);
    } else {
        shown_tab_.reset();
        render_launcher(panel);
        body_->setCurrentWidget(launcher_);
    }
}

} // namespace loa::ui::surface
