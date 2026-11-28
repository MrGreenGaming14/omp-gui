use loa_surfaces::docs::{DataQuery, DocsServer, Fonts, Paged, RenderOpts, docx::DocxDoc, pdf::PdfDoc, pptx::PptxDoc, xlsx::XlsxDoc};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

fn build_pdf(pages: &[(f32, f32, i32, &str)]) -> Vec<u8> {
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    for (i, (w, h, rotate, content)) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Rotate {rotate} /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i
        ));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len() + 1));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for (i, object) in objects.iter().enumerate() {
        xref.push_str(&format!("{:010} 00000 n \n", out.len()));
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let start = out.len();
    out.extend_from_slice(format!("{xref}trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

fn para(text: &str) -> docx_rs::Paragraph {
    docx_rs::Paragraph::new().add_run(docx_rs::Run::new().add_text(text))
}

/// Headings (by style name, id and `basedOn`), run formatting through styles, nested lists in several formats,
/// a table, a content control, page breaks and enough body text for three pages.
fn build_docx() -> Vec<u8> {
    use docx_rs::*;
    let level = |l: usize, format: &str, text: &str| {
        Level::new(l, Start::new(1), NumberFormat::new(format), LevelText::new(text), LevelJc::new("left"))
    };
    let item = |text: &str, id: usize, l: usize| para(text).numbering(NumberingId::new(id), IndentLevel::new(l));
    let cell = |text: &str| TableCell::new().add_paragraph(para(text));
    let mut docx = Docx::new()
        .page_size(8000, 10000)
        .page_margin(PageMargin::new().top(1440).left(1080).right(1080).bottom(1440))
        .add_style(Style::new("Title", StyleType::Paragraph).name("Title"))
        .add_style(Style::new("Heading1", StyleType::Paragraph).name("heading 1"))
        .add_style(Style::new("Custom", StyleType::Paragraph).name("heading 2"))
        .add_style(Style::new("Loud", StyleType::Paragraph).name("Loud").bold())
        .add_style(Style::new("Louder", StyleType::Paragraph).name("Louder").based_on("Loud").italic())
        .add_style(Style::new("Mark", StyleType::Character).name("Mark").underline("single"))
        .add_abstract_numbering(
            AbstractNumbering::new(5)
                .add_level(level(0, "decimal", "%1."))
                .add_level(level(1, "lowerLetter", "%1.%2)"))
                .add_level(level(2, "upperRoman", "(%3)")),
        )
        .add_abstract_numbering(AbstractNumbering::new(6).add_level(level(0, "bullet", "\u{2022}")))
        .add_numbering(Numbering::new(2, 5))
        .add_numbering(Numbering::new(3, 6))
        .add_paragraph(para("Golden document").style("Title"))
        .add_paragraph(para("Intro").style("Heading1"))
        .add_paragraph(
            Paragraph::new()
                .add_run(Run::new().add_text("plain "))
                .add_run(Run::new().add_text("strong").bold())
                .add_run(Run::new().add_text(" em").italic().underline("single").add_tab().add_text("x"))
                .add_run(Run::new().add_text(" marked").style("Mark")),
        )
        .add_paragraph(para("inherited").style("Louder"))
        .add_paragraph(item("first", 2, 0))
        .add_paragraph(item("sub", 2, 1))
        .add_paragraph(item("deeper", 2, 2))
        .add_paragraph(item("sub again", 2, 1))
        .add_paragraph(item("second", 2, 0))
        .add_paragraph(item("dot", 3, 0))
        .add_table(Table::new(vec![
            TableRow::new(vec![cell("a1"), cell("b1")]),
            TableRow::new(vec![TableCell::new().add_paragraph(para("two")).add_paragraph(para("lines")), cell("b2")]),
        ]))
        .add_paragraph(para("Details").style("Custom").page_break_before(true))
        .add_paragraph(Paragraph::new().add_run(Run::new().add_text("before").add_break(BreakType::TextWrapping).add_text("after")))
        .add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)))
        .add_paragraph(para("Deep").style("Heading3"));
    for i in 0..30 {
        docx = docx.add_paragraph(para(&format!("Body paragraph {i} with enough words to wrap across the narrow text column.")));
    }
    let mut bytes = Cursor::new(Vec::new());
    docx.build().pack(&mut bytes).unwrap();
    bytes.into_inner()
}

