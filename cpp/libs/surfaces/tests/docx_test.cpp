#include <doctest/doctest.h>

#include <string>
#include <utility>
#include <vector>

#include "loa/runtime/json.hpp"
#include "loa/surfaces/docs/docx.hpp"
#include "ooxml_zip.hpp"

namespace docs = loa::surfaces::docs;
namespace docx = loa::surfaces::docs::docx;
using docs::DocsError;
using docs::Format;

namespace {

// The parts `docx_rs::Docx::build().pack()` writes, reduced to what the reader looks at.
constexpr const char* W_NS = R"(xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main")";

std::string run(const std::string& text, const std::string& props = "") {
    return "<w:r><w:rPr>" + props + R"(</w:rPr><w:t xml:space="preserve">)" + text + "</w:t></w:r>";
}

std::string para(const std::string& runs, const std::string& props = "") {
    return "<w:p><w:pPr>" + props + "<w:rPr /></w:pPr>" + runs + "</w:p>";
}

std::string styled(const std::string& text, const std::string& style, const std::string& props = "") {
    return para(run(text), R"(<w:pStyle w:val=")" + style + R"(" />)" + props);
}

std::string table(const std::vector<std::string>& cells) {
    std::string row = "<w:tr><w:trPr />";
    for (const auto& cell : cells) row += R"(<w:tc><w:tcPr><w:tcW w:w="0" w:type="auto" /></w:tcPr>)" + para(run(cell)) + "</w:tc>";
    return R"(<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto" /><w:jc w:val="left" /></w:tblPr><w:tblGrid />)" + row +
           "</w:tr></w:tbl>";
}

std::string style(const std::string& id, const std::string& name) {
    return R"(<w:style w:type="paragraph" w:styleId=")" + id + R"("><w:name w:val=")" + name +
           R"(" /><w:rPr /><w:pPr><w:rPr /></w:pPr><w:qFormat /></w:style>)";
}

std::vector<std::uint8_t> pack(const std::string& body, const std::string& styles, const std::string& numbering) {
    const std::string document = std::string(R"(<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document )") + W_NS +
                                 "><w:body>" + body +
                                 R"(<w:sectPr><w:pgSz w:w="8000" w:h="10000" /><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="851" w:footer="992" w:gutter="0" /><w:cols w:space="425" w:num="1" /></w:sectPr></w:body></w:document>)";
    return loa::surfaces::testing::zip_parts({
        {"[Content_Types].xml",
         R"(<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml" /><Default Extension="xml" ContentType="application/xml" /><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml" /><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml" /><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml" /></Types>)"},
        {"_rels/.rels",
         R"(<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml" /></Relationships>)"},
        {"word/_rels/document.xml.rels",
         R"(<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml" /><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml" /></Relationships>)"},
        {"word/document.xml", document},
        {"word/styles.xml", std::string(R"(<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles )") + W_NS +
                                R"(><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="21" /></w:rPr></w:rPrDefault></w:docDefaults>)" +
                                styles + "</w:styles>"},
        {"word/numbering.xml", std::string(R"(<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering )") + W_NS +
                                   ">" + numbering + "</w:numbering>"},
    });
}

std::string level(int l, const std::string& format, const std::string& text) {
    return R"(<w:lvl w:ilvl=")" + std::to_string(l) + R"("><w:start w:val="1" /><w:numFmt w:val=")" + format +
           R"(" /><w:lvlText w:val=")" + text + R"(" /><w:lvlJc w:val="left" /><w:pPr><w:rPr /></w:pPr><w:rPr /></w:lvl>)";
}

// docx-rs always writes its own abstract numbering 1 and numbering 1.
std::string default_numbering() {
    return R"(<w:abstractNum w:abstractNumId="1">)" + level(0, "decimal", "%1.") + "</w:abstractNum>";
}

bool any_op(const docx::Pages& pages, auto pred) {
    for (const auto& page : pages)
        for (const auto& op : page)
            if (const auto* t = std::get_if<docx::op::Text>(&op); t != nullptr && pred(*t)) return true;
    return false;
}

} // namespace

