//! Text out of an uploaded or fetched document. The bytes are read in memory
//! and dropped: only the extracted text goes on (into a message), nothing is
//! written anywhere. The type is judged by extension and then by content, so a
//! renamed binary is refused.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{anyhow, bail, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

pub const MAX_UPLOAD: usize = 10 * 1024 * 1024;
pub const ALLOWED: &str = "pdf, docx, xlsx, pptx, odt, ods, odp, txt, md, csv, tsv";
const MAX_CHARS: usize = 200_000;
/// Zip bomb guards: entries in the archive and bytes read out of all of them.
const MAX_ENTRIES: usize = 2000;
const MAX_UNZIPPED: u64 = 50 * 1024 * 1024;

/// The lowercase extension of `name` when it is an allowed type.
pub fn allowed_extension(name: &str) -> Result<String> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if !ALLOWED.split(", ").any(|a| a == ext) {
        bail!("this file type is not accepted; send one of: {ALLOWED}");
    }
    Ok(ext)
}

/// Run `extract` off the async worker: parsing a large file takes a while.
pub async fn extract_async(name: String, bytes: Vec<u8>) -> Result<String> {
    tokio::task::spawn_blocking(move || extract(&name, &bytes)).await.map_err(|e| anyhow!("extraction failed: {e}"))?
}

pub fn extract(name: &str, bytes: &[u8]) -> Result<String> {
    let ext = allowed_extension(name)?;
    if bytes.len() > MAX_UPLOAD {
        bail!("the file is larger than {} MB", MAX_UPLOAD / 1024 / 1024);
    }
    let text = match ext.as_str() {
        "pdf" => pdf(bytes)?,
        "docx" => {
            let mut z = Zip::open(bytes)?;
            units(&z.need("word/document.xml")?, "p", false)?.join("\n")
        }
        "xlsx" => xlsx(&mut Zip::open(bytes)?)?,
        "pptx" => pptx(&mut Zip::open(bytes)?)?,
        "odt" | "ods" | "odp" => odf(&mut Zip::open(bytes)?, &ext)?,
        _ => plain(bytes)?,
    };
    if text.trim().is_empty() {
        bail!("no text found in the file");
    }
    Ok(cap(text))
}

/// The file name as shown to the model: last path component, no control characters.
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.chars().filter(|c| !c.is_control()).take(100).collect()
}

/// The extracted text as a message block. The fence is longer than any run of
/// backticks in the text, so the text cannot close it early.
pub fn attachment(name: &str, text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("[file: {}]\n{fence}\n{text}\n{fence}", clean_name(name))
}

fn cap(mut text: String) -> String {
    if let Some((i, _)) = text.char_indices().nth(MAX_CHARS) {
        text.truncate(i);
        text.push_str("\n[truncated]");
    }
    text
}

fn plain(bytes: &[u8]) -> Result<String> {
    if bytes.contains(&0) {
        bail!("this is not a text file (it contains binary data)");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("this is not a UTF-8 text file"))?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

fn pdf(bytes: &[u8]) -> Result<String> {
    if !bytes.starts_with(b"%PDF-") {
        bail!("this is not a PDF file");
    }
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| anyhow!("the PDF could not be read: {e}"))?;
    if doc.is_encrypted() {
        bail!("the PDF is encrypted");
    }
    let mut out = String::new();
    for page in doc.get_pages().keys() {
        if let Ok(t) = doc.extract_text(&[*page]) {
            out.push_str(&t);
            out.push('\n');
        }
        if out.len() > MAX_CHARS * 4 {
            break;
        }
    }
    Ok(out)
}

/// A zip whose total decompressed reads are bounded.
struct Zip<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    left: u64,
}

impl<'a> Zip<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self> {
        if !bytes.starts_with(b"PK\x03\x04") {
            bail!("the content is not what the file extension says (not a zip-based document)");
        }
        let archive = ZipArchive::new(Cursor::new(bytes)).map_err(|e| anyhow!("the file could not be read: {e}"))?;
        if archive.len() > MAX_ENTRIES {
            bail!("the archive has too many entries");
        }
        Ok(Zip { archive, left: MAX_UNZIPPED })
    }

    fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        let file = match self.archive.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(anyhow!("the file could not be read: {e}")),
        };
        let mut buf = Vec::new();
        file.take(self.left + 1).read_to_end(&mut buf).context("the file could not be read")?;
        if buf.len() as u64 > self.left {
            bail!("the file expands to too much data");
        }
        self.left -= buf.len() as u64;
        Ok(Some(buf))
    }

    fn need(&mut self, name: &str) -> Result<Vec<u8>> {
        self.read(name)?.ok_or_else(|| anyhow!("this is not a valid document of that type ({name} is missing)"))
    }
}

