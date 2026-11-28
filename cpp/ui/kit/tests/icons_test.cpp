// Every icon is a compiled-in resource and renders in exactly its tint.

#include <doctest/doctest.h>

#include <set>

#include <QFile>
#include <QImage>
#include <QPixmap>

#include "fixture.hpp"
#include "loa/ui/kit/icons.hpp"

using namespace loa::ui::kit;

TEST_CASE("the icon table lists every icon in discriminant order with unique resources") {
    std::set<std::string_view> files;
    for (std::size_t i = 0; i < ICON_COUNT; ++i) {
        CHECK(static_cast<std::size_t>(all_icons()[i]) == i);
        files.insert(icon_file(all_icons()[i]));
    }
    CHECK(files.size() == 37);
    CHECK(icon_file(Icon::Settings) == "settings-2");
    CHECK(icon_resource(Icon::X) == ":/loa/icons/x.svg");
}

TEST_CASE("every icon renders with the tone tint") {
    qt_app();
    const auto tint = theme::Color::rgb8(0x7f, 0xb7, 0xa4);
    for (const Icon i : all_icons()) {
        INFO(icon_file(i));
        CHECK(QFile::exists(QString::fromUtf8(icon_resource(i).data(), static_cast<qsizetype>(icon_resource(i).size()))));
        const QImage img = render_icon(i, 24, tint).convertToFormat(QImage::Format_ARGB32);
        int opaque = 0;
        bool exact = true;
        for (int y = 0; y < img.height(); ++y)
            for (int x = 0; x < img.width(); ++x) {
                const QRgb px = img.pixel(x, y);
                if (qAlpha(px) < 255) continue;
                ++opaque;
                exact = exact && qRed(px) == 0x7f && qGreen(px) == 0xb7 && qBlue(px) == 0xa4;
            }
        CHECK(opaque > 0);
        CHECK(exact);
    }
    const QPixmap px = tinted(Icon::Check, theme::TextRole::Body, Tone::Accent);
    CHECK(px.width() == static_cast<int>(theme::text_size(theme::TextRole::Body)));
}