TEST_CASE("docx body typography changes layout and pagination without changing headings") {
    std::string body = styled("Heading", "Heading1") + table({"table text"});
    for (int i = 0; i < 25; ++i) body += para(run("Body text with enough words to wrap at a larger size."));
    const auto bytes = pack(body, style("Heading1", "heading 1"), default_numbering() + R"(<w:num w:numId="1"><w:abstractNumId w:val="1" /></w:num>)");
    auto doc = docs::DocxDoc::parse(bytes);
    REQUIRE(doc);
    const auto& d = **doc;
    docs::RenderOpts opts;
    opts.typography.body = 24.0F;
    const auto pages = docx::layout(d.outline(), d.page(), d.margins(), docs::Fonts::none(), opts.typography.body);
    CHECK(pages.size() > d.page_count());
    CHECK(d.page_count_with_opts(opts) == pages.size());
    CHECK(d.page_size_with_opts(pages.size() - 1, opts).value() == d.page());
    const auto out_of_range = d.page_size_with_opts(pages.size(), opts);
    REQUIRE_FALSE(out_of_range);
    CHECK(out_of_range.error().kind() == DocsError::Kind::PageRange);
    CHECK(any_op(pages, [](const docx::op::Text& t) { return t.block == 0 && t.size == docx::HEADING_SIZES[1]; }));
    CHECK(any_op(pages, [](const docx::op::Text& t) { return t.cell.has_value() && t.size == 24.0F; }));
    CHECK(any_op(pages, [](const docx::op::Text& t) { return t.block == 2 && t.size == 24.0F; }));
    CHECK(d.render_page(0, 1.0F, docs::Fonts::none()).value().data != d.render(0, 1.0F, docs::Fonts::none(), opts).value().data);
    CHECK(d.render(pages.size() - 1, 1.0F, docs::Fonts::none(), opts).has_value());
}

TEST_CASE("a docx document round-trips to outline, text, pages and pixels") {
    const std::string bold = "<w:b /><w:bCs />";
    const std::string em = R"(<w:u w:val="single" /><w:i /><w:iCs />)";
    const auto item = [](const std::string& text, int l) {
        return para(run(text), R"(<w:numPr><w:numId w:val="2" /><w:ilvl w:val=")" + std::to_string(l) + R"(" /></w:numPr>)");
    };
    const std::string em_run = R"(<w:r><w:rPr>)" + em + R"(</w:rPr><w:t xml:space="preserve"> em</w:t><w:tab /><w:t xml:space="preserve">x</w:t></w:r>)";
    const std::string body = styled("Intro", "Heading1") + para(run("plain ") + run("strong", bold) + em_run) + item("first", 0) +
                             item("sub", 1) + item("second", 0) + table({"a1", "b1"}) +
                             styled("Details", "Custom", "<w:pageBreakBefore />") +
                             para(R"(<w:r><w:rPr /><w:br w:type="page" /></w:r>)") + styled("Deep", "Heading3");
    const std::string numbering = default_numbering() + R"(<w:abstractNum w:abstractNumId="5">)" + level(0, "decimal", "%1.") +
                                  level(1, "lowerLetter", "%1.%2)") + "</w:abstractNum>" +
                                  R"(<w:num w:numId="1"><w:abstractNumId w:val="1" /></w:num><w:num w:numId="2"><w:abstractNumId w:val="5" /></w:num>)";
    const auto bytes = pack(body, style("Heading1", "heading 1") + style("Custom", "heading 2"), numbering);
    auto parsed = docs::DocxDoc::parse(bytes);
    REQUIRE(parsed);
    const auto& doc = **parsed;

    CHECK(doc.text() == "Intro\nplain strong em\tx\nfirst\nsub\nsecond\na1\tb1\nDetails\n\n\nDeep");
    std::vector<const docx::Paragraph*> paragraphs;
    for (const auto& b : doc.outline().blocks)
        if (const auto* p = b.paragraph()) paragraphs.push_back(p);
    std::vector<std::pair<int, std::string>> headings;
    for (const auto* p : paragraphs)
        if (p->heading_level) headings.emplace_back(*p->heading_level, p->text);
    CHECK(headings == std::vector<std::pair<int, std::string>>{{1, "Intro"}, {2, "Details"}, {3, "Deep"}});
    CHECK(paragraphs[1]->runs == std::vector<docx::Run>{{"plain ", false, false, false}, {"strong", true, false, false}, {" em\tx", false, true, true}});
    std::vector<std::pair<int, std::string>> markers;
    for (std::size_t i = 2; i < 5; ++i) {
        REQUIRE(paragraphs[i]->list);
        markers.emplace_back(paragraphs[i]->list->level, paragraphs[i]->list->marker);
    }
    CHECK(markers == std::vector<std::pair<int, std::string>>{{0, "1."}, {1, "1.a)"}, {0, "2."}});
    const auto* t = doc.outline().blocks[5].table();
    REQUIRE(t != nullptr);
    CHECK(t->rows == std::vector<std::vector<std::string>>{{"a1", "b1"}});

    CHECK(doc.page_count() == 3);
    CHECK(doc.page_size(0).value() == docs::PageSize{400.0F, 500.0F});
    const auto image = doc.render_page(0, 2.0F, docs::Fonts::none()).value();
    CHECK(image.width == 800);
    CHECK(image.height == 1000);
    const auto dark = [&](std::uint32_t x0, std::uint32_t y0, std::uint32_t x1, std::uint32_t y1) {
        for (std::uint32_t y = y0; y < y1; ++y)
            for (std::uint32_t x = x0; x < x1; ++x)
                if (auto p = image.pixel(x, y); p && (*p)[0] < 128) return true;
        return false;
    };
    // Text starts at the 72pt margins, scaled by 2.
    CHECK(dark(144, 144, 344, 210));
    CHECK_FALSE(dark(0, 0, 800, 140));
    CHECK_FALSE(dark(0, 0, 140, 1000));
    const auto out_of_range = doc.render_page(3, 1.0F, docs::Fonts::none());
    REQUIRE_FALSE(out_of_range);
    CHECK(out_of_range.error().kind() == DocsError::Kind::PageRange);
    CHECK(out_of_range.error().index() == 3);
    CHECK(out_of_range.error().count() == 3);

    const std::string not_zip = "not a zip";
    const auto bad = docs::DocxDoc::parse(std::span(reinterpret_cast<const std::uint8_t*>(not_zip.data()), not_zip.size()));
    REQUIRE_FALSE(bad);
    CHECK(bad.error().kind() == DocsError::Kind::Invalid);
    CHECK(bad.error().format() == Format::Docx);
}