/// One parsed XML event. Element names are the local part; attribute keys keep
/// their prefix (`r:id` is not `id`).
enum Ev {
    Open(String, Vec<(String, String)>),
    Empty(String, Vec<(String, String)>),
    Close(String),
    Text(String),
}

fn walk(xml: &[u8], mut f: impl FnMut(Ev)) -> Result<()> {
    fn name(n: quick_xml::name::QName) -> String {
        String::from_utf8_lossy(n.local_name().as_ref()).into_owned()
    }
    fn attrs(e: &quick_xml::events::BytesStart) -> Vec<(String, String)> {
        e.attributes()
            .flatten()
            .map(|a| {
                let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
                (key, a.unescape_value().map(|v| v.into_owned()).unwrap_or_default())
            })
            .collect()
    }
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).map_err(|e| anyhow!("malformed XML in the document: {e}"))? {
            Event::Start(e) => f(Ev::Open(name(e.name()), attrs(&e))),
            Event::Empty(e) => f(Ev::Empty(name(e.name()), attrs(&e))),
            Event::End(e) => f(Ev::Close(name(e.name()))),
            Event::Text(t) => f(Ev::Text(t.unescape().map(|t| t.into_owned()).unwrap_or_default())),
            Event::CData(c) => f(Ev::Text(String::from_utf8_lossy(&c.into_inner()).into_owned())),
            Event::Eof => return Ok(()),
            _ => {}
        }
        buf.clear();
    }
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// The text (`t` elements) of each `unit` element (`p` paragraph, `si` shared string).
fn units(xml: &[u8], unit: &str, keep_empty: bool) -> Result<Vec<String>> {
    let (mut out, mut cur, mut in_t) = (Vec::new(), String::new(), false);
    walk(xml, |ev| match ev {
        Ev::Open(n, _) if n == "t" => in_t = true,
        Ev::Close(n) if n == "t" => in_t = false,
        Ev::Empty(n, _) if n == "tab" => cur.push('\t'),
        Ev::Empty(n, _) if n == "br" => cur.push('\n'),
        Ev::Text(t) if in_t => cur.push_str(&t),
        Ev::Close(n) if n == unit => {
            if keep_empty || !cur.trim().is_empty() {
                out.push(if keep_empty { std::mem::take(&mut cur) } else { cur.trim().to_string() });
            }
            cur.clear();
        }
        _ => {}
    })?;
    Ok(out)
}

/// Relationship id to part path, for the parts a workbook or presentation lists.
fn relations(z: &mut Zip, rels: &str, base: &str) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    walk(&z.need(rels)?, |ev| {
        if let Ev::Empty(n, a) | Ev::Open(n, a) = ev {
            if let (true, Some(id), Some(target)) = (n == "Relationship", attr(&a, "Id"), attr(&a, "Target")) {
                let path = match target.strip_prefix('/') {
                    Some(abs) => abs.to_string(),
                    None => format!("{base}/{target}"),
                };
                map.insert(id.to_string(), path);
            }
        }
    })?;
    Ok(map)
}

/// The `r:id` (and `name`) of every `elem` element, in document order.
fn listed(xml: &[u8], elem: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    walk(xml, |ev| {
        if let Ev::Empty(n, a) | Ev::Open(n, a) = ev {
            if let (true, Some(id)) = (n == elem, attr(&a, "r:id")) {
                out.push((attr(&a, "name").unwrap_or_default().to_string(), id.to_string()));
            }
        }
    })?;
    Ok(out)
}

