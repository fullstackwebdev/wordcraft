//! `w:altChunk`: the embedded document a Word package can hold (#4374).
//!
//! Word shows the content of the part a `w:altChunk` names where the element stands, and only then
//! turns it into the document's own content. Producers that build their file from HTML write the
//! whole page that way: html-docx-js, which Zotero's Better Notes exports with, writes nothing but
//! the chunk — so a reader that skips it opens the file blank.
//!
//! The `docx` reader cannot read HTML (it has no importer, and the two crates are siblings, so it
//! may not depend on `formats`): it reports the chunks it met, with the block each one stood
//! before, and the import happens here, where both crates are in reach.

use wordcraft_doc::{Blocks, Document, SectionProps};

/// Put each chunk's content where it stood in `doc`. A chunk whose format WordCraft can't read is
/// left out (logged): its content is lost either way, and the rest of the document still opens.
pub(super) fn insert(doc: &mut Document, chunks: Vec<wordcraft_docx::AltChunk>) {
    // The page the chunk's tables size to: the document's own, since that is where they are laid
    // out. Cloned up front because the body is spliced below.
    let page = doc.last_section.clone();
    // GUARDRAIL: fill the places from the last chunk backwards — each `at` counts the blocks as
    // the reader produced them, and every insertion moves all later blocks, so filling the first
    // place first leaves the second chunk's content one chunk too far down (#4374).
    for chunk in chunks.into_iter().rev() {
        let Some(blocks) = read(&chunk, &page) else {
            log::warn!("docx: an embedded {:?} document (block {}) is left out: WordCraft can't read that format", chunk.content_type, chunk.at);
            continue;
        };
        let at = chunk.at.min(doc.body.len());
        doc.body.splice(at..at, blocks);
    }
}

/// The blocks of the page a chunk holds: its HTML, or the page inside its MIME HTML body. `None`
/// for a chunk whose format these two paths don't cover.
fn read(chunk: &wordcraft_docx::AltChunk, page: &SectionProps) -> Option<Blocks> {
    let html = match media_type(&chunk.content_type).as_str() {
        "text/html" | "application/xhtml+xml" => wordcraft_formats::html::decode(&chunk.bytes),
        // GUARDRAIL: html-docx-js declares the part `message/rfc822` while its body is a
        // `multipart/related` — dispatching on the parts' own Content-Type rather than the package's
        // content type alone is what finds the page (#4374); the reverse (a `multipart/*` declared
        // part) is accepted the same way.
        "message/rfc822" | "multipart/related" | "multipart/alternative" => wordcraft_formats::mht::html(&chunk.bytes)?,
        _ => return None,
    };
    // A page's pictures are `data:` URIs in the HTML or parts of the MIME body; only the former
    // come with the page (see `formats::mht`), so no image loader is needed.
    Some(wordcraft_formats::html::import_on(&html, &|_| None, page).body)
}

/// The media type of a content type (`text/html; charset=utf-8` → `text/html`), lowercased.
fn media_type(content_type: &str) -> String {
    content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page part's blocks come out of the chunk, in its place, for both shapes producers write.
    #[test]
    fn html_and_mime_html_chunks_become_blocks() {
        let page = SectionProps::default();
        let html = wordcraft_docx::AltChunk {
            at: 0,
            content_type: "text/html".into(),
            bytes: std::sync::Arc::new(b"<html><body><p>from the chunk</p></body></html>".to_vec()),
        };
        let mht = wordcraft_docx::AltChunk {
            at: 0,
            content_type: "message/rfc822".into(),
            bytes: std::sync::Arc::new(
                b"Content-Type: multipart/related; boundary=b\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>from the MIME body</p>\r\n--b--\r\n"
                    .to_vec(),
            ),
        };
        let text = |chunk: &wordcraft_docx::AltChunk| {
            let blocks = read(chunk, &page).expect("the page");
            blocks.iter().filter_map(|b| b.as_para().map(|p| p.plain_text())).collect::<Vec<_>>().join("\n")
        };
        assert_eq!(text(&html), "from the chunk");
        assert_eq!(text(&mht), "from the MIME body");
        // A format these two paths don't cover is not guessed at.
        let rtf = wordcraft_docx::AltChunk { at: 0, content_type: "application/rtf".into(), bytes: std::sync::Arc::new(b"{\\rtf1 x}".to_vec()) };
        assert!(read(&rtf, &page).is_none(), "an RTF chunk is left out, not read as text");
    }
}
