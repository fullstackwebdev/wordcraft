//! MIME HTML (`.mht`, `.mhtml`): a whole web page packed into one mail-style MIME body
//! (RFC 2557: the page in the `text/html` part of a `multipart/related`, its pictures in the
//! parts around it).
//!
//! html-docx-js — what Zotero's Better Notes exports with — gives a Word package all of its
//! content as one of these, behind a `w:altChunk` (#4374), so the page has to be taken out of the
//! MIME body before the HTML importer can read it.
//!
//! Only the page comes out: the pictures stay behind, so a `cid:` or `file:` source in the HTML
//! resolves to nothing.

use std::borrow::Cow;

use crate::model::base64_decode;

/// Most MIME entities looked at in one file, and the deepest `multipart/*` nesting followed.
const MAX_PARTS: usize = 256;
const MAX_DEPTH: u8 = 4;
/// Most bytes of headers before a body with no empty line to end them is refused as not MIME.
const MAX_HEADERS: usize = 64 * 1024;

/// The HTML page inside an MHT file: the first `text/html` part of its MIME body,
/// transfer-decoded (quoted-printable or base64) and converted from the charset it names.
/// `None` when the file is not a MIME body or holds no HTML part.
pub fn html(bytes: &[u8]) -> Option<String> {
    let mut budget = MAX_PARTS;
    page(bytes, 0, &mut budget)
}

fn page(bytes: &[u8], depth: u8, budget: &mut usize) -> Option<String> {
    *budget = budget.checked_sub(1)?;
    let (headers, body) = split_message(bytes)?;
    let content_type = header(&headers, "content-type").unwrap_or_default();
    match media_type(&content_type).as_str() {
        "text/html" | "application/xhtml+xml" => {
            let transfer = header(&headers, "content-transfer-encoding").unwrap_or_default();
            let decoded = transfer_decode(&transfer, body);
            let charset = param(&content_type, "charset");
            Some(charset.as_deref().and_then(|cs| crate::txt::decode_as(&decoded, cs)).unwrap_or_else(|| crate::txt::decode(&decoded)))
        }
        // The page sits beside its pictures in a multipart body, whatever else that body is
        // typed as (`multipart/related`, `multipart/alternative`, `multipart/mixed`).
        kind if kind.starts_with("multipart/") => {
            if depth >= MAX_DEPTH {
                return None;
            }
            let boundary = param(&content_type, "boundary")?;
            split_multipart(body, &boundary).into_iter().find_map(|part| page(part, depth + 1, budget))
        }
        _ => None,
    }
}

/// Split a MIME entity at the empty line that ends its headers. A body with no headers at all is
/// not a MIME entity: an MHT file always starts with them.
fn split_message(bytes: &[u8]) -> Option<(Cow<'_, str>, &[u8])> {
    let mut at = 0usize;
    loop {
        let rest = bytes.get(at..)?;
        let nl = rest.iter().position(|&c| c == b'\n')?;
        let line = rest.get(..nl).unwrap_or_default();
        at += nl + 1;
        if line.is_empty() || line == b"\r" {
            return Some((String::from_utf8_lossy(bytes.get(..at)?), bytes.get(at..)?));
        }
        if at > MAX_HEADERS {
            return None;
        }
    }
}

/// The value of header `name` (case-insensitive), its folded continuation lines unfolded
/// (RFC 5322 §2.2.3 drops the line break and keeps the leading white space). `None` when absent.
fn header(headers: &str, name: &str) -> Option<String> {
    let mut out: Option<String> = None;
    let mut collecting = false;
    for line in headers.lines() {
        if line.starts_with([' ', '\t']) {
            if collecting && let Some(v) = out.as_mut() {
                v.push_str(line);
            }
            continue;
        }
        collecting = false;
        let Some((field, value)) = line.split_once(':') else { continue };
        if field.trim().eq_ignore_ascii_case(name) && out.is_none() {
            collecting = true;
            out = Some(value.trim_start().to_string());
        }
    }
    out
}

