#include <doctest/doctest.h>

#include <fstream>
#include <iterator>
#include <string>
#include <tuple>
#include <vector>

#include <xlsxwriter.h>

#include "loa/runtime/tempfile.hpp"
#include "loa/surfaces/docs/xlsx.hpp"
#include "ooxml_zip.hpp"

namespace docs = loa::surfaces::docs;
namespace json = loa::runtime::json;
using docs::Cell;

namespace {

std::vector<std::uint8_t> read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

// `Data` from A1 with text, numbers, a bool, a formula and a date; `Offset` starting at D3.
std::vector<std::uint8_t> sample_workbook() {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto path = dir->path() / "book.xlsx";
    lxw_workbook* book = workbook_new(path.c_str());
    REQUIRE(book != nullptr);
    lxw_worksheet* data = workbook_add_worksheet(book, "Data");
    worksheet_write_string(data, 0, 0, "Name", nullptr);
    worksheet_write_string(data, 1, 0, "apple", nullptr);
    worksheet_write_number(data, 1, 1, 3.0, nullptr);
    worksheet_write_boolean(data, 2, 0, 1, nullptr);
    worksheet_write_formula_num(data, 2, 1, "=SUM(B2:B2)", nullptr, 3.0);
    lxw_format* date = workbook_add_format(book);
    format_set_num_format(date, "yyyy-mm-dd");
    worksheet_write_number(data, 3, 0, 45366.0, date);
    lxw_worksheet* offset = workbook_add_worksheet(book, "Offset");
    worksheet_write_string(offset, 2, 3, "origin", nullptr);
    worksheet_write_number(offset, 3, 4, 42.0, nullptr);
    REQUIRE(workbook_close(book) == LXW_NO_ERROR);
    return read_file(path);
}

constexpr const char* CONTENT_TYPES =
    R"(<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>)";
constexpr const char* ROOT_RELS =
    R"(<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>)";
constexpr const char* WORKBOOK_RELS =
    R"(<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet" Target="chartsheets/sheet1.xml"/></Relationships>)";

// A one-sheet workbook (plus a chart sheet) around `sheet_data`.
std::vector<std::uint8_t> hand_workbook(const std::string& sheet_data, bool date1904 = false) {
    const std::string workbook =
        std::string(R"(<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><workbookPr)") +
        (date1904 ? R"( date1904="1")" : "") +
        R"(/><sheets><sheet name="Hand" sheetId="1" r:id="rId1"/><sheet name="Chart" sheetId="2" r:id="rId2"/></sheets></workbook>)";
    const std::string styles =
        R"(<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="2"><numFmt numFmtId="164" formatCode="[h]:mm:ss"/><numFmt numFmtId="165" formatCode="&quot;d&quot;0.00"/></numFmts><cellXfs count="5"><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="165"/><xf numFmtId="22"/><xf numFmtId="46"/></cellXfs></styleSheet>)";
    const std::string strings =
        R"(<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t xml:space="preserve"> plain </t></si><si><r><t>rich</t></r><r><t xml:space="preserve"> text</t></r><rPh><t>phonetic</t></rPh></si><si><t>a_x000D_b</t></si></sst>)";
    const std::string sheet =
        R"(<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:C6"/><sheetData>)" +
        sheet_data + "</sheetData></worksheet>";
    return loa::surfaces::testing::zip_parts({{"[Content_Types].xml", CONTENT_TYPES},
                                              {"_rels/.rels", ROOT_RELS},
                                              {"xl/workbook.xml", workbook},
                                              {"xl/_rels/workbook.xml.rels", WORKBOOK_RELS},
                                              {"xl/styles.xml", styles},
                                              {"xl/sharedStrings.xml", strings},
                                              {"xl/worksheets/sheet1.xml", sheet},
                                              {"xl/chartsheets/sheet1.xml", "<chartsheet/>"}});
}

} // namespace