fn xlsx(z: &mut Zip) -> Result<String> {
    let book = z.need("xl/workbook.xml")?;
    let shared = match z.read("xl/sharedStrings.xml")? {
        Some(xml) => units(&xml, "si", true)?,
        None => Vec::new(),
    };
    let paths = relations(z, "xl/_rels/workbook.xml.rels", "xl")?;
    let mut out = String::new();
    for (name, id) in listed(&book, "sheet")? {
        let Some(path) = paths.get(&id) else { continue };
        let Some(xml) = z.read(path)? else { continue };
        out.push_str(&format!("Sheet: {name}\n{}\n", sheet_rows(&xml, &shared)?));
        if out.len() > MAX_CHARS * 4 {
            break;
        }
    }
    Ok(out)
}

/// Column letters of a cell reference such as `AB12`, as a zero-based index.
fn column(reference: &str) -> usize {
    let n = reference
        .bytes()
        .take_while(|b| b.is_ascii_alphabetic())
        .fold(0usize, |n, b| (n * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize).min(20_000));
    n.saturating_sub(1).min(16_383)
}

fn sheet_rows(xml: &[u8], shared: &[String]) -> Result<String> {
    let (mut out, mut row, mut col) = (String::new(), Vec::<String>::new(), 0);
    let (mut kind, mut value, mut grab) = (String::new(), String::new(), false);
    walk(xml, |ev| match ev {
        Ev::Open(n, a) if n == "c" => {
            col = attr(&a, "r").map_or(row.len(), column);
            kind = attr(&a, "t").unwrap_or_default().to_string();
            value.clear();
        }
        Ev::Open(n, _) if n == "v" || n == "t" => grab = true,
        Ev::Text(t) if grab => value.push_str(&t),
        Ev::Close(n) if n == "v" || n == "t" => grab = false,
        Ev::Close(n) if n == "c" => {
            let cell = if kind == "s" {
                value.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).cloned().unwrap_or_default()
            } else {
                value.clone()
            };
            if row.len() <= col {
                row.resize(col + 1, String::new());
            }
            row[col] = cell;
        }
        Ev::Close(n) if n == "row" => {
            while row.last().is_some_and(String::is_empty) {
                row.pop();
            }
            if !row.is_empty() {
                out.push_str(&row.join("\t"));
                out.push('\n');
            }
            row.clear();
        }
        _ => {}
    })?;
    Ok(out)
}

fn pptx(z: &mut Zip) -> Result<String> {
    let deck = z.need("ppt/presentation.xml")?;
    let paths = relations(z, "ppt/_rels/presentation.xml.rels", "ppt")?;
    let mut out = String::new();
    for (i, (_, id)) in listed(&deck, "sldId")?.iter().enumerate() {
        let Some(path) = paths.get(id) else { continue };
        let Some(xml) = z.read(path)? else { continue };
        out.push_str(&format!("Slide {}:\n{}\n\n", i + 1, units(&xml, "p", false)?.join("\n")));
        if out.len() > MAX_CHARS * 4 {
            break;
        }
    }
    Ok(out)
}

