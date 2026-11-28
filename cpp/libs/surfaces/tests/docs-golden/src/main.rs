use loa_surfaces::docs::{DataQuery, DocsServer, Fonts, Paged, pdf::PdfDoc};
use std::path::PathBuf;

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
}