TEST_CASE("xlsx: a workbook round trips to sheets, cells and a grid") {
    const auto bytes = sample_workbook();
    auto parsed = docs::XlsxDoc::parse(bytes);
    REQUIRE_MESSAGE(parsed, (parsed ? "" : parsed.error().message()));
    const auto& doc = **parsed;

    const auto info = [](const char* name, std::uint32_t rows, std::uint32_t columns, const char* start) {
        return docs::SheetInfo{name, rows, columns, std::string(start)};
    };
    CHECK(doc.sheets() == std::vector{info("Data", 4, 2, "A1"), info("Offset", 2, 2, "D3")});
    auto block = doc.cells(std::nullopt, "A1:B4");
    REQUIRE(block);
    using Row = std::tuple<std::string, Cell, std::optional<std::string>>;
    std::vector<Row> values;
    for (const auto& row : block->rows)
        for (const auto& c : row) values.emplace_back(c.address, c.value, c.formula);
    const Cell date{Cell::DateTime{45366.0, "2024-03-15T00:00:00"}};
    const std::vector<Row> expected{
        {"A1", Cell{Cell::String{"Name"}}, std::nullopt}, {"B1", Cell{Cell::Empty{}}, std::nullopt},
        {"A2", Cell{Cell::String{"apple"}}, std::nullopt}, {"B2", Cell{Cell::Float{3.0}}, std::nullopt},
        {"A3", Cell{Cell::Bool{true}}, std::nullopt},     {"B3", Cell{Cell::Float{3.0}}, "SUM(B2:B2)"},
        {"A4", date, std::nullopt},                       {"B4", Cell{Cell::Empty{}}, std::nullopt},
    };
    CHECK(values == expected);

    // Ranges are absolute even when the data starts elsewhere, and clipped to the used area.
    CHECK(doc.cells("offset", "$E$4")->rows[0][0].value == Cell{Cell::Float{42.0}});
    auto wide = doc.cells("Offset", "A1:Z1000");
    REQUIRE(wide);
    CHECK(wide->range == "A1:E4");
    CHECK(wide->rows[2][3].value == Cell{Cell::String{"origin"}});
    const std::vector<std::pair<std::optional<std::string_view>, std::string_view>> bad{
        {"Missing", "A1"}, {std::nullopt, "B"}, {std::nullopt, "12"}, {std::nullopt, "A0"}, {std::nullopt, "XFE1"}};
    for (const auto& [sheet, range] : bad) {
        auto r = doc.cells(sheet, range);
        REQUIRE_FALSE(r);
        CHECK(r.error().kind() == docs::DocsError::Kind::Request);
    }
    CHECK(doc.cells("Missing", "A1").error().message() == "no sheet named `Missing`, the sheets are: Data, Offset");
    CHECK(doc.cells(std::nullopt, "B").error().message() == "`B` is not a cell in A1 notation such as B3");
    CHECK(docs::xlsx::column_name(0) == "A");
    CHECK(docs::xlsx::column_name(25) == "Z");
    CHECK(docs::xlsx::column_name(26) == "AA");
    CHECK(docs::xlsx::column_name(701) == "ZZ");
    CHECK(docs::xlsx::column_name(702) == "AAA");

    const docs::PageSize size{docs::xlsx::ROW_HEADER_WIDTH + 5.0F * docs::xlsx::COLUMN_WIDTH, 5.0F * docs::xlsx::ROW_HEIGHT};
    CHECK(*doc.page_size(1) == size);
    auto image = doc.render_page(1, 1.0F, docs::Fonts::none());
    REQUIRE(image);
    const auto dark = [&](std::uint32_t x, std::uint32_t y) {
        for (std::uint32_t py = y; py < y + 18; ++py)
            for (std::uint32_t px = x; px < x + 62; ++px)
                if (auto p = image->pixel(px, py); p && (*p)[0] < 128) return true;
        return false;
    };
    // Inside cell A1, empty, and D3, which holds `origin`.
    CHECK_FALSE(dark(41, 21));
    CHECK(dark(233, 61));
    CHECK(image->pixel(static_cast<std::uint32_t>(docs::xlsx::ROW_HEADER_WIDTH) + 3, 3) != loa::surfaces::Pixel{255, 255, 255, 255});
    auto out = doc.render_page(2, 1.0F, docs::Fonts::none());
    REQUIRE_FALSE(out);
    CHECK(out.error().kind() == docs::DocsError::Kind::PageRange);
    CHECK(out.error().index() == 2);
    CHECK(out.error().count() == 2);

    const std::string junk = "PK\x03\x04 not a workbook";
    auto broken = docs::XlsxDoc::parse(std::span(reinterpret_cast<const std::uint8_t*>(junk.data()), junk.size()));
    REQUIRE_FALSE(broken);
    CHECK(broken.error().kind() == docs::DocsError::Kind::Invalid);
    CHECK(broken.error().format() == docs::Format::Xlsx);
}