fn odf(z: &mut Zip, ext: &str) -> Result<String> {
    let want = match ext {
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        _ => "application/vnd.oasis.opendocument.presentation",
    };
    if String::from_utf8_lossy(&z.need("mimetype")?).trim() != want {
        bail!("the content is not what the file extension says (wrong document type)");
    }
    let xml = z.need("content.xml")?;
    let (mut out, mut para, mut in_para, mut slide) = (String::new(), String::new(), false, 0);
    let (mut row, mut cell, mut in_cell) = (Vec::<String>::new(), String::new(), false);
    walk(&xml, |ev| match ev {
        Ev::Open(n, a) => match n.as_str() {
            "table" if ext == "ods" => {
                out.push_str(&format!("Sheet: {}\n", attr(&a, "table:name").unwrap_or_default()));
            }
            "page" => {
                slide += 1;
                out.push_str(&format!("Slide {slide}:\n"));
            }
            "table-cell" | "covered-table-cell" => (in_cell, cell) = (true, String::new()),
            "p" | "h" => (in_para, para) = (true, String::new()),
            _ => {}
        },
        Ev::Empty(n, _) => match n.as_str() {
            "s" if in_para => para.push(' '),
            "tab" if in_para => para.push('\t'),
            "line-break" if in_para => para.push('\n'),
            "table-cell" | "covered-table-cell" => row.push(String::new()),
            _ => {}
        },
        Ev::Text(t) if in_para => para.push_str(&t),
        Ev::Close(n) => match n.as_str() {
            "p" | "h" => {
                in_para = false;
                if in_cell {
                    if !cell.is_empty() {
                        cell.push(' ');
                    }
                    cell.push_str(para.trim());
                } else {
                    out.push_str(para.trim());
                    out.push('\n');
                }
            }
            "table-cell" | "covered-table-cell" => {
                in_cell = false;
                row.push(std::mem::take(&mut cell));
            }
            "table-row" => {
                while row.last().is_some_and(String::is_empty) {
                    row.pop();
                }
                if !row.is_empty() {
                    out.push_str(&row.join("\t"));
                    out.push('\n');
                }
                row.clear();
            }
            _ => {}
        },
        _ => {}
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 700 Td ({text}) Tj ET");
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offsets {
            pdf.extend(format!("{o:010} 00000 n \n").bytes());
        }
        pdf.extend(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes(),
        );
        pdf
    }

    #[test]
    fn docx_paragraphs() {
        let xml = r#"<w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> &amp; bye</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:body></w:document>"#;
        let text = extract("a.DOCX", &zip(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(text, "Hello & bye\nSecond");
    }

    #[test]
    fn xlsx_rows_resolve_shared_strings() {
        let book = r#"<workbook xmlns:r="r"><sheets><sheet name="Sales" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let rels = r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let shared = r#"<sst><si><t>Item</t></si><si><r><t>Pri</t></r><r><t>ce</t></r></si></sst>"#;
        let sheet = r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row><row r="2"><c r="A2"><v>7</v></c></row></sheetData></worksheet>"#;
        let bytes = zip(&[
            ("xl/workbook.xml", book),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
        ]);
        assert_eq!(extract("s.xlsx", &bytes).unwrap(), "Sheet: Sales\nItem\t\tPrice\n7\n\n");
    }

    #[test]
    fn pptx_slides_in_listed_order() {
        let deck = r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId1"/></p:sldIdLst></p:presentation>"#;
        let rels = r#"<Relationships><Relationship Id="rId1" Target="slides/slide1.xml"/><Relationship Id="rId2" Target="slides/slide2.xml"/></Relationships>"#;
        let slide = |t: &str| format!(r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#);
        let (s1, s2) = (slide("one"), slide("two"));
        let bytes = zip(&[
            ("ppt/presentation.xml", deck),
            ("ppt/_rels/presentation.xml.rels", rels),
            ("ppt/slides/slide1.xml", &s1),
            ("ppt/slides/slide2.xml", &s2),
        ]);
        assert_eq!(extract("d.pptx", &bytes).unwrap(), "Slide 1:\ntwo\n\nSlide 2:\none\n\n");
    }

    #[test]
    fn odt_text_and_wrong_mimetype() {
        let content = r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><text:p>Hi<text:s/>there</text:p></office:body></office:document-content>"#;
        let ok = zip(&[("mimetype", "application/vnd.oasis.opendocument.text"), ("content.xml", content)]);
        assert_eq!(extract("a.odt", &ok).unwrap(), "Hi there\n");
        assert!(extract("a.ods", &ok).is_err());
    }

    #[test]
    fn pdf_text() {
        let text = extract("a.pdf", &tiny_pdf("Hello PDF")).unwrap();
        assert!(text.contains("Hello PDF"), "{text:?}");
    }

    #[test]
    fn text_files() {
        assert_eq!(extract("n.md", b"# hi").unwrap(), "# hi");
        assert!(extract("n.txt", b"a\0b").is_err());
        assert!(extract("n.csv", &[0xff, 0xfe, b'a']).is_err());
        let long = "x".repeat(MAX_CHARS + 5);
        assert!(extract("n.txt", long.as_bytes()).unwrap().ends_with("[truncated]"));
    }

    #[test]
    fn refusals() {
        let pdf = tiny_pdf("x");
        for name in ["a.exe", "a.pdf.exe", "a.zip", "a.doc", "a.xls", "a.png", "noext", "a."] {
            assert!(extract(name, &pdf).is_err(), "{name}");
        }
        assert!(extract("a.pdf", b"MZ\x90\x00 not a pdf").is_err());
        assert!(extract("a.docx", &pdf).is_err());
        assert!(extract("a.docx", &zip(&[("hello.txt", "x")])).is_err());
        assert!(extract("a.txt", &vec![b'a'; MAX_UPLOAD + 1]).is_err());
    }
}