/// Three sheets: typed values, formulas and dates from A1; an empty sheet; data starting at D3.
fn build_xlsx() -> Vec<u8> {
    use rust_xlsxwriter::{Format as CellFormat, Formula, Workbook};
    let mut book = Workbook::new();
    let data = book.add_worksheet().set_name("Data").unwrap();
    data.write_string(0, 0, "Name").unwrap();
    data.write_string(0, 1, "Value").unwrap();
    data.write_string(0, 2, "Note").unwrap();
    data.write_string(1, 0, "apple").unwrap();
    data.write_number(1, 1, 3.0).unwrap();
    data.write_string(1, 2, "multi\nline").unwrap();
    data.write_boolean(2, 0, true).unwrap();
    data.write_formula(2, 1, Formula::new("=SUM(B2:B2)").set_result("3")).unwrap();
    data.write_number(2, 2, 1234567.891).unwrap();
    data.write_number_with_format(3, 0, 45366.0, &CellFormat::new().set_num_format("yyyy-mm-dd")).unwrap();
    data.write_number_with_format(3, 1, 45366.75, &CellFormat::new().set_num_format("yyyy-mm-dd hh:mm:ss")).unwrap();
    data.write_number_with_format(3, 2, 0.5, &CellFormat::new().set_num_format("hh:mm")).unwrap();
    data.write_number_with_format(4, 0, 1.0625, &CellFormat::new().set_num_format("[h]:mm:ss")).unwrap();
    data.write_number(4, 1, -0.000012).unwrap();
    data.write_number(4, 2, 1e20).unwrap();
    data.write_string(5, 0, "caf\u{e9} \u{2603}").unwrap();
    data.write_number(5, 2, 0.1).unwrap();
    book.add_worksheet().set_name("Empty").unwrap();
    let offset = book.add_worksheet().set_name("Offset").unwrap();
    offset.write_string(2, 3, "origin").unwrap();
    offset.write_number(3, 4, 42.0).unwrap();
    book.save_to_buffer().unwrap()
}

const PPTX_NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