TEST_CASE("xlsx: queries answer sheets and cells as the serde JSON") {
    const auto bytes = sample_workbook();
    auto doc = docs::XlsxDoc::parse(bytes);
    REQUIRE(doc);
    auto sheets = (*doc)->query(docs::DataQuery{docs::query::Sheets{}});
    REQUIRE(sheets);
    REQUIRE(*sheets);
    CHECK(json::dump(**sheets) ==
          R"([{"name":"Data","rows":4,"columns":2,"start":"A1"},{"name":"Offset","rows":2,"columns":2,"start":"D3"}])");
    auto cells = (*doc)->query(docs::DataQuery{docs::query::Cells{"Data", "A3:B4"}});
    REQUIRE(cells);
    REQUIRE(*cells);
    CHECK(json::dump(**cells) ==
          R"j({"sheet":"Data","range":"A3:B4","rows":[[{"address":"A3","value":{"type":"bool","value":true},"formula":null},{"address":"B3","value":{"type":"float","value":3.0},"formula":"SUM(B2:B2)"}],[{"address":"A4","value":{"type":"date_time","value":{"serial":45366.0,"iso":"2024-03-15T00:00:00"}},"formula":null},{"address":"B4","value":{"type":"empty"},"formula":null}]]})j");
    CHECK_FALSE((*doc)->query(docs::DataQuery{docs::query::Slides{}}));

    auto far = docs::XlsxDoc::parse(hand_workbook(R"(<row r="200"><c r="CV200"><v>1</v></c></row>)"));
    REQUIRE(far);
    auto big = (*far)->query(docs::DataQuery{docs::query::Cells{std::nullopt, "A1:CV200"}});
    REQUIRE(big);
    REQUIRE_FALSE(*big);
    CHECK(big->error().message() == "range A1:CV200 has 20000 cells, request at most 10000 at a time");
}

