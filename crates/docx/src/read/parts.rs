use super::{
    AltChunk, Arc, Blocks, ContentTypes, DocxError, El, MAX_ALT_CHUNKS, MAX_FONT_NAME, MAX_HF_DEPTH, MAX_PARTS, MAX_TABLE_FONTS, MAX_VBA_RELATED,
    PartKind, Reader, Rels, Revision, RevisionKind, Rgb, StoryCtx, VBA_PROJECT_PART, VBA_RELATED, props, shape_style,
};

impl Reader<'_> {
    /// Collect the chunk a `w:altChunk` refers to (see [`AltChunk`]): the part its relationship
    /// names, with the content type the package declares for it, to stand where the element is
    /// when the caller can read its format. A chunk that names no part, or more than
    /// [`MAX_ALT_CHUNKS`] of them, is left out (logged).
    pub(super) fn alt_chunk(&mut self, e: &El, rels: &Rels, at: usize) {
        if !self.collect_chunks {
            return;
        }
        if self.alt_chunks.len() >= MAX_ALT_CHUNKS {
            log::warn!("docx: more than {MAX_ALT_CHUNKS} embedded documents; the rest are left out");
            return;
        }
        // GUARDRAIL: `r:id` is the relationship (Word writes it), but older producers write the same
        // id as `w:id` — reading only one of them left those files' content out with no sign that
        // anything was there (#4374) — fall back to `w:id`.
        let Some(id) = e.attr("r:id").or_else(|| e.attr("w:id")) else { return };
        let Some(rel) = rels.by_id(id).filter(|r| !r.external) else {
            log::warn!("docx: altChunk {id:?} names no part of the package; left out");
            return;
        };
        // GUARDRAIL: a chunk part is often most of the document (a whole page in an MHT) — copying
        // it here held the page twice while the file opened, as copied pictures once did (#12) —
        // share the package's bytes and only clone the `Arc`.
        let Some(bytes) = self.pkg.get_shared(&rel.target) else {
            log::warn!("docx: the altChunk part {} is missing; left out", rel.target);
            return;
        };
        let pkg = self.pkg;
        let content_type = self.content_types.get_or_insert_with(|| ContentTypes::read(pkg)).of(&rel.target).unwrap_or("").to_string();
        let bytes = Arc::clone(bytes);
        self.alt_chunks.push(AltChunk { at, content_type, bytes });
    }
    /// Keep the parts the VBA project at `project` relates to, with their relationship types and
    /// content types (see [`VBA_RELATED`]). Parts outside `word/`, relationship parts, the project
    /// itself and missing targets are skipped (logged); at most [`MAX_VBA_RELATED`] are kept.
    pub(super) fn read_vba_related(&mut self, project: &str) {
        let pkg = self.pkg;
        let types = self.content_types.get_or_insert_with(|| ContentTypes::read(pkg));
        let mut manifest = String::new();
        let mut kept = 0usize;
        for rel in self.pkg.rels(project).list.iter().filter(|r| !r.external) {
            // Part names compare case-insensitively (OPC): `/WORD/x` names the part the writer
            // stores as `word/x`.
            let target = rel.target.as_str();
            let path = match (target.get(..5), target.get(5..)) {
                (Some(dir), Some(rest)) if dir.eq_ignore_ascii_case("word/") => format!("word/{rest}"),
                _ => target.to_string(),
            };
            let path = path.as_str();
            let ct = types.of(path).unwrap_or("application/octet-stream");
            let usable = path.strip_prefix("word/").is_some_and(|t| !t.is_empty())
                && !path.to_ascii_lowercase().ends_with(".rels")
                && !path.eq_ignore_ascii_case(project)
                && !path.eq_ignore_ascii_case(VBA_PROJECT_PART)
                && ![rel.kind.as_str(), path, ct].iter().any(|f| f.is_empty() || f.contains(['\t', '\r', '\n']));
            if !usable {
                log::warn!("docx: not carrying VBA-related part {path:?}");
                continue;
            }
            let Some(bytes) = self.pkg.get_shared(path) else {
                log::warn!("docx: VBA-related part {path} is missing; keeping the project without it");
                continue;
            };
            if kept >= MAX_VBA_RELATED {
                log::warn!("docx: more than {MAX_VBA_RELATED} VBA-related parts; the rest are dropped");
                break;
            }
            kept += 1;
            manifest.push_str(&format!("{}\t{path}\t{ct}\n", rel.kind));
            self.doc.passthrough.entry(path.to_string()).or_insert_with(|| bytes.clone());
        }
        if !manifest.is_empty() {
            self.doc.passthrough.insert(VBA_RELATED.into(), Arc::new(manifest.into_bytes()));
        }
    }

    /// The fonts the font table classes as roman (`w:font/w:family w:val="roman"`).
    pub(super) fn read_font_families(&mut self, path: &str) {
        let Ok(Some(table)) = self.xml(path) else { return };
        self.doc.settings.roman_fonts = table
            .children("w:font")
            .filter(|f| f.child("w:family").and_then(|v| v.attr("w:val")) == Some("roman"))
            .filter_map(|f| f.attr("w:name"))
            .filter(|n| !n.is_empty() && n.len() <= MAX_FONT_NAME)
            .take(MAX_TABLE_FONTS)
            .map(str::to_string)
            .collect();
    }

    pub(super) fn xml(&self, path: &str) -> Result<Option<El>, DocxError> {
        self.pkg.xml(path)
    }

    pub fn revision(&mut self, kind: RevisionKind, e: &El) -> u32 {
        self.revision_named(kind, e, None)
    }

    pub(super) fn revision_named(&mut self, kind: RevisionKind, e: &El, move_name: Option<String>) -> u32 {
        let author = e.attr("w:author").unwrap_or("").chars().take(256).collect::<String>();
        let date = e.attr("w:date").unwrap_or("").chars().take(64).collect::<String>();
        let k = match kind {
            RevisionKind::Insert => 0,
            RevisionKind::Delete => 1,
            RevisionKind::Format => 2,
        };
        let key = (k, author.clone(), date.clone(), move_name.clone());
        if let Some(i) = self.revs.get(&key) {
            return *i;
        }
        let i = self.doc.revisions.len() as u32;
        self.doc.revisions.push(Revision { kind, author, date, move_name });
        self.revs.insert(key, i);
        i
    }

    /// Media key for an image relationship (loads the bytes once).
    pub fn media_for(&mut self, rels: &Rels, id: &str) -> Option<String> {
        let rel = rels.by_id(id)?;
        if rel.external {
            return None;
        }
        if let Some(k) = self.media_by_path.get(&rel.target) {
            return Some(k.clone());
        }
        let bytes = self.pkg.get_shared(&rel.target)?;
        let base: String =
            rel.target.rsplit('/').next().unwrap_or("image").chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).collect();
        let base = if base.is_empty() || base.starts_with('.') { format!("image{base}") } else { base };
        let mut key = base.clone();
        let mut n = 1;
        while self.doc.media.contains_key(&key) {
            n += 1;
            key = match base.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}_{n}.{ext}"),
                None => format!("{base}_{n}"),
            };
        }
        self.doc.media.insert(key.clone(), bytes.clone());
        self.media_by_path.insert(rel.target.clone(), key.clone());
        Some(key)
    }

    /// A chart or diagram part, parsed once however often it's referenced (`None` when unreadable).
    pub(super) fn graphic_part(&mut self, path: &str) -> Option<Arc<El>> {
        if let Some(p) = self.graphic_parts.get(path) {
            return p.clone();
        }
        let p = self.xml(path).ok().flatten().map(Arc::new);
        self.graphic_parts.insert(path.to_string(), p.clone());
        p
    }

    pub fn load_header_footer(&mut self, path: &str, footer: bool) -> Option<u32> {
        if let Some(id) = self.hf_by_path.get(path) {
            return Some(*id);
        }
        if self.doc.parts.len() >= MAX_PARTS || self.hf_depth >= MAX_HF_DEPTH {
            return None;
        }
        // Reserve the id first so a (malformed) self-reference can't recurse.
        let kind = if footer { PartKind::Footer } else { PartKind::Header };
        let id = self.doc.add_part(kind, Blocks::new());
        self.hf_by_path.insert(path.to_string(), id);
        let root = match self.xml(path) {
            Ok(Some(r)) => r,
            _ => return Some(id),
        };
        let rels = self.pkg.rels(path);
        let mut sc = StoryCtx::default();
        sc.in_hf = true;
        let mut blocks = Blocks::new();
        self.hf_depth += 1;
        self.read_blocks(&mut sc, &root, &rels, &mut blocks, 0);
        self.hf_depth = self.hf_depth.saturating_sub(1);
        self.flush_pending(&mut sc, &mut blocks);
        if let Some(p) = self.doc.parts.get_mut(&id)
            && !blocks.is_empty()
        {
            p.blocks = blocks;
        }
        Some(id)
    }

    pub(super) fn read_theme(&mut self, path: &str) {
        let Ok(Some(t)) = self.xml(path) else { return };
        if let Some(n) = t.attr("name").filter(|n| !n.is_empty()) {
            self.doc.settings.theme_name = n.to_string();
        }
        let Some(el) = t.child("a:themeElements") else { return };
        if let Some(cs) = el.child("a:clrScheme") {
            let names = ["dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"];
            let mut colors = self.doc.settings.theme_colors.clone();
            colors.resize(12, Rgb::BLACK);
            for (i, n) in names.iter().enumerate() {
                let Some(c) = cs.child(&format!("a:{n}")) else { continue };
                let rgb = c
                    .child("a:srgbClr")
                    .and_then(|x| x.attr("val"))
                    .and_then(Rgb::parse)
                    .or_else(|| c.child("a:sysClr").and_then(|x| x.attr("lastClr")).and_then(Rgb::parse));
                if let (Some(rgb), Some(slot)) = (rgb, colors.get_mut(i)) {
                    *slot = rgb;
                }
            }
            self.pc.theme_colors = colors.clone();
            self.doc.settings.theme_colors = colors;
        }
        self.theme_fills = shape_style::ThemeFills::read(el.child("a:fmtScheme"));
        if let Some(fs) = el.child("a:fontScheme") {
            let face =
                |n: &str| fs.child(n).and_then(|f| f.child("a:latin")).and_then(|l| l.attr("typeface")).filter(|t| !t.is_empty()).map(str::to_string);
            if let Some(f) = face("a:majorFont") {
                self.doc.settings.major_font = f;
            }
            if let Some(f) = face("a:minorFont") {
                self.doc.settings.minor_font = f;
            }
            let cs = |n: &str| fs.child(n).and_then(|f| f.child("a:cs")).and_then(|c| c.attr("typeface")).unwrap_or_default().to_string();
            self.pc.major_cs = cs("a:majorFont");
            self.pc.minor_cs = cs("a:minorFont");
            // Per-script fonts (`a:font script="Arab"`); Office's themes list about 30.
            let scripts = |n: &str| -> Vec<(String, String)> {
                let Some(f) = fs.child(n) else { return Vec::new() };
                f.els()
                    .filter(|e| e.name == "a:font")
                    .filter_map(|e| Some((e.attr("script")?, e.attr("typeface")?)))
                    .filter(|(s, t)| s.len() <= 8 && !t.is_empty() && t.len() < 256)
                    .take(256)
                    .map(|(s, t)| (s.to_string(), t.to_string()))
                    .collect()
            };
            self.pc.major_scripts = scripts("a:majorFont");
            self.pc.minor_scripts = scripts("a:minorFont");
        }
        self.pc.major_font = self.doc.settings.major_font.clone();
        self.pc.minor_font = self.doc.settings.minor_font.clone();
        self.pc.theme = self.doc.settings.theme_colors.clone();
    }

    /// `w:settings/w:clrSchemeMapping`: which scheme colour text1, background1… stand for. Also
    /// `w:themeFontLang/@w:bidi`, the language whose script picks the `…Bidi` theme fonts.
    pub(super) fn read_color_mapping(&mut self, path: &str) {
        let Ok(Some(root)) = self.xml(path) else { return };
        self.pc.bidi_script = root.child("w:themeFontLang").and_then(|l| l.attr("w:bidi")).filter(|v| !v.trim().is_empty()).map(props::lang_script);
        let Some(m) = root.child("w:clrSchemeMapping") else { return };
        let names = [
            "dark1",
            "light1",
            "dark2",
            "light2",
            "accent1",
            "accent2",
            "accent3",
            "accent4",
            "accent5",
            "accent6",
            "hyperlink",
            "followedHyperlink",
        ];
        let attrs = [
            "w:t1",
            "w:bg1",
            "w:t2",
            "w:bg2",
            "w:accent1",
            "w:accent2",
            "w:accent3",
            "w:accent4",
            "w:accent5",
            "w:accent6",
            "w:hyperlink",
            "w:followedHyperlink",
        ];
        let mut map = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        for (slot, a) in map.iter_mut().zip(attrs) {
            if let Some(i) = m.attr(a).and_then(|v| names.iter().position(|n| *n == v)) {
                *slot = i;
            }
        }
        self.pc.color_map = Some(map);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    const W_NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
    const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    /// The page html-docx-js puts in the chunk part, as Zotero's Better Notes exports it.
    const PAGE: &str = "<html><body><p>Verbeke and Lee, 2021</p></body></html>";

    /// A package shaped like a Better Notes export: the body holds `body`, and `htmlChunk` names
    /// `word/afchunk.html`, declared `text/html`.
    fn export(body: &str) -> Vec<u8> {
        let content_types = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
            r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
            r#"<Default Extension="xml" ContentType="application/xml"/>"#,
            r#"<Default Extension="html" ContentType="text/html"/>"#,
            r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#,
            r#"<Override PartName="/word/afchunk.html" ContentType="text/html"/></Types>"#,
        );
        let rels = concat!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="htmlChunk" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/aFChunk" Target="/word/afchunk.html"/>"#,
            r#"</Relationships>"#,
        );
        let doc = format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W_NS}><w:body>{body}</w:body></w:document>"#);
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, bytes) in [
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", ROOT_RELS.as_bytes()),
            ("word/document.xml", doc.as_bytes()),
            ("word/_rels/document.xml.rels", rels.as_bytes()),
            ("word/afchunk.html", PAGE.as_bytes()),
        ] {
            zw.start_file(name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    /// The reported defect: a Better Notes export is one `w:altChunk` and nothing else, so the
    /// body as read has no content at all and the file looks empty (#4374). The chunk is what
    /// holds the page, with the place it takes.
    #[test]
    fn an_exported_page_is_one_chunk_and_no_blocks() {
        let bytes = export(r#"<w:altChunk r:id="htmlChunk"/><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>"#);
        let (doc, chunks) = crate::read_with_chunks(&bytes, None).unwrap();
        assert!(doc.plain_text(wordcraft_doc::StoryRef::Body).trim().is_empty(), "the reader has no blocks to show");
        assert_eq!(chunks.len(), 1, "the page is in the chunk");
        let chunk = &chunks[0];
        assert_eq!(chunk.at, 0);
        assert_eq!(chunk.content_type, "text/html");
        assert_eq!(chunk.bytes.as_slice(), PAGE.as_bytes());
        assert_eq!(doc.last_section.page_w, 612.0, "the section the file gives is read as before");
    }

    /// The chunk stands where it was written, in the body as read: the blocks around it are
    /// counted, so a producer that wraps a page in its own paragraphs keeps them.
    #[test]
    fn a_chunk_between_paragraphs_keeps_its_place() {
        let bytes = export(r#"<w:p><w:r><w:t>before</w:t></w:r></w:p><w:altChunk r:id="htmlChunk"/><w:p><w:r><w:t>after</w:t></w:r></w:p>"#);
        let (doc, chunks) = crate::read_with_chunks(&bytes, None).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].at, 1, "between the two paragraphs");
        assert_eq!(doc.body.len(), 2);
    }

    /// A plain read is unchanged: no chunk part is materialised, so a document that carries a
    /// whole page in one does not pay for it when nobody can read it.
    #[test]
    fn a_plain_read_collects_nothing() {
        let bytes = export(r#"<w:p><w:r><w:t>before</w:t></w:r></w:p><w:altChunk r:id="htmlChunk"/>"#);
        let doc = crate::read(&bytes).unwrap();
        assert_eq!(doc.plain_text(wordcraft_doc::StoryRef::Body).trim(), "before");
    }

    /// A chunk that names no part, or a part the package lacks, is left out rather than failing
    /// the whole document: the rest of the file still opens.
    #[test]
    fn a_chunk_with_no_part_is_left_out() {
        let bytes = export(r#"<w:p><w:r><w:t>kept</w:t></w:r></w:p><w:altChunk r:id="missing"/>"#);
        let (doc, chunks) = crate::read_with_chunks(&bytes, None).unwrap();
        assert!(chunks.is_empty(), "{chunks:?}");
        assert_eq!(doc.plain_text(wordcraft_doc::StoryRef::Body).trim(), "kept");
    }
}