fn zip_parts(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in parts {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn pptx_rels(entries: &[(&str, &str, &str)]) -> Vec<u8> {
    let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let body: String = entries
        .iter()
        .map(|(id, kind, target)| format!(r#"<Relationship Id="{id}" Type="{rel}/{kind}" Target="{target}"/>"#))
        .collect();
    format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#).into_bytes()
}

fn pptx_tree(root: &str, shapes: &str) -> Vec<u8> {
    format!(r#"<?xml version="1.0" encoding="UTF-8"?><p:{root} {PPTX_NS}><p:cSld><p:spTree><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>{shapes}</p:spTree></p:cSld></p:{root}>"#).into_bytes()
}

/// The `pptx.rs` test presentation plus a table, a master placeholder and an `mc:AlternateContent` shape.
fn build_pptx() -> Vec<u8> {
    let presentation = format!(
        r#"<p:presentation {PPTX_NS}><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#
    );
    let first = pptx_tree("sld", concat!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="457200" y="274320"/><a:ext cx="10972800" cy="1143000"/></a:xfrm></p:spPr>"#,
        r#"<p:txBody><a:p><a:r><a:rPr sz="4000" b="1"/><a:t>Quarterly results &amp; plans</a:t></a:r></a:p></p:txBody></p:sp>"#,
        r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content 2"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/>"#,
        r#"<p:txBody><a:p><a:r><a:t>Revenue up</a:t></a:r></a:p><a:p><a:r><a:t>Costs</a:t></a:r><a:br/><a:r><a:t>down</a:t></a:r></a:p><a:p><a:endParaRPr/></a:p></p:txBody></p:sp>"#,
        r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="4" name="Group 3"/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="1270000" y="4445000"/><a:ext cx="2540000" cy="2540000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr>"#,
        r#"<p:pic><p:nvPicPr><p:cNvPr id="5" name="Picture 4"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="0"/><a:ext cx="635000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:grpSp>"#,
    ));
    let second = pptx_tree("sld", concat!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp>"#,
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="3" name="Table 2"/></p:nvGraphicFramePr><p:xfrm><a:off x="635000" y="1905000"/><a:ext cx="6350000" cy="1270000"/></p:xfrm>"#,
        r#"<a:graphic><a:graphicData><a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:rPr sz="1200"/><a:t>h1</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>h2</a:t></a:r></a:p><a:p><a:r><a:t>more</a:t></a:r></a:p></a:txBody></a:tc></a:tr>"#,
        r#"<a:tr><a:tc><a:txBody><a:p><a:r><a:t>c1</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p/></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
        r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><mc:Choice Requires="x"><p:sp><p:nvSpPr><p:cNvPr id="4" name="Choice"/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>chosen</a:t></a:r></a:p></p:txBody></p:sp></mc:Choice>"#,
        r#"<mc:Fallback><p:sp><p:nvSpPr><p:cNvPr id="5" name="Fallback"/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>fallback</a:t></a:r></a:p></p:txBody></p:sp></mc:Fallback></mc:AlternateContent>"#,
        r#"<p:sp><p:nvSpPr><p:cNvPr id="6" name="Number"/><p:nvPr><p:ph type="sldNum" idx="12"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>2</a:t></a:r></a:p></p:txBody></p:sp>"#,
    ));
    let layout = pptx_tree("sldLayout", r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2"/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="838200" y="1825625"/><a:ext cx="10515600" cy="4351338"/></a:xfrm></p:spPr></p:sp>"#);
    let layout2 = pptx_tree("sldLayout", r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp>"#);
    let master = pptx_tree("sldMaster", r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Master Title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="635000" y="254000"/><a:ext cx="10922000" cy="1270000"/></a:xfrm></p:spPr></p:sp>"#);
    let notes = pptx_tree("notes", r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image"/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Speaker notes here</a:t></a:r></a:p></p:txBody></p:sp>"#);
    let mut png = Cursor::new(Vec::new());
    ::image::RgbaImage::from_pixel(4, 4, ::image::Rgba([255, 0, 0, 255])).write_to(&mut png, ::image::ImageFormat::Png).unwrap();
    zip_parts(&[
        ("ppt/presentation.xml", presentation.into_bytes()),
        ("ppt/_rels/presentation.xml.rels", pptx_rels(&[("rId2", "slide", "slides/slide1.xml"), ("rId3", "slide", "/ppt/slides/slide2.xml")])),
        ("ppt/slides/slide1.xml", second),
        ("ppt/slides/_rels/slide1.xml.rels", pptx_rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout2.xml")])),
        ("ppt/slides/slide2.xml", first),
        (
            "ppt/slides/_rels/slide2.xml.rels",
            pptx_rels(&[
                ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                ("rId2", "image", "../media/image1.png"),
                ("rId3", "notesSlide", "../notesSlides/notesSlide1.xml"),
            ]),
        ),
        ("ppt/slideLayouts/slideLayout1.xml", layout),
        ("ppt/slideLayouts/slideLayout2.xml", layout2),
        ("ppt/slideLayouts/_rels/slideLayout2.xml.rels", pptx_rels(&[("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")])),
        ("ppt/slideMasters/slideMaster1.xml", master),
        ("ppt/notesSlides/notesSlide1.xml", notes),
        ("ppt/media/image1.png", png.into_inner()),
    ])
}

fn write_json(out: &Path, name: &str, value: &serde_json::Value) {
    std::fs::write(out.join(name), serde_json::to_vec(value).unwrap()).unwrap();
}

/// Writes the Office fixtures and what the Rust parsers answer about them.
async fn office(out: &Path) {
    let (docx, xlsx, pptx) = (build_docx(), build_xlsx(), build_pptx());
    std::fs::write(out.join("sample.docx"), &docx).unwrap();
    std::fs::write(out.join("sample.xlsx"), &xlsx).unwrap();
    std::fs::write(out.join("sample.pptx"), &pptx).unwrap();

    let doc = DocxDoc::parse(&docx).unwrap();
    write_json(out, "docx.outline.json", &doc.query(&DataQuery::Outline).unwrap().unwrap());
    write_json(out, "docx.text.json", &doc.query(&DataQuery::Text { page: None }).unwrap().unwrap());
    let mut big = RenderOpts::default();
    big.typography.body = 24.0;
    let size = doc.page_size(0).unwrap();
    let pages = format!("{} {} {} {}\n", doc.page_count(), doc.page_count_with_opts(&big), size.width, size.height);
    std::fs::write(out.join("docx.pages.txt"), pages).unwrap();

    let book = XlsxDoc::parse(&xlsx).unwrap();
    write_json(out, "xlsx.sheets.json", &book.query(&DataQuery::Sheets).unwrap().unwrap());
    let cells = [(None, "A1:C6"), (Some("offset"), "$E$4"), (Some("Offset"), "A1:Z1000"), (Some("Empty"), "B2:C3"), (Some("Data"), "c2")];
    let mut lines = String::new();
    for (sheet, range) in cells {
        let q = DataQuery::Cells { sheet: sheet.map(str::to_owned), range: range.into() };
        lines.push_str(&serde_json::to_string(&book.query(&q).unwrap().unwrap()).unwrap());
        lines.push('\n');
    }
    std::fs::write(out.join("xlsx.cells.jsonl"), lines).unwrap();
    let mut sizes = String::new();
    for i in 0..book.page_count() {
        let s = book.page_size(i).unwrap();
        sizes.push_str(&format!("{i} {} {}\n", s.width, s.height));
    }
    std::fs::write(out.join("xlsx.sizes.txt"), sizes).unwrap();

    let deck = PptxDoc::parse(&pptx).unwrap();
    write_json(out, "pptx.slides.json", &deck.query(&DataQuery::Slides).unwrap().unwrap());
    write_json(out, "pptx.text.json", &deck.query(&DataQuery::Text { page: None }).unwrap().unwrap());

    // Request errors through the server, by file: unsupported queries and bad cell requests.
    let server = DocsServer::new(Fonts::none());
    let all = [
        DataQuery::Text { page: None },
        DataQuery::Text { page: Some(0) },
        DataQuery::Text { page: Some(5) },
        DataQuery::Sheets,
        DataQuery::Cells { sheet: None, range: "A1".into() },
        DataQuery::Cells { sheet: Some("Missing".into()), range: "A1".into() },
        DataQuery::Cells { sheet: None, range: "B".into() },
        DataQuery::Cells { sheet: None, range: "XFE1".into() },
        DataQuery::Cells { sheet: None, range: "A1:XFD1048576".into() },
        DataQuery::Outline,
        DataQuery::Slides,
    ];
    let mut errors = String::new();
    for file in ["sample.docx", "sample.xlsx", "sample.pptx"] {
        let surface = server.open_file(file).await.unwrap();
        for q in &all {
            if let Err(e) = surface.data(q.clone()).await {
                errors.push_str(&format!("{file}\t{}\t{e}\n", serde_json::to_string(q).unwrap()));
            }
        }
    }
    std::fs::write(out.join("office.errors.tsv"), errors).unwrap();
}

#[tokio::main]
async fn main() {
    let out = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").unwrap()).join("docs");
    std::fs::create_dir_all(&out).unwrap();
    let first = "BT /F1 12 Tf 72 600 Td (Second line) Tj ET BT /F1 12 Tf 72 500 Td (Split) Tj 40 0 Td (words) Tj ET \
                 BT /F1 12 Tf 72 450 Td [(Ke) -20 (rn)] TJ ET BT /F1 24 Tf 72 700 Td (Hello PDF) Tj ET 0 0 1 rg 100 100 50 50 re f";
    let rotated = "BT /F1 12 Tf 0 1 -1 0 280 100 Tm (Body text) Tj ET BT /F1 20 Tf 0 1 -1 0 250 100 Tm (Upright title) Tj ET \
                   1 0 0 rg 0 0 50 50 re f";
    let pdf = build_pdf(&[(612.0, 792.0, 0, first), (842.0, 595.0, 0, ""), (612.0, 792.0, 90, rotated), (595.276, 841.89, 0, "")]);
    std::fs::write(out.join("sample.pdf"), &pdf).unwrap();
    let doc = PdfDoc::parse(&pdf).unwrap();
    let mut sizes = String::new();
    for i in 0..doc.page_count() {
        let s = doc.page_size(i).unwrap();
        let r = doc.render_page(i, 1.3, &Fonts::none()).unwrap();
        sizes.push_str(&format!("{i} {} {} 1.3 {} {}\n", s.width, s.height, r.width, r.height));
    }
    std::fs::write(out.join("sample.sizes.txt"), sizes).unwrap();
    std::env::set_current_dir(&out).unwrap();
    let server = DocsServer::new(Fonts::none());
    let surface = server.open_file("sample.pdf").await.unwrap();
    let info = surface.data(DataQuery::Info).await.unwrap();
    let text = surface.data(DataQuery::Text { page: None }).await.unwrap();
    std::fs::write(out.join("text.json"), serde_json::to_vec(&text).unwrap()).unwrap();
    std::fs::write(out.join("info.json"), serde_json::to_vec(&info).unwrap()).unwrap();
    let queries = [
        DataQuery::Info,
        DataQuery::Text { page: None },
        DataQuery::Text { page: Some(2) },
        DataQuery::Sheets,
        DataQuery::Cells { sheet: None, range: "A1:B2".into() },
        DataQuery::Cells { sheet: Some("Data".into()), range: "C3".into() },
        DataQuery::Outline,
        DataQuery::Slides,
    ];
    let lines: Vec<String> = queries.iter().map(|q| serde_json::to_string(q).unwrap()).collect();
    std::fs::write(out.join("queries.jsonl"), lines.join("\n") + "\n").unwrap();
    office(&out).await;
}
