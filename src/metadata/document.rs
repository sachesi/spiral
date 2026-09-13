//! What a PDF, an office document or an e-book says: its pages, title and author, read
//! from the parts of the file that hold them.

use super::*;

/// What `pdfinfo` from poppler, run here inside the sandbox, says of a PDF. Without it, as
/// for the preview, nothing.
pub(super) fn probe_pdf(path: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let Ok(ran) = std::process::Command::new("pdfinfo")
        .args(["-enc", "UTF-8"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return out;
    };
    if !ran.status.success() {
        return out;
    }
    for line in String::from_utf8_lossy(&ran.stdout).lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "Title" => out.push(("title", value.to_string())),
            "Author" => out.push(("author", value.to_string())),
            "Pages" => out.push(("pages", value.to_string())),
            // "595.276 x 841.89 pts (A4)"
            "Page size" => {
                let mut words = value.split_whitespace();
                if let (Some(w), Some("x"), Some(h)) = (words.next(), words.next(), words.next()) {
                    out.push(("page_width", w.to_string()));
                    out.push(("page_height", h.to_string()));
                }
                if let Some(open) = value.find('(')
                    && let Some(close) = value.rfind(')')
                    && open < close
                {
                    out.push(("paper", value[open + 1..close].to_string()));
                }
            }
            _ => {}
        }
    }
    out
}

/// The title, the author and the counts an office document or an e-book keeps in the zip
/// it is: `meta.xml` in OpenDocument, `docProps/` in Office Open XML, the package file an
/// EPUB points to.
pub(super) fn probe_document(path: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let Some(mut zip) = Zip::open(path) else {
        return out;
    };
    let mut title = None;
    let mut author = None;
    let mut pages = None;
    let mut words = None;
    let mut slides = None;
    if let Some(meta) = zip.read("meta.xml") {
        title = element(&meta, "title");
        author = element(&meta, "initial-creator").or_else(|| element(&meta, "creator"));
        pages = attribute(&meta, "document-statistic", "page-count");
        words = attribute(&meta, "document-statistic", "word-count");
    } else if let Some(core) = zip.read("docProps/core.xml") {
        title = element(&core, "title");
        author = element(&core, "creator");
        if let Some(app) = zip.read("docProps/app.xml") {
            pages = element(&app, "Pages");
            words = element(&app, "Words");
            slides = element(&app, "Slides");
        }
    } else if let Some(package) = zip
        .read("META-INF/container.xml")
        .and_then(|c| attribute(&c, "rootfile", "full-path"))
        .and_then(|name| zip.read(&name))
    {
        title = element(&package, "title");
        author = element(&package, "creator");
    }
    for (key, value) in [
        ("title", title),
        ("author", author),
        ("pages", pages),
        ("words", words),
        ("slides", slides),
    ] {
        if let Some(value) = value {
            out.push((key, value));
        }
    }
    out
}

/// Largest member of a zip that is read, and largest central directory.
pub(super) const ZIP_MEMBER_MAX: usize = 4 * 1024 * 1024;

/// Just enough of a zip to read a few small members by name: the central directory at the
/// end, and stored or deflated members.
pub(super) struct Zip {
    file: std::fs::File,
    /// Name, compression method, compressed size, size, where its local header is.
    entries: Vec<(String, u16, u64, u64, u64)>,
}

