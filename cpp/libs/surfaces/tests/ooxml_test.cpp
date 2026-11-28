// The OOXML package reader, and the Office fixtures in `cpp/tests/golden/docs` against what the Rust parsers
// answered about them (`cpp/libs/surfaces/tests/docs-golden`).

#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <iterator>
#include <sstream>

#include "loa/runtime/json.hpp"
#include "loa/surfaces/docs/docx.hpp"
#include "loa/surfaces/docs/format.hpp"
#include "loa/surfaces/docs/ooxml.hpp"
#include "loa/surfaces/docs/pptx.hpp"
#include "loa/surfaces/docs/server.hpp"
#include "loa/surfaces/docs/xlsx.hpp"
#include "ooxml_zip.hpp"

namespace docs = loa::surfaces::docs;
namespace ooxml = loa::surfaces::docs::ooxml;
namespace json = loa::runtime::json;
using docs::Format;
using loa::surfaces::testing::zip_parts;

namespace {

const std::filesystem::path GOLDEN = std::filesystem::path(LOA_GOLDEN_DIR) / "docs";

std::string read_text(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    REQUIRE(in);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

std::vector<std::uint8_t> read_bytes(const std::filesystem::path& path) {
    const std::string s = read_text(path);
    return {s.begin(), s.end()};
}

std::vector<std::string> lines(const std::string& text) {
    std::vector<std::string> out;
    std::istringstream in(text);
    for (std::string line; std::getline(in, line);) out.push_back(line);
    return out;
}

std::string answer(const docs::Paged& doc, docs::DataQuery query) {
    auto value = doc.query(query);
    REQUIRE(value);
    REQUIRE(*value);
    return json::dump(**value);
}

std::unique_ptr<docs::Paged> open_golden(Format format, const char* file) {
    const auto bytes = read_bytes(GOLDEN / file);
    auto doc = docs::open_pages(format, bytes);
    REQUIRE(doc);
    return std::move(*doc);
}

} // namespace

TEST_CASE("ooxml resolves relationship targets against their source part") {
    CHECK(ooxml::resolve_target("ppt/slides/slide1.xml", "../media/image1.png") == "ppt/media/image1.png");
    CHECK(ooxml::resolve_target("ppt/presentation.xml", "slides/./slide2.xml") == "ppt/slides/slide2.xml");
    CHECK(ooxml::resolve_target("ppt/presentation.xml", "/ppt/slides/slide2.xml") == "ppt/slides/slide2.xml");
    CHECK(ooxml::resolve_target("word.xml", "../../a//b") == "a/b");
    CHECK(ooxml::resolve_target("ppt/slides/slide1.xml", "_rels/slide1.xml.rels") == "ppt/slides/_rels/slide1.xml.rels");
}

TEST_CASE("ooxml reads parts, relationships and attributes") {
    const std::string rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const auto bytes = zip_parts({
        {"ppt/slides/slide1.xml", "\xEF\xBB\xBF<p:sld xmlns:p=\"p\" xmlns:r=\"r\"><p:x id=\" 12 \" r:id=\"rId1\" bad=\"1.5\"/></p:sld>"},
        {"ppt/slides/_rels/slide1.xml.rels",
         "<Relationships><Relationship Id=\"rId1\" Type=\"" + rel + "/image\" Target=\"../media/a.png\"/>"
         "<Relationship Id=\"rId2\" Type=\"" + rel + "/hyperlink\" Target=\"https://x\" TargetMode=\"External\"/></Relationships>"},
        {"ppt/broken.xml", "<a><b></a>"},
        {"ppt/latin1.xml", "<a>\xE9</a>"},
        {"ppt/norels.xml", "<a/>"},
        {"ppt/_rels/badrels.xml.rels", "<Relationships><Relationship Id=\"x\"/></Relationships>"},
    });
    auto pkg = ooxml::Package::open(bytes, Format::Pptx);
    REQUIRE(pkg);

    auto missing = pkg->read("nope.xml");
    REQUIRE(missing);
    CHECK_FALSE(*missing);

    auto rels = pkg->rels("ppt/slides/slide1.xml");
    REQUIRE(rels);
    REQUIRE(rels->size() == 1);
    CHECK((*rels)[0].id == "rId1");
    CHECK((*rels)[0].kind == rel + "/image");
    CHECK((*rels)[0].target == "ppt/media/a.png");
    auto none = pkg->rels("ppt/norels.xml");
    REQUIRE(none);
    CHECK(none->empty());
    auto bad_rels = pkg->rels("ppt/badrels.xml");
    REQUIRE_FALSE(bad_rels);
    CHECK(bad_rels.error().message() ==
          "invalid PPTX file: ppt/_rels/badrels.xml.rels: Relationship lacks Id, Type or Target");

    auto attrs = pkg->parsed<std::string>("ppt/slides/slide1.xml", [](std::string_view xml) -> ooxml::XmlResult<std::string> {
        std::string out;
        auto walked = ooxml::each_element(xml, [&](const pugi::xml_node& e) -> ooxml::XmlResult<void> {
            if (ooxml::local_name(e) != "x") return {};
            const auto rid = ooxml::find_attr(e, [](const pugi::xml_attribute& a) {
                return !ooxml::prefix(a).empty() && ooxml::local_name(a) == "id";
            });
            auto id = ooxml::num_attr(e, "id");
            if (!id) return std::unexpected(id.error());
            out = std::string(rid.value_or("")) + " " + std::to_string(static_cast<int>(id->value_or(0)));
            auto bad = ooxml::num_attr(e, "bad");
            if (bad) return std::unexpected(std::string("1.5 parsed"));
            out += " | " + bad.error();
            CHECK_FALSE(ooxml::num_attr(e, "absent")->has_value());
            return {};
        });
        if (!walked) return std::unexpected(walked.error());
        return out;
    });
    REQUIRE(attrs);
    REQUIRE(*attrs);
    CHECK(**attrs == "rId1 12 | attribute bad=\"1.5\" is not a number");

    auto broken = pkg->parsed<int>("ppt/broken.xml", [](std::string_view xml) -> ooxml::XmlResult<int> {
        auto doc = ooxml::xml_reader(xml);
        if (!doc) return std::unexpected(doc.error());
        return 0;
    });
    REQUIRE_FALSE(broken);
    CHECK(broken.error().kind() == docs::DocsError::Kind::Invalid);
    CHECK(broken.error().message().starts_with("invalid PPTX file: ppt/broken.xml: "));
    auto latin1 = pkg->parsed<int>("ppt/latin1.xml", [](std::string_view) -> ooxml::XmlResult<int> { return 0; });
    REQUIRE_FALSE(latin1);
    CHECK(latin1.error().message() == "invalid PPTX file: ppt/latin1.xml is not UTF-8 XML");
}

TEST_CASE("broken and non-zip Office input is an invalid file error") {
    const std::vector<std::vector<std::uint8_t>> inputs = {
        {},
        {'n', 'o', 't', ' ', 'a', ' ', 'z', 'i', 'p'},
        {'P', 'K', 3, 4, ' ', 'n', 'o', 't'},
        zip_parts({}),
        zip_parts({{"word/document.xml", "<w:document><w:body><w:p>"}, {"xl/workbook.xml", "<workbook"},
                   {"ppt/presentation.xml", "<p:presentation><p:sldIdLst><p:sldId id=\"1\"/></p:sldIdLst></p:presentation>"}}),
    };
    for (const Format format : {Format::Docx, Format::Xlsx, Format::Pptx}) {
        for (const auto& bytes : inputs) {
            auto doc = docs::open_pages(format, bytes);
            REQUIRE_FALSE(doc);
            CHECK(doc.error().kind() == docs::DocsError::Kind::Invalid);
            CHECK(doc.error().format() == format);
        }
        // Truncated copies of a valid file never crash.
        const char* file = format == Format::Docx ? "sample.docx" : format == Format::Xlsx ? "sample.xlsx" : "sample.pptx";
        const auto good = read_bytes(GOLDEN / file);
        for (std::size_t cut = 0; cut < good.size(); cut += 97) {
            const std::vector<std::uint8_t> part(good.begin(), good.begin() + static_cast<std::ptrdiff_t>(cut));
            auto doc = docs::open_pages(format, part);
            if (!doc) CHECK(doc.error().kind() == docs::DocsError::Kind::Invalid);
        }
    }
}

TEST_CASE("the DOCX golden parses to the Rust outline, text and pages") {
    auto doc = open_golden(Format::Docx, "sample.docx");
    CHECK(answer(*doc, docs::DataQuery{docs::query::Outline{}}) == read_text(GOLDEN / "docx.outline.json"));
    CHECK(answer(*doc, docs::DataQuery{docs::query::Text{}}) == read_text(GOLDEN / "docx.text.json"));
    docs::RenderOpts big;
    big.typography.body = 24.0F;
    const auto size = doc->page_size(0);
    REQUIRE(size);
    std::ostringstream pages;
    pages << doc->page_count() << ' ' << doc->page_count_with_opts(big) << ' ' << size->width << ' ' << size->height << '\n';
    CHECK(pages.str() == read_text(GOLDEN / "docx.pages.txt"));
}

TEST_CASE("the XLSX golden parses to the Rust sheets, cells and sizes") {
    auto doc = open_golden(Format::Xlsx, "sample.xlsx");
    CHECK(answer(*doc, docs::DataQuery{docs::query::Sheets{}}) == read_text(GOLDEN / "xlsx.sheets.json"));
    const std::vector<docs::query::Cells> queries = {
        {std::nullopt, "A1:C6"}, {"offset", "$E$4"}, {"Offset", "A1:Z1000"}, {"Empty", "B2:C3"}, {"Data", "c2"}};
    const auto want = lines(read_text(GOLDEN / "xlsx.cells.jsonl"));
    REQUIRE(want.size() == queries.size());
    for (std::size_t i = 0; i < queries.size(); ++i) {
        CAPTURE(i);
        CHECK(answer(*doc, docs::DataQuery{queries[i]}) == want[i]);
    }
    std::ostringstream sizes;
    for (std::size_t i = 0; i < doc->page_count(); ++i) {
        const auto s = doc->page_size(i);
        REQUIRE(s);
        sizes << i << ' ' << s->width << ' ' << s->height << '\n';
    }
    CHECK(sizes.str() == read_text(GOLDEN / "xlsx.sizes.txt"));
}

TEST_CASE("the PPTX golden parses to the Rust slides and text") {
    auto doc = open_golden(Format::Pptx, "sample.pptx");
    CHECK(answer(*doc, docs::DataQuery{docs::query::Slides{}}) == read_text(GOLDEN / "pptx.slides.json"));
    CHECK(answer(*doc, docs::DataQuery{docs::query::Text{}}) == read_text(GOLDEN / "pptx.text.json"));
}

TEST_CASE("Office surfaces refuse queries with the Rust error text") {
    docs::DocsServer server(docs::Fonts::none());
    std::string got;
    for (const char* file : {"sample.docx", "sample.xlsx", "sample.pptx"}) {
        auto surface = server.open_file_blocking(GOLDEN / file);
        REQUIRE(surface);
        const std::vector<docs::DataQuery> all = {
            {docs::query::Text{}},
            {docs::query::Text{0}},
            {docs::query::Text{5}},
            {docs::query::Sheets{}},
            {docs::query::Cells{std::nullopt, "A1"}},
            {docs::query::Cells{"Missing", "A1"}},
            {docs::query::Cells{std::nullopt, "B"}},
            {docs::query::Cells{std::nullopt, "XFE1"}},
            {docs::query::Cells{std::nullopt, "A1:XFD1048576"}},
            {docs::query::Outline{}},
            {docs::query::Slides{}},
        };
        for (const auto& q : all) {
            auto out = (*surface)->answer(q);
            if (!out) got += std::string(file) + "\t" + json::dump(q.to_json()) + "\t" + out.error().message() + "\n";
        }
    }
    CHECK(got == read_text(GOLDEN / "office.errors.tsv"));
}
