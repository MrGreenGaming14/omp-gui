#include <doctest/doctest.h>

#include <cmath>
#include <string>
#include <tuple>
#include <vector>

#include "loa/surfaces/docs/pptx.hpp"
#include "ooxml_zip.hpp"

namespace docs = loa::surfaces::docs;
using docs::Format;
using docs::ShapeKind;

namespace {

const std::string NS =
    R"(xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main")";

std::string rels(const std::vector<std::tuple<std::string, std::string, std::string>>& entries) {
    const std::string rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    std::string body;
    for (const auto& [id, kind, target] : entries)
        body += R"(<Relationship Id=")" + id + R"(" Type=")" + rel + "/" + kind + R"(" Target=")" + target + R"("/>)";
    return R"(<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">)" + body + "</Relationships>";
}

std::string tree(const std::string& root, const std::string& shapes) {
    return R"(<?xml version="1.0" encoding="UTF-8"?><p:)" + root + " " + NS +
           R"(><p:cSld><p:spTree><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>)" +
           shapes + "</p:spTree></p:cSld></p:" + root + ">";
}

// Two slides listed in the reverse of their file names, the first with a title,
// a body inheriting its geometry from the layout, a grouped picture and notes.
std::vector<std::uint8_t> sample() {
    const std::string presentation =
        "<p:presentation " + NS +
        R"(><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>)";
    const std::string first = tree(
        "sld",
        R"(<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="457200" y="274320"/><a:ext cx="10972800" cy="1143000"/></a:xfrm></p:spPr>)"
        R"(<p:txBody><a:p><a:r><a:rPr sz="4000" b="1"/><a:t>Quarterly results &amp; plans</a:t></a:r></a:p></p:txBody></p:sp>)"
        R"(<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content 2"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/>)"
        R"(<p:txBody><a:p><a:r><a:t>Revenue up</a:t></a:r></a:p><a:p><a:r><a:t>Costs</a:t></a:r><a:br/><a:r><a:t>down</a:t></a:r></a:p><a:p><a:endParaRPr/></a:p></p:txBody></p:sp>)"
        R"(<p:grpSp><p:nvGrpSpPr><p:cNvPr id="4" name="Group 3"/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="1270000" y="4445000"/><a:ext cx="2540000" cy="2540000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr>)"
        R"(<p:pic><p:nvPicPr><p:cNvPr id="5" name="Picture 4"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="0"/><a:ext cx="635000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:grpSp>)");
    const std::string second = tree(
        "sld",
        R"(<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12192000" cy="1270000"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp>)");
    const std::string layout = tree(
        "sldLayout",
        R"(<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2"/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="838200" y="1825625"/><a:ext cx="10515600" cy="4351338"/></a:xfrm></p:spPr></p:sp>)");
    const std::string notes = tree(
        "notes",
        R"(<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image"/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Speaker notes here</a:t></a:r></a:p></p:txBody></p:sp>)");
    auto png = loa::surfaces::RgbaImage::filled(4, 4, {255, 0, 0, 255}).to_png();
    REQUIRE(png);
    return loa::surfaces::testing::zip_parts({
        {"ppt/presentation.xml", presentation},
        {"ppt/_rels/presentation.xml.rels", rels({{"rId2", "slide", "slides/slide1.xml"}, {"rId3", "slide", "/ppt/slides/slide2.xml"}})},
        {"ppt/slides/slide1.xml", second},
        {"ppt/slides/slide2.xml", first},
        {"ppt/slides/_rels/slide2.xml.rels",
         rels({{"rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"},
               {"rId2", "image", "../media/image1.png"},
               {"rId3", "notesSlide", "../notesSlides/notesSlide1.xml"}})},
        {"ppt/slideLayouts/slideLayout1.xml", layout},
        {"ppt/notesSlides/notesSlide1.xml", notes},
        {"ppt/media/image1.png", std::string(png->begin(), png->end())},
    });
}

} // namespace

