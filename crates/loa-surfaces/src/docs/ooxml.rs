//! Office Open XML packages: zip parts, relationships and quick-xml attribute helpers.

use crate::docs::{DocsError, Format, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use quick_xml::{Reader, XmlVersion};
use std::borrow::Cow;
use std::io::{Cursor, Read};

/// Largest decompressed part accepted, a guard against zip bombs.
const MAX_PART_BYTES: u64 = 512 << 20;

pub(crate) type XmlResult<T> = std::result::Result<T, String>;

/// The package zip of a `format` file.
pub(crate) struct Package<'b> {
    zip: zip::ZipArchive<Cursor<&'b [u8]>>,
    format: Format,
}

/// A resolved package relationship.
pub(crate) struct Rel {
    pub id: String,
    pub kind: String,
    /// Part path inside the zip, without a leading `/`.
    pub target: String,
}

impl<'b> Package<'b> {
    pub fn open(bytes: &'b [u8], format: Format) -> Result<Self> {
        let zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| DocsError::invalid(format, e))?;
        Ok(Self { zip, format })
    }

    pub fn invalid(&self, message: impl std::fmt::Display) -> DocsError {
        DocsError::invalid(self.format, message)
    }

    /// Bytes of a part, `None` when the zip has no such entry.
    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        let file = match self.zip.by_name(name) {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(DocsError::invalid(self.format, format!("{name}: {e}"))),
        };
        let mut out = Vec::new();
        let read = file.take(MAX_PART_BYTES + 1).read_to_end(&mut out);
        read.map_err(|e| self.invalid(format!("{name}: {e}")))?;
        if out.len() as u64 > MAX_PART_BYTES {
            return Err(self.invalid(format!("{name} is larger than {} MiB", MAX_PART_BYTES >> 20)));
        }
        Ok(Some(out))
    }

    /// A UTF-8 XML part parsed by `parse`, with errors naming the part.
    pub fn parsed<T>(&mut self, name: &str, parse: impl FnOnce(&str) -> XmlResult<T>) -> Result<Option<T>> {
        let Some(bytes) = self.read(name)? else { return Ok(None) };
        let xml = String::from_utf8(bytes).map_err(|_| self.invalid(format!("{name} is not UTF-8 XML")))?;
        parse(&xml).map(Some).map_err(|m| self.invalid(format!("{name}: {m}")))
    }

    /// The internal relationships of `part`, none when it has no rels part.
    pub fn rels(&mut self, part: &str) -> Result<Vec<Rel>> {
        let path = resolve_target(part, &format!("_rels/{}.rels", part.rsplit('/').next().unwrap_or(part)));
        let rels = self.parsed(&path, |xml| {
            let mut rels = Vec::new();
            each_element(xml, |e| {
                if e.local_name().as_ref() != "Relationship" || attr(e, "TargetMode")?.is_some_and(|m| m == "External") {
                    return Ok(());
                }
                let (Some(id), Some(kind), Some(target)) = (attr(e, "Id")?, attr(e, "Type")?, attr(e, "Target")?) else {
                    return Err("Relationship lacks Id, Type or Target".into());
                };
                rels.push(Rel { id: id.into_owned(), kind: kind.into_owned(), target: resolve_target(part, &target) });
                Ok(())
            })?;
            Ok(rels)
        })?;
        Ok(rels.unwrap_or_default())
    }
}

/// Resolves a relationship target against the directory of its source part.
pub(crate) fn resolve_target(source: &str, target: &str) -> String {
    if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_owned();
    }
    let mut segments: Vec<&str> = source.rsplit_once('/').map(|(dir, _)| dir.split('/').collect()).unwrap_or_default();
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => drop(segments.pop()),
            s => segments.push(s),
        }
    }
    segments.join("/")
}

/// A reader over `xml` with any byte order mark stripped.
pub(crate) fn xml_reader(xml: &str) -> Reader<&[u8]> {
    Reader::from_str(xml.strip_prefix('\u{feff}').unwrap_or(xml))
}

/// Calls `f` on every start and empty element of `xml`.
pub(crate) fn each_element(xml: &str, mut f: impl FnMut(&BytesStart<'_>) -> XmlResult<()>) -> XmlResult<()> {
    let mut reader = xml_reader(xml);
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) | Event::Empty(e) => f(&e)?,
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Value of the first attribute whose name matches `matches`, entities resolved.
pub(crate) fn find_attr<'e>(e: &'e BytesStart<'_>, matches: impl Fn(QName<'_>) -> bool) -> XmlResult<Option<Cow<'e, str>>> {
    for attr in e.attributes().with_checks(false) {
        let attr = attr.map_err(|err| err.to_string())?;
        if matches(attr.key) {
            return attr.normalized_value(XmlVersion::Implicit1_0).map(Some).map_err(|err| err.to_string());
        }
    }
    Ok(None)
}

/// Attribute by local name, ignoring any prefix.
pub(crate) fn attr<'e>(e: &'e BytesStart<'_>, local: &str) -> XmlResult<Option<Cow<'e, str>>> {
    find_attr(e, |key| key.local_name().as_ref() == local)
}

/// A whole number attribute (EMU, hundredths of a point).
pub(crate) fn num_attr(e: &BytesStart<'_>, local: &str) -> XmlResult<Option<f64>> {
    let Some(v) = attr(e, local)? else { return Ok(None) };
    v.trim().parse::<i64>().map(|n| Some(n as f64)).map_err(|_| format!("attribute {local}=\"{v}\" is not a number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_resolve_against_the_source_directory() {
        for (source, target, want) in [
            ("ppt/slides/slide1.xml", "../media/a.png", "ppt/media/a.png"),
            ("ppt/presentation.xml", "slides/slide1.xml", "ppt/slides/slide1.xml"),
            ("ppt/slides/slide1.xml", "/ppt/media/b.png", "ppt/media/b.png"),
            ("root.xml", "./x.xml", "x.xml"),
        ] {
            assert_eq!(resolve_target(source, target), want);
        }
    }
}