impl Zip {
    fn open(path: &Path) -> Option<Self> {
        use std::io::{Read, Seek, SeekFrom};
        let le16 =
            |b: &[u8], at: usize| Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?));
        let le32 =
            |b: &[u8], at: usize| Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
        let mut file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        // The end record, after which only a comment of at most 64 KiB can come.
        let tail_len = len.min(22 + 65_535);
        file.seek(SeekFrom::Start(len - tail_len)).ok()?;
        let mut tail = vec![0; tail_len as usize];
        file.read_exact(&mut tail).ok()?;
        let end = tail.windows(4).rposition(|w| w == b"PK\x05\x06")?;
        let size = le32(&tail, end + 12)? as usize;
        let offset = u64::from(le32(&tail, end + 16)?);
        if size > ZIP_MEMBER_MAX {
            return None;
        }
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut dir = vec![0; size];
        file.read_exact(&mut dir).ok()?;
        let mut entries = Vec::new();
        let mut at = 0;
        while dir.get(at..at + 4) == Some(b"PK\x01\x02") {
            let method = le16(&dir, at + 10)?;
            let compressed = u64::from(le32(&dir, at + 20)?);
            let size = u64::from(le32(&dir, at + 24)?);
            let name_len = le16(&dir, at + 28)? as usize;
            let extra_len = le16(&dir, at + 30)? as usize;
            let comment_len = le16(&dir, at + 32)? as usize;
            let local = u64::from(le32(&dir, at + 42)?);
            let name = String::from_utf8_lossy(dir.get(at + 46..at + 46 + name_len)?).into_owned();
            entries.push((name, method, compressed, size, local));
            at += 46 + name_len + extra_len + comment_len;
        }
        Some(Self { file, entries })
    }

    fn read(&mut self, name: &str) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let &(_, method, compressed, size, local) =
            self.entries.iter().find(|(n, ..)| n == name)?;
        if size as usize > ZIP_MEMBER_MAX || compressed as usize > ZIP_MEMBER_MAX {
            return None;
        }
        self.file.seek(SeekFrom::Start(local)).ok()?;
        let mut header = [0u8; 30];
        self.file.read_exact(&mut header).ok()?;
        if &header[..4] != b"PK\x03\x04" {
            return None;
        }
        let skip = u16::from_le_bytes([header[26], header[27]]) as i64
            + u16::from_le_bytes([header[28], header[29]]) as i64;
        self.file.seek(SeekFrom::Current(skip)).ok()?;
        let mut data = vec![0; compressed as usize];
        self.file.read_exact(&mut data).ok()?;
        let data = match method {
            0 => data,
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(&data, ZIP_MEMBER_MAX).ok()?,
            _ => return None,
        };
        Some(String::from_utf8_lossy(&data).into_owned())
    }
}

/// The text of the first element called `name` in `xml`, whatever its namespace prefix.
pub(super) fn element(xml: &str, name: &str) -> Option<String> {
    let (tag, rest) = open_tag(xml, name)?;
    if tag.ends_with('/') {
        return None;
    }
    let text = rest.trim_start();
    if let Some(cdata) = text.strip_prefix("<![CDATA[") {
        let text = &cdata[..cdata.find("]]>")?];
        return Some(text.trim().to_string()).filter(|t| !t.is_empty());
    }
    let text = &text[..text.find("</")?];
    Some(unescape(text.trim())).filter(|t| !t.is_empty())
}

/// The value of the attribute `attr` of the first element called `name`, prefixes aside.
pub(super) fn attribute(xml: &str, name: &str, attr: &str) -> Option<String> {
    let (tag, _) = open_tag(xml, name)?;
    let mut rest = tag;
    while let Some(at) = rest.find(attr) {
        let before = rest[..at].chars().next_back();
        let after = &rest[at + attr.len()..];
        if matches!(
            before,
            Some(':') | Some(' ') | Some('\t') | Some('\n') | Some('\r')
        ) && let Some(quote) = after.strip_prefix('=').and_then(|a| a.chars().next())
            && (quote == '"' || quote == '\'')
        {
            let value = &after[2..];
            return Some(unescape(&value[..value.find(quote)?])).filter(|v| !v.is_empty());
        }
        rest = after;
    }
    None
}

/// The inside of the first opening tag called `name`, and what follows it.
pub(super) fn open_tag<'a>(xml: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let mut rest = xml;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let end = rest.find(|c: char| c == '>' || c == '/' || c.is_whitespace())?;
        let tag_name = &rest[..end];
        if tag_name.rsplit(':').next() == Some(name) {
            let close = rest.find('>')?;
            return Some((&rest[..close], &rest[close + 1..]));
        }
    }
    None
}

pub(super) fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