TEST_CASE("pptx: a presentation round trips to slides, shapes, text and pixels") {
    auto parsed = docs::PptxDoc::parse(sample());
    REQUIRE(parsed);
    const auto& doc = **parsed;
    CHECK(doc.size() == docs::PageSize{960.0F, 540.0F});
    CHECK(doc.page_count() == 2);
    CHECK(doc.text() ==
          "Slide 1\nQuarterly results & plans\nRevenue up\nCosts\ndown\nNotes: Speaker notes here\n\nSlide 2\nSecond\n");
    const auto& shapes = doc.slides()[0].shapes;
    using Geometry = std::tuple<std::string, ShapeKind, float, float, float, float>;
    std::vector<Geometry> geometry;
    for (const auto& s : shapes)
        geometry.emplace_back(s.name, s.kind, std::round(s.x), std::round(s.y), std::round(s.width), std::round(s.height));
    const std::vector<Geometry> expected{
        {"Title 1", ShapeKind::Text, 36.0F, 22.0F, 864.0F, 90.0F},
        {"Content 2", ShapeKind::Text, 66.0F, 144.0F, 828.0F, 343.0F},
        {"Picture 4", ShapeKind::Picture, 120.0F, 350.0F, 100.0F, 100.0F},
    };
    CHECK(geometry == expected);
    CHECK(shapes[0].styles[0] == docs::ParaStyle{40.0F, true});
    CHECK(shapes[2].image == std::optional<std::string>("ppt/media/image1.png"));

    auto page = doc.render_page(0, 0.5F, docs::Fonts::none());
    REQUIRE(page);
    CHECK(page->width == 480);
    CHECK(page->height == 270);
    CHECK(page->pixel(85, 200) == std::optional<loa::surfaces::Pixel>({255, 0, 0, 255}));
    CHECK(page->pixel(50, 200) == std::optional<loa::surfaces::Pixel>({255, 255, 255, 255}));
    const auto dark = [&](std::uint32_t y0, std::uint32_t y1) {
        for (std::uint32_t y = y0; y < y1; ++y)
            for (std::uint32_t x = 0; x < 480; ++x)
                if (auto p = page->pixel(x, y); p && (*p)[0] < 128) return true;
        return false;
    };
    CHECK(dark(10, 56));
    CHECK(dark(72, 160));
    CHECK_FALSE(dark(57, 70));

    auto out = doc.render_page(2, 1.0F, docs::Fonts::none());
    REQUIRE_FALSE(out);
    CHECK(out.error().kind() == docs::DocsError::Kind::PageRange);
    CHECK(out.error().index() == 2);
    CHECK(out.error().count() == 2);

    auto empty = docs::PptxDoc::parse(loa::surfaces::testing::zip_parts({}));
    REQUIRE_FALSE(empty);
    CHECK(empty.error().kind() == docs::DocsError::Kind::Invalid);
    CHECK(empty.error().format() == std::optional<Format>(Format::Pptx));
}

TEST_CASE("pptx: queries answer slides and text, others are unsupported") {
    auto parsed = docs::PptxDoc::parse(sample());
    REQUIRE(parsed);
    const auto& doc = **parsed;
    auto text = doc.query({docs::query::Text{std::nullopt}});
    REQUIRE(text);
    REQUIRE(*text);
    CHECK(**text == loa::runtime::json::Value(doc.text()));
    auto slide = doc.query({docs::query::Text{1}});
    REQUIRE(slide);
    REQUIRE(*slide);
    CHECK(**slide == doc.slides()[1].to_json());
    auto out = doc.query({docs::query::Text{5}});
    REQUIRE(out);
    CHECK_FALSE(*out);
    auto slides = doc.query({docs::query::Slides{}});
    REQUIRE(slides);
    REQUIRE(*slides);
    CHECK(!doc.query({docs::query::Outline{}}));
    CHECK(loa::runtime::json::dump(doc.slides()[0].shapes[2].to_json()) ==
          R"({"name":"Picture 4","kind":"picture","placeholder":null,"x":120.0,"y":350.0,"width":100.0,"height":100.0,"paragraphs":[],"image":"ppt/media/image1.png"})");
}