/// The media type of a `Content-Type` value, lowercased and without its parameters.
fn media_type(value: &str) -> String {
    value.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

/// A parameter of a `Content-Type` value (`charset="utf-8"` → `utf-8`), unquoted. Values are
/// case-sensitive (a MIME boundary is), and lookups of them are case-insensitive on their own.
// GUARDRAIL: a MIME boundary is case-sensitive while `charset` is not — lowercasing every value
// here made each delimiter fail to match its `Content-Type: multipart/...; boundary=...` and the
// page was never found (#4374) — return the value as written, and case-fold at the lookups that
// need it (`charset`, `Content-Transfer-Encoding`).
fn param(value: &str, name: &str) -> Option<String> {
    for p in value.split(';').skip(1) {
        let Some((key, v)) = p.split_once('=') else { continue };
        if !key.trim().eq_ignore_ascii_case(name) {
            continue;
        }
        let v = v.trim().trim_matches('"');
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

/// Body bytes as their `Content-Transfer-Encoding` says (RFC 2045 §6.1); an encoding we don't
/// know leaves the bytes alone.
fn transfer_decode(encoding: &str, body: &[u8]) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "quoted-printable" => quoted_printable(body),
        "base64" => base64_decode(&String::from_utf8_lossy(body)).unwrap_or_else(|| body.to_vec()),
        _ => body.to_vec(),
    }
}

/// Quoted-printable (RFC 2045 §6.7): `=XX` is one byte in hex, and `=` at the end of a line is a
/// soft break that belongs to the encoding, not the text. Anything else after `=` is left as it
/// was written.
// GUARDRAIL: the soft break joins the words it split, with nothing in between — html-docx-js wraps
// its page at 76 characters, so keeping the break (or adding a space for it) splits words and
// inserts spaces into the imported text (#4374) — drop the break entirely.
fn quoted_printable(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0usize;
    while let Some(&c) = body.get(i) {
        if c != b'=' {
            out.push(c);
            i += 1;
            continue;
        }
        match body.get(i + 1) {
            Some(b'\n') => i += 2,
            Some(b'\r') if body.get(i + 2) == Some(&b'\n') => i += 3,
            Some(hi) if body.get(i + 2).is_some() => {
                let (hi, lo) = (*hi, body.get(i + 2).copied().unwrap_or(b'0'));
                match (hex(hi), hex(lo)) {
                    (Some(h), Some(l)) => {
                        out.push(h.wrapping_mul(16).wrapping_add(l));
                        i += 3;
                    }
                    _ => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// The parts of a `multipart/*` body: each runs from the end of a `--boundary` line to the start
/// of the next one; a `--boundary--` line closes the body. A body without boundary lines has no
/// parts. The bound is [`MAX_PARTS`] here even when nesting is shallow, so a body made of nothing
/// but delimiter lines costs no more than one made of parts.
fn split_multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let delimiter = format!("--{boundary}");
    let delimiter = delimiter.as_bytes();
    let mut marks: Vec<usize> = Vec::new();
    // Where the closing `--boundary--` line starts: the last part ends there, so its line break
    // and the delimiter are not part of the page.
    let mut close = None;
    for (at, line) in lines(body) {
        if !line.starts_with(delimiter) {
            continue;
        }
        if marks.len() >= MAX_PARTS {
            break;
        }
        // `--boundary--` ends the body: nothing after it is a part.
        if line.get(delimiter.len()..).is_some_and(|rest| rest.starts_with(b"--")) {
            close = Some(at);
            break;
        }
        marks.push(at);
    }
    let mut parts = Vec::with_capacity(marks.len());
    for (i, at) in marks.iter().enumerate() {
        let Some(head) = body.get(*at..) else { continue };
        // The part starts after the delimiter line, and ends where the next delimiter line
        // starts (the closing one ends the last part).
        let Some(nl) = head.iter().position(|&c| c == b'\n') else { continue };
        let begin = at.saturating_add(nl).saturating_add(1);
        let end = marks.get(i + 1).copied().or(close).unwrap_or(body.len());
        if let Some(part) = body.get(begin..end) {
            // GUARDRAIL: the line break before a delimiter belongs to the delimiter (RFC 2046
            // §5.1.1), not to the part — leaving it in gave the imported page a trailing line
            // break it never had (#4374) — strip it.
            parts.push(part.strip_suffix(b"\r\n").or_else(|| part.strip_suffix(b"\n")).unwrap_or(part));
        }
    }
    parts
}

/// `body` line by line, as (byte offset of the line, the line without its line break).
fn lines(body: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let rest = body.get(at..)?;
        if rest.is_empty() {
            return None;
        }
        let len = rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len());
        let line = rest.get(..len).unwrap_or_default();
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let start = at;
        at = at.saturating_add(len).saturating_add(1);
        Some((start, line))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `multipart/related` body as html-docx-js writes it: the page quoted-printable in the
    /// first part, a picture base64 in the second.
    const HTML_DOCX_JS: &str = concat!(
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/related; boundary=\"----=_NextPart_01D8\"; type=\"text/html\"\r\n",
        "\r\n",
        "------=_NextPart_01D8\r\n",
        "Content-Location: file:///C:/Temp/document.html\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n",
        "Content-Type: text/html; charset=\"utf-8\"\r\n",
        "\r\n",
        "<html><body><p>Verbeke and Lee, 2021</p><p>Zotero=20refresh</p><img src=3D\"file:///C:/Temp/image001.png\">\r\n",
        "------=_NextPart_01D8\r\n",
        "Content-Location: file:///C:/Temp/image001.png\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "Content-Type: image/png\r\n",
        "\r\n",
        "iVBORw0KGgo=\r\n",
        "------=_NextPart_01D8--\r\n",
    );

    #[test]
    fn the_page_comes_out_of_the_mime_body() {
        let page = html(HTML_DOCX_JS.as_bytes()).expect("the text/html part");
        assert!(page.contains("<p>Verbeke and Lee, 2021</p>"), "{page:?}");
        // `=20` is a space; `=3D` is the `=` of `src="`.
        assert!(page.contains("Zotero refresh"), "{page:?}");
        assert!(page.contains(r#"<img src="file:///C:/Temp/image001.png">"#), "{page:?}");
        assert!(!page.contains("iVBORw0KGgo"), "the picture part is not the page: {page:?}");
    }

    #[test]
    fn a_soft_line_break_joins_the_words_it_split() {
        // Word joins the two lines with nothing: the break is the encoding's, not the page's.
        let qp = b"a=\r\nb and c=\nd";
        assert_eq!(quoted_printable(qp), b"ab and cd");
    }

    #[test]
    fn a_part_that_is_not_html_is_skipped() {
        let body = concat!(
            "Content-Type: multipart/related; boundary=b\r\n\r\n",
            "--b\r\nContent-Type: image/png\r\n\r\npicture\r\n",
            "--b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>after the picture</p>\r\n",
            "--b--\r\n",
        );
        assert_eq!(html(body.as_bytes()).as_deref(), Some("<p>after the picture</p>"));
    }

    #[test]
    fn a_folded_content_type_is_one_header() {
        let body = concat!(
            "Content-Type: multipart/related;\r\n boundary=\"b\"\r\n",
            "\r\n",
            "--b\r\nContent-Type: text/html\r\n\r\n<p>x</p>\r\n--b--\r\n",
        );
        assert_eq!(html(body.as_bytes()).as_deref(), Some("<p>x</p>"));
    }

    #[test]
    fn base64_page_content() {
        let body = concat!(
            "Content-Type: multipart/related; boundary=b\r\n\r\n",
            "--b\r\nContent-Type: text/html\r\nContent-Transfer-Encoding: base64\r\n\r\n",
            "PHA+eDwvcD4=\r\n",
            "--b--\r\n",
        );
        assert_eq!(html(body.as_bytes()).as_deref(), Some("<p>x</p>"));
    }

    #[test]
    fn a_file_with_no_mime_headers_has_no_page() {
        assert!(html(b"<html><body><p>x</p></body></html>").is_none());
        assert!(html(b"").is_none());
    }

    #[test]
    fn a_body_of_nothing_but_delimiters_ends() {
        // Millions of delimiter lines must not build millions of parts.
        let mut body = String::from("Content-Type: multipart/related; boundary=b\r\n\r\n");
        for _ in 0..(MAX_PARTS * 4) {
            body.push_str("--b\r\n");
        }
        let parts = split_multipart(body.as_bytes(), "b");
        assert_eq!(parts.len(), MAX_PARTS.min(body.matches("--b\r\n").count()));
    }
}