TEST_CASE("docx outline JSON matches the serde shape") {
    docx::Paragraph p;
    p.style = "Heading1";
    p.heading_level = 1;
    p.list = docx::ListItem{2, 0, "1."};
    p.runs = {{"Hi", true, false, false}};
    p.text = "Hi";
    const docx::Outline outline{{docx::Block{p}, docx::Block{docx::Table{{{"a", "b"}}}}}};
    CHECK(loa::runtime::json::dump(outline.to_json()) ==
          R"({"blocks":[{"type":"paragraph","style":"Heading1","heading_level":1,"list":{"num_id":2,"level":0,"marker":"1."},)"
          R"("runs":[{"text":"Hi","bold":true,"italic":false,"underline":false}],"text":"Hi","page_break_before":false,)"
          R"("page_break_after":false},{"type":"table","rows":[["a","b"]]}]})");
}

TEST_CASE("docx run formatting follows docx-rs value semantics") {
    const std::string body = para(run("off", R"(<w:b w:val="false" /><w:i w:val="0" /><w:u w:val="none" />)") +
                                  run("styled", R"(<w:rStyle w:val="Strong" />)"));
    const std::string styles =
        R"(<w:style w:type="character" w:styleId="Base"><w:name w:val="Base" /><w:rPr><w:b /><w:u w:val="double" /></w:rPr></w:style>)"
        R"(<w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong" /><w:basedOn w:val="Base" /><w:rPr><w:i /></w:rPr></w:style>)";
    auto parsed = docs::DocxDoc::parse(pack(body, styles, ""));
    REQUIRE(parsed);
    const auto& runs = (*parsed)->outline().blocks.at(0).paragraph()->runs;
    CHECK(runs == std::vector<docx::Run>{{"off", false, false, false}, {"styled", true, true, true}});

    auto malformed = docs::DocxDoc::parse(pack(para(run("x", "<w:u />")), "", ""));
    REQUIRE_FALSE(malformed);
    CHECK(malformed.error().message().find("malformed document part") != std::string::npos);
}
