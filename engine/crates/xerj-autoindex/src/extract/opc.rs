//! Shared reading of OPC (Open Packaging Conventions) zip containers — the
//! package format under PPTX and XLSX: a bounded decompression budget for the
//! parts, `.rels` relationship parsing, and relationship-target resolution.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::io::{BufRead, BufReader, Read, Seek};

/// Decompression budget shared by every part read from one container.
///
/// SECURITY: a container is many parts, so there are two caps: one per part
/// (stops a crafted part inflating without limit) and one across the whole
/// container (a thousand capped parts is still a zip bomb). A read that hits
/// either sets `exhausted`, which the extractor reports as `truncated`.
pub(crate) struct Budget {
    pub part: u64,
    pub left: u64,
    pub exhausted: bool,
}

impl Budget {
    pub fn new(part: u64, total: u64) -> Self {
        Budget {
            part,
            left: total,
            exhausted: false,
        }
    }

    /// Read one part, or `None` when it is missing or the budget is spent.
    pub fn read<R: Read + Seek>(
        &mut self,
        z: &mut zip::ZipArchive<R>,
        name: &str,
    ) -> Option<Vec<u8>> {
        self.stream(z, name, |r| {
            let mut out = Vec::new();
            // A read error mid-part keeps what was read; the XML parser stops
            // at the damage like it does at a cap.
            r.read_to_end(&mut out).ok();
            out
        })
    }

    /// Hand one part to `f` as a bounded reader, charging the budget for what
    /// `f` actually consumed. For parts too large to hold whole (worksheets),
    /// and for parts a sampling caller stops reading early.
    pub fn stream<R: Read + Seek, T>(
        &mut self,
        z: &mut zip::ZipArchive<R>,
        name: &str,
        f: impl FnOnce(&mut dyn BufRead) -> T,
    ) -> Option<T> {
        if self.left == 0 {
            self.exhausted = true;
            return None;
        }
        let entry = z.by_name(name).ok()?;
        let cap = self.part.min(self.left);
        let mut counted = BufReader::with_capacity(
            64 << 10,
            Counted {
                inner: entry.take(cap),
                n: 0,
            },
        );
        let out = f(&mut counted);
        let used = counted.get_ref().n;
        self.left -= used.min(self.left);
        if used == cap {
            self.exhausted = true;
        }
        Some(out)
    }
}

struct Counted<R> {
    inner: R,
    n: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n += k as u64;
        Ok(k)
    }
}

pub(crate) struct Rel {
    pub id: String,
    pub kind: String,
    pub target: String,
}

/// Internal relationships of an OPC `.rels` part.
pub(crate) fn parse_rels(xml: &[u8]) -> Vec<Rel> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut rels = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if e.local_name().as_ref() == b"Relationship" =>
            {
                let external = attr(&e, |k| k == b"TargetMode").is_some_and(|m| m == "External");
                if let (false, Some(id), Some(kind), Some(target)) = (
                    external,
                    attr(&e, |k| k == b"Id"),
                    attr(&e, |k| k == b"Type"),
                    attr(&e, |k| k == b"Target"),
                ) {
                    rels.push(Rel { id, kind, target });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    rels
}

/// The first attribute whose (qualified) key satisfies `key`, with entity
/// references resolved (a sheet named `P&L` is stored as `P&amp;L`). A value
/// that does not unescape cleanly is returned raw rather than dropped.
pub(crate) fn attr(e: &BytesStart, key: impl Fn(&[u8]) -> bool) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| key(a.key.as_ref()))
        .map(
            |a| match a.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
                Ok(v) => v.into_owned(),
                Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
            },
        )
}

/// Resolve a relationship target against the source part's directory. A
/// leading `/` is relative to the package root.
pub(crate) fn resolve_target(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => dir.split('/').filter(|s| !s.is_empty()).collect(),
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn relationship_targets_resolve_like_opc() {
        assert_eq!(
            resolve_target("ppt", "slides/slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(
            resolve_target("ppt/slides", "../notesSlides/notesSlide1.xml"),
            "ppt/notesSlides/notesSlide1.xml"
        );
        assert_eq!(
            resolve_target("ppt/slides", "/ppt/notesSlides/n.xml"),
            "ppt/notesSlides/n.xml"
        );
        assert_eq!(
            resolve_target("ppt", "./slides/./s.xml"),
            "ppt/slides/s.xml"
        );
    }

    #[test]
    fn streaming_charges_only_what_was_read_and_flags_a_cap_hit() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("big.xml", opts).unwrap();
            w.write_all(&[b'x'; 1000]).unwrap();
            w.finish().unwrap();
        }
        let mut z = zip::ZipArchive::new(buf).unwrap();

        // The budget is charged for bytes pulled through the reader. Its
        // buffer pulls the whole of this small part on the first read.
        let mut b = Budget::new(10_000, 10_000);
        let got = b.stream(&mut z, "big.xml", |r| {
            let mut first = [0u8; 10];
            r.read_exact(&mut first).unwrap();
            first.len()
        });
        assert_eq!(got, Some(10));
        assert_eq!(b.left, 9_000);
        assert!(!b.exhausted);

        // A whole read over the per-part cap is cut there and flagged.
        let mut b = Budget::new(100, 10_000);
        assert_eq!(b.read(&mut z, "big.xml").map(|v| v.len()), Some(100));
        assert!(b.exhausted);

        // A missing part costs nothing.
        let mut b = Budget::new(100, 10_000);
        assert!(b.read(&mut z, "nope.xml").is_none());
        assert_eq!(b.left, 10_000);
    }
}