TEST_CASE("xlsx: cell types, styles, shared strings and shared formulas follow calamine") {
    const std::string rows =
        R"(<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><v>2</v></c></row>)"
        R"(<row r="2"><c r="A2" t="inlineStr"><is><t>inline</t></is></c><c r="B2" t="e"><v>#DIV/0!</v></c><c r="C2" t="str"><f>"x"&amp;"y"</f><v>xy</v></c></row>)"
        R"(<row r="3"><c r="A3" s="1"><v>1.0625</v></c><c r="B3" s="2"><v>2.5</v></c><c r="C3" s="3"><v>45366.5</v></c></row>)"
        R"(<row r="4"><c r="A4"><f t="shared" ref="A4:C4" si="0">A1+$B$1+B$2</f><v>1</v></c><c r="B4"><f t="shared" si="0"/><v>2</v></c><c r="C4"><f t="shared" si="0"/></c></row>)"
        R"(<row><c><v>7</v></c><c t="d"><v>2024-01-02T03:04:05</v></c><c t="b"><v>false</v></c></row>)"
        R"(<row r="6"><c r="A6"><v>1e-5</v></c><c r="B6"><v>1234567890123456</v></c><c r="C6" s="4"><v>-0.5</v></c></row>)";
    auto doc = docs::XlsxDoc::parse(hand_workbook(rows));
    REQUIRE_MESSAGE(doc, (doc ? "" : doc.error().message()));
    // The chart sheet is not listed.
    REQUIRE((*doc)->page_count() == 1);
    auto block = (*doc)->cells(std::nullopt, "A1:C6");
    REQUIRE(block);
    const auto& r = block->rows;
    CHECK(r[0][0].value == Cell{Cell::String{" plain "}});
    CHECK(r[0][1].value == Cell{Cell::String{"rich text"}});
    CHECK(r[0][2].value == Cell{Cell::String{"a\rb"}});
    CHECK(r[1][0].value == Cell{Cell::String{"inline"}});
    CHECK(r[1][1].value == Cell{Cell::Error{"#DIV/0!"}});
    CHECK(r[1][2].value == Cell{Cell::String{"xy"}});
    CHECK(r[1][2].formula == "\"x\"&\"y\"");
    CHECK(r[2][0].value == Cell{Cell::Duration{1.0625, "PT25H30M0S"}});
    CHECK(r[2][1].value == Cell{Cell::Float{2.5}});
    CHECK(r[2][2].value == Cell{Cell::DateTime{45366.5, "2024-03-15T12:00:00"}});
    CHECK(r[3][0].formula == "A1+$B$1+B$2");
    CHECK(r[3][1].formula == "B1+$B$1+C$2");
    CHECK(r[3][2].formula == "C1+$B$1+D$2");
    CHECK(r[3][2].value == Cell{Cell::Empty{}});
    CHECK(r[4][0].value == Cell{Cell::Float{7.0}});
    CHECK(r[4][1].value == Cell{Cell::DateTime{std::nullopt, "2024-01-02T03:04:05"}});
    CHECK(r[4][2].value == Cell{Cell::Bool{false}});
    CHECK(r[5][2].value == Cell{Cell::Duration{-0.5, "-PT12H0M0S"}});

    auto d1904 = docs::XlsxDoc::parse(hand_workbook(R"(<row r="1"><c r="A1" s="3"><v>0</v></c></row>)", true));
    REQUIRE(d1904);
    CHECK((*d1904)->cells(std::nullopt, "A1")->rows[0][0].value == Cell{Cell::DateTime{0.0, "1904-01-01T00:00:00"}});

    auto bad_error = docs::XlsxDoc::parse(hand_workbook(R"(<row r="1"><c r="A1" t="e"><v>#OOPS</v></c></row>)"));
    REQUIRE_FALSE(bad_error);
    CHECK(bad_error.error().message() == "invalid XLSX file: Unsupported cell error value '#OOPS'");
    auto bad_index = docs::XlsxDoc::parse(hand_workbook(R"(<row r="1"><c r="A1" t="s"><v>9</v></c></row>)"));
    REQUIRE_FALSE(bad_index);
    CHECK(bad_index.error().message() == "invalid XLSX file: Cell string index not found in shared strings table");
}

TEST_CASE("xlsx: General formatting matches Rust") {
    using docs::xlsx::general;
    CHECK(general(3.0) == "3");
    CHECK(general(-0.25) == "-0.25");
    CHECK(general(1.0 / 3.0) == "0.333333333");
    CHECK(general(1234567890123456.0) == "1.2346e15");
    CHECK(general(0.00001) == "1.0000e-5");
    CHECK(general(-2.5e-7) == "-2.5000e-7");
    CHECK(general(0.0) == "0");
}

TEST_CASE("xlsx: broken input is an invalid XLSX file") {
    for (const std::string junk : {std::string(), std::string("not a zip"), std::string("PK\x03\x04\x14\x00garbage", 12)}) {
        auto r = docs::XlsxDoc::parse(std::span(reinterpret_cast<const std::uint8_t*>(junk.data()), junk.size()));
        REQUIRE_FALSE(r);
        CHECK(r.error().kind() == docs::DocsError::Kind::Invalid);
        CHECK(r.error().format() == docs::Format::Xlsx);
    }
    // A zip without the workbook parts, and one with broken XML.
    auto empty = docs::XlsxDoc::parse(loa::surfaces::testing::zip_parts({{"a.txt", "x"}}));
    REQUIRE_FALSE(empty);
    CHECK(empty.error().message() == "invalid XLSX file: File not found '_rels/.rels'");
    auto xml = docs::XlsxDoc::parse(hand_workbook("<row><c><v>1</c></row>"));
    REQUIRE_FALSE(xml);
    CHECK(xml.error().kind() == docs::DocsError::Kind::Invalid);
}
