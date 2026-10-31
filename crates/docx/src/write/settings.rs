use super::parts::push_part;
use super::{CT_WML, Document, PartRels, W, Writer, math, rt, story, xml};

/// The file id of the first footnote or endnote: after the separators (-1, 0) and the
/// continuation notice (1), when the document has one.
pub(super) fn note_id_base(doc: &Document, foot: bool) -> usize {
    let seps = if foot { &doc.footnote_separators } else { &doc.endnote_separators };
    if seps.continuation_notice.is_some() { 2 } else { 1 }
}

/// Whether the footnotes (or endnotes) part is written: there are notes, or separators of the
/// document's own to keep.
pub(super) fn notes_part(doc: &Document, wr: &Writer<'_>, foot: bool) -> bool {
    let (ids, seps) = if foot { (&wr.footnotes, &doc.footnote_separators) } else { (&wr.endnotes, &doc.endnote_separators) };
    !ids.is_empty() || !seps.is_empty()
}

/// `w:settings`, its children in the schema's order (CT_Settings is a sequence). `template_rel`
/// is the id of the attached template's relationship, when there is one.
pub(super) fn settings_xml(doc: &Document, footnotes: bool, endnotes: bool, template_rel: Option<&str>) -> Vec<u8> {
    let s = &doc.settings;
    let s_notes = (&doc.footnote_separators, &doc.endnote_separators);
    let mut w = W::new();
    let ns = xml::body_ns();
    let ns_refs: Vec<(&str, &str)> = ns.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    w.open("w:settings", &ns_refs);
    let zoom = s.zoom.unwrap_or(wordcraft_doc::Zoom { percent: 100, fit: None });
    let percent = zoom.percent.clamp(10, 500).to_string();
    match zoom.fit {
        Some(fit) => {
            let val = match fit {
                wordcraft_doc::ZoomFit::OnePage => "fullPage",
                wordcraft_doc::ZoomFit::PageWidth => "bestFit",
                wordcraft_doc::ZoomFit::TextWidth => "textFit",
            };
            w.empty("w:zoom", &[("w:val", val), ("w:percent", &percent)]);
        }
        None => w.empty("w:zoom", &[("w:percent", &percent)]),
    }
    // §17.15.1.70 / §17.15.1.69, right after `w:zoom` in CT_Settings.
    if s.remove_personal_info {
        w.empty("w:removePersonalInformation", &[]);
    }
    if s.remove_date_time {
        w.empty("w:removeDateAndTime", &[]);
    }
    if s.page_color.is_some() || s.page_picture.is_some() {
        w.empty("w:displayBackgroundShape", &[]);
    }
    if s.mirror_margins {
        w.empty("w:mirrorMargins", &[]);
    }
    if let Some(id) = template_rel {
        w.empty("w:attachedTemplate", &[("r:id", id)]);
    }
    if s.link_styles {
        w.empty("w:linkStyles", &[]);
    }
    if s.track_changes {
        w.empty("w:trackRevisions", &[]);
    }
    if !s.track_moves {
        w.empty("w:doNotTrackMoves", &[]);
    }
    if let Some(p) = &s.protection {
        let mut a: Vec<(&str, &str)> = Vec::new();
        if p != "none" {
            a.push(("w:edit", p));
        }
        if s.protect_formatting {
            a.push(("w:formatting", "1"));
        }
        a.push(("w:enforcement", "1"));
        // Only the hash attributes the reader keeps, each once.
        for name in crate::read::PROTECTION_HASH_ATTRS {
            if let Some((_, v)) = s.protect_hash.iter().find(|(k, _)| k == name) {
                a.push((name, v));
            }
        }
        w.empty("w:documentProtection", &a);
    }
    w.val("w:defaultTabStop", &crate::units::twips(s.default_tab.clamp(1.0, 1584.0)));
    if s.auto_hyphenation {
        w.empty("w:autoHyphenation", &[]);
    }
    // Schema order: consecutiveHyphenLimit, hyphenationZone, doNotHyphenateCaps.
    if s.consecutive_hyphen_limit > 0 {
        w.val("w:consecutiveHyphenLimit", &s.consecutive_hyphen_limit.min(32_767).to_string());
    }
    if s.hyphenation_zone.is_finite() && (s.hyphenation_zone - wordcraft_doc::DEFAULT_HYPHENATION_ZONE).abs() > 0.01 {
        w.val("w:hyphenationZone", &crate::units::twips(s.hyphenation_zone.clamp(0.0, 1584.0)));
    }
    if !s.hyphenate_caps {
        w.empty("w:doNotHyphenateCaps", &[]);
    }
    if s.even_odd_headers {
        w.empty("w:evenAndOddHeaders", &[]);
    }
    for (tag, v) in [("w:drawingGridHorizontalSpacing", s.grid_h), ("w:drawingGridVerticalSpacing", s.grid_v)] {
        if v.is_finite() && (v - wordcraft_doc::DEFAULT_GRID).abs() > 0.01 {
            w.val(tag, &crate::units::twips(v.clamp(0.5, 1584.0)));
        }
    }
    w.val("w:characterSpacingControl", "doNotCompress");
    write_note_settings(&mut w, s, s_notes, footnotes, endnotes);
    write_compatibility_settings(&mut w, doc);
    if let Some(m) = &s.math {
        math::math_pr(&mut w, m);
    }
    w.close("w:settings");
    w.into_bytes()
}

fn write_note_settings(
    w: &mut W,
    settings: &wordcraft_doc::Settings,
    separators: (&wordcraft_doc::NoteSeparators, &wordcraft_doc::NoteSeparators),
    footnotes: bool,
    endnotes: bool,
) {
    for (tag, fmt, pr, used, el) in [
        ("w:footnotePr", settings.footnote_format, &settings.footnote_pr, footnotes, "w:footnote"),
        ("w:endnotePr", settings.endnote_format, &settings.endnote_pr, endnotes, "w:endnote"),
    ] {
        w.open(tag, &[]);
        story::note_pr_inner(w, &wordcraft_doc::section::NoteProps { num_fmt: Some(fmt), ..*pr });
        if used {
            w.empty(el, &[("w:id", "-1")]);
            w.empty(el, &[("w:id", "0")]);
            let seps = if el == "w:footnote" { separators.0 } else { separators.1 };
            if seps.continuation_notice.is_some() {
                w.empty(el, &[("w:id", "1")]);
            }
        }
        w.close(tag);
    }
}

fn write_compatibility_settings(w: &mut W, doc: &Document) {
    w.open("w:compat", &[]);
    // CT_Compat: the older switches come before the `w:compatSetting`s.
    if doc.settings.do_not_expand_shift_return {
        w.empty("w:doNotExpandShiftReturn", &[]);
    }
    if doc.settings.sum_paragraph_spacing {
        w.empty("w:doNotUseHTMLParagraphAutoSpacing", &[]);
    }
    if doc.settings.allow_space_of_same_style_in_table {
        w.empty("w:allowSpaceOfSameStyleInTable", &[]);
    }
    let mode = doc.settings.compat_mode.clamp(11, 15).to_string();
    w.empty("w:compatSetting", &[("w:name", "compatibilityMode"), ("w:uri", "http://schemas.microsoft.com/office/word"), ("w:val", &mode)]);
    w.close("w:compat");
}

/// Is `d` a usable `w16cid:durableId`: 1–8 hex digits, below 0x7FFFFFFF, not 0?
pub(crate) fn durable_id_ok(d: &str) -> bool {
    d.len() <= 8 && u32::from_str_radix(d, 16).is_ok_and(|v| v != 0 && v < 0x7FFF_FFFF)
}

/// `commentsIds.xml` (paragraph id → durable id) and `commentsExtensible.xml` (durable id → UTC
/// date) for the comments that got paragraph ids. Durable ids read from the file are kept (unique
/// ones); comments without one get the next free id. Word uses them to keep tracking a comment
/// across saves and for its UTC time.
pub(super) fn write_comment_ids(
    doc: &Document,
    comment_paras: &[(u32, String)],
    entries: &mut Vec<(String, Vec<u8>)>,
    overrides: &mut Vec<(String, String)>,
    rels: &mut PartRels,
) {
    if comment_paras.is_empty() {
        return;
    }
    // Keep the existing exact-string reservation rule, including comments without a written part.
    let reserved: std::collections::HashSet<&str> = doc.comments.values().filter_map(|c| c.durable_id.as_deref()).collect();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut next = 0x1000_0000u32;
    let mut rows: Vec<(String, String, Option<String>)> = Vec::new();
    for (cid, pid) in comment_paras {
        let Some(c) = doc.comments.get(cid) else { continue };
        let kept = c.durable_id.as_deref().filter(|d| durable_id_ok(d)).map(|d| format!("{:08X}", u32::from_str_radix(d, 16).unwrap_or(0)));
        let durable = match kept.filter(|d| !used.contains(d)) {
            Some(d) => d,
            None => loop {
                next = next.saturating_add(1);
                let d = format!("{next:08X}");
                if !used.contains(&d) && !reserved.contains(d.as_str()) {
                    break d;
                }
            },
        };
        used.insert(durable.clone());
        // Only a known UTC time: `w:date` is local time, often with a `Z` it doesn't mean.
        // Remove date and time on save (#1062): no comment date here either.
        let utc = if doc.settings.remove_date_time { None } else { c.date_utc.clone() };
        rows.push((pid.clone(), durable, utc));
    }
    let mc = "http://schemas.openxmlformats.org/markup-compatibility/2006";
    let mut w = W::new();
    w.open(
        "w16cid:commentsIds",
        &[("xmlns:mc", mc), ("xmlns:w16cid", "http://schemas.microsoft.com/office/word/2016/wordml/cid"), ("mc:Ignorable", "w16cid")],
    );
    for (pid, durable, _) in &rows {
        w.empty("w16cid:commentId", &[("w16cid:paraId", pid), ("w16cid:durableId", durable)]);
    }
    w.close("w16cid:commentsIds");
    rels.add(rt::COMMENTS_IDS, "commentsIds.xml", false);
    push_part(entries, overrides, "word/commentsIds.xml", w.into_bytes(), &format!("{CT_WML}commentsIds+xml"), PartRels::default());

    let mut w = W::new();
    w.open(
        "w16cex:commentsExtensible",
        &[("xmlns:mc", mc), ("xmlns:w16cex", "http://schemas.microsoft.com/office/word/2018/wordml/cex"), ("mc:Ignorable", "w16cex")],
    );
    for (_, durable, utc) in &rows {
        match utc {
            Some(d) => w.empty("w16cex:commentExtensible", &[("w16cex:durableId", durable), ("w16cex:dateUtc", d)]),
            None => w.empty("w16cex:commentExtensible", &[("w16cex:durableId", durable)]),
        }
    }
    w.close("w16cex:commentsExtensible");
    rels.add(rt::COMMENTS_EXTENSIBLE, "commentsExtensible.xml", false);
    push_part(entries, overrides, "word/commentsExtensible.xml", w.into_bytes(), &format!("{CT_WML}commentsExtensible+xml"), PartRels::default());
}

/// The theme part to write: the file's own while the document's theme is as it was read (#3541),
/// else one built from the model. The kept part carries what a theme built from the model has no
/// room for — its format scheme, the per-script fonts, the object defaults.
pub(super) fn theme_part(doc: &Document) -> Vec<u8> {
    let settings = &doc.settings;
    // GUARDRAIL: compare the theme the part was read into with the document's theme, never assume
    // a read document's theme is untouched — Design › Colours and › Fonts change it, and keeping
    // the file's part then loses the edit (#3541).
    match (doc.passthrough.get(crate::package::THEME_ORIG), doc.passthrough.get(crate::package::THEME_READ)) {
        (Some(kept), Some(read)) if !kept.is_empty() && read.as_slice() == crate::package::theme_fingerprint(settings).as_bytes() => {
            // GUARDRAIL: the kept bytes are the markup without its XML declaration
            // (`package::theme_markup`) — put the package's own in front, or the part is not XML a
            // reader can be told the encoding of (#3541).
            let mut part = W::new().into_bytes();
            part.extend_from_slice(kept);
            part
        }
        _ => theme_xml(doc),
    }
}

pub(super) fn theme_xml(doc: &Document) -> Vec<u8> {
    let s = &doc.settings;
    let names = ["dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"];
    let mut w = W::new();
    let name = if s.theme_name.is_empty() { "Craft" } else { s.theme_name.as_str() };
    w.open("a:theme", &[("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main"), ("name", name)]);
    w.open("a:themeElements", &[]);
    w.open("a:clrScheme", &[("name", name)]);
    for (i, n) in names.iter().enumerate() {
        let c = s.theme_colors.get(i).or_else(|| wordcraft_doc::THEME_COLORS.get(i)).copied().unwrap_or_default();
        w.open(&format!("a:{n}"), &[]);
        w.empty("a:srgbClr", &[("val", &c.hex())]);
        w.close(&format!("a:{n}"));
    }
    w.close("a:clrScheme");
    w.open("a:fontScheme", &[("name", name)]);
    for (tag, face) in [("a:majorFont", &s.major_font), ("a:minorFont", &s.minor_font)] {
        w.open(tag, &[]);
        w.empty("a:latin", &[("typeface", face)]);
        w.empty("a:ea", &[("typeface", "")]);
        w.empty("a:cs", &[("typeface", "")]);
        w.close(tag);
    }
    w.close("a:fontScheme");
    // A minimal format scheme (three entries per list, as the schema requires).
    w.open("a:fmtScheme", &[("name", name)]);
    w.open("a:fillStyleLst", &[]);
    for _ in 0..3 {
        w.raw("<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>");
    }
    w.close("a:fillStyleLst");
    w.open("a:lnStyleLst", &[]);
    for width in ["6350", "12700", "19050"] {
        w.raw(&format!(
            "<a:ln w=\"{width}\" cap=\"flat\" cmpd=\"sng\" algn=\"ctr\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:prstDash val=\"solid\"/><a:miter lim=\"800000\"/></a:ln>"
        ));
    }
    w.close("a:lnStyleLst");
    w.open("a:effectStyleLst", &[]);
    for _ in 0..3 {
        w.raw("<a:effectStyle><a:effectLst/></a:effectStyle>");
    }
    w.close("a:effectStyleLst");
    w.open("a:bgFillStyleLst", &[]);
    for _ in 0..3 {
        w.raw("<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>");
    }
    w.close("a:bgFillStyleLst");
    w.close("a:fmtScheme");
    w.close("a:themeElements");
    w.empty("a:objectDefaults", &[]);
    w.empty("a:extraClrSchemeLst", &[]);
    w.close("a:theme");
    w.into_bytes()
}

/// The author the saved file gives a comment or tracked change when the document removes personal
/// information on save (`w:removePersonalInformation`), as Word does.
pub(crate) const ANONYMOUS_AUTHOR: &str = "Author";

pub(crate) const ANONYMOUS_INITIALS: &str = "A";

/// The `w:author` written for an annotation by `author`: "Author" when the document removes
/// personal information on save, "Unknown" when it has none.
pub(crate) fn annotation_author<'a>(doc: &Document, author: &'a str) -> &'a str {
    if doc.settings.remove_personal_info {
        ANONYMOUS_AUTHOR
    } else if author.is_empty() {
        "Unknown"
    } else {
        author
    }
}

pub(super) fn core_xml(doc: &Document) -> Vec<u8> {
    let c = &doc.core;
    // Remove personal information (§17.15.1.70): no creator or last-modified-by in the file.
    let personal = |v: &String| if doc.settings.remove_personal_info { String::new() } else { v.clone() };
    let (creator, last_modified_by) = (personal(&c.creator), personal(&c.last_modified_by));
    let mut w = W::new();
    w.open(
        "cp:coreProperties",
        &[
            ("xmlns:cp", "http://schemas.openxmlformats.org/package/2006/metadata/core-properties"),
            ("xmlns:dc", "http://purl.org/dc/elements/1.1/"),
            ("xmlns:dcterms", "http://purl.org/dc/terms/"),
            ("xmlns:dcmitype", "http://purl.org/dc/dcmitype/"),
            ("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"),
        ],
    );
    for (tag, v) in [
        ("dc:title", &c.title),
        ("dc:subject", &c.subject),
        ("dc:creator", &creator),
        ("cp:keywords", &c.keywords),
        ("dc:description", &c.description),
        ("cp:lastModifiedBy", &last_modified_by),
        ("cp:category", &c.category),
    ] {
        if !v.is_empty() {
            w.leaf(tag, &[], v);
        }
    }
    if c.revision > 0 {
        w.leaf("cp:revision", &[], &c.revision.to_string());
    }
    for (tag, v) in [("dcterms:created", &c.created), ("dcterms:modified", &c.modified)] {
        if !v.is_empty() {
            w.leaf(tag, &[("xsi:type", "dcterms:W3CDTF")], v);
        }
    }
    w.close("cp:coreProperties");
    w.into_bytes()
}

pub(super) fn app_xml(doc: &Document) -> Vec<u8> {
    let mut w = W::new();
    w.open(
        "Properties",
        &[
            ("xmlns", "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"),
            ("xmlns:vt", "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"),
        ],
    );
    w.leaf("Application", &[], "WordCraft");
    w.leaf("Words", &[], &doc.word_count().to_string());
    w.leaf("Paragraphs", &[], &doc.paragraph_count().to_string());
    w.leaf("DocSecurity", &[], "0");
    // Kept properties, except the personal ones when the file asks Word to remove personal
    // information on save.
    let private = doc.settings.remove_personal_info;
    if let Some(kept) = doc.passthrough.get(crate::package::APP_KEPT) {
        for line in String::from_utf8_lossy(kept).lines().take(crate::package::APP_KEPT_PROPS.len()) {
            let Some((name, value)) = line.split_once('\t') else { continue };
            let Some(name) = crate::package::APP_KEPT_PROPS.iter().find(|n| **n == name) else { continue };
            if private && matches!(*name, "Company" | "Manager") {
                continue;
            }
            w.leaf(name, &[], value);
        }
    }
    w.close("Properties");
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use wordcraft_doc::Rgb;

    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const THEME_PART: &str = "word/theme/theme1.xml";
    /// The XML declaration every part WordCraft writes in front of its markup (`xml::W::new`).
    const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

    /// A theme shaped like Word's: system colours in the scheme, per-script fonts beside the Latin
    /// ones, a format scheme with a gradient, and object defaults — none of which the model holds.
    const THEME: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme"><a:themeElements>"#,
        r#"<a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>"#,
        r#"<a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1>"#,
        r#"<a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4>"#,
        r#"<a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink>"#,
        r#"<a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme>"#,
        r#"<a:fontScheme name="Office"><a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface=""/><a:cs typeface=""/>"#,
        r#"<a:font script="Jpan" typeface="Yu Gothic Light"/><a:font script="Arab" typeface="Times New Roman"/></a:majorFont>"#,
        r#"<a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/><a:font script="Jpan" typeface="Yu Mincho"/></a:minorFont></a:fontScheme>"#,
        r#"<a:fmtScheme name="Office"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill>"#,
        r#"<a:gradFill rotWithShape="1"><a:gsLst><a:gs pos="0"><a:schemeClr val="phClr"><a:lumMod val="110000"/></a:schemeClr></a:gs>"#,
        r#"<a:gs pos="100000"><a:schemeClr val="phClr"><a:satMod val="105000"/><a:tint val="67000"/></a:schemeClr></a:gs></a:gsLst>"#,
        r#"<a:lin ang="5400000" scaled="0"/></a:gradFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst>"#,
        r#"<a:lnStyleLst><a:ln w="6350" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill>"#,
        r#"<a:prstDash val="solid"/><a:miter lim="800000"/></a:ln><a:ln w="12700" cap="flat" cmpd="sng" algn="ctr"><a:solidFill>"#,
        r#"<a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="19050" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst>"#,
        r#"<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle>"#,
        r#"<a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill>"#,
        r#"<a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme>"#,
        r#"</a:themeElements><a:objectDefaults><a:spDef><a:spPr/><a:bodyPr/><a:lstStyle/><a:style><a:lnRef idx="1"><a:schemeClr val="accent1"/></a:lnRef>"#,
        r#"<a:fillRef idx="3"><a:schemeClr val="accent1"/></a:fillRef></a:style></a:spDef></a:objectDefaults><a:extraClrSchemeLst/></a:theme>"#,
    );

    /// A package whose main document is one paragraph and which relates `theme` as its theme.
    fn package(theme: &str) -> Vec<u8> {
        let doc = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Text</w:t></w:r></w:p></w:body></w:document>"#;
        let root = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
        );
        let rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/theme" Target="theme/theme1.xml"/></Relationships>"#
        );
        let types = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("[Content_Types].xml", types),
            ("_rels/.rels", &root),
            ("word/document.xml", doc),
            ("word/_rels/document.xml.rels", &rels),
            (THEME_PART, theme),
        ] {
            zw.start_file(name, zip::write::SimpleFileOptions::default()).expect("entry");
            zw.write_all(bytes.as_bytes()).expect("write");
        }
        zw.finish().expect("zip").into_inner()
    }

    /// The theme's markup without its XML declaration: what a Word XML Document (Flat OPC) inlines
    /// in `pkg:xmlData` (`flat::xml_fragment`), and so what a save reads back from one.
    fn markup() -> &'static str {
        THEME.strip_prefix(XML_DECL).expect("the theme starts with the package's declaration")
    }

    /// One part of a package, as written.
    fn part(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("package");
        let mut out = Vec::new();
        z.by_name(name).expect(name).read_to_end(&mut out).expect("part");
        out
    }

    /// Opening a file and saving it unchanged writes its theme part back byte for byte (#3541: the
    /// theme was rebuilt from the model, so its format scheme, per-script fonts and object defaults
    /// were replaced), and a theme edited in WordCraft is still written from the model.
    #[test]
    fn the_theme_part_is_saved_as_it_was_read() {
        let doc = crate::read(&package(THEME)).expect("read");
        assert_eq!(doc.settings.theme_name, "Office Theme", "the theme is read");
        let saved = part(&crate::write(&doc).expect("write"), THEME_PART);
        assert_eq!(String::from_utf8_lossy(&saved), THEME, "the theme part is not the file's own");
        // Opening the saved file again keeps it, so a save loop changes nothing.
        let reopened = crate::read(&crate::write(&doc).expect("write")).expect("reopen");
        assert_eq!(part(&crate::write(&reopened).expect("write"), THEME_PART), saved, "the theme changes on the second save");
        // GUARDRAIL: a Flat OPC file inlines a part's markup without its XML declaration, so the
        // same theme read from a .docx and from a Word XML file must be stored — and written — as
        // the one part: keeping the declaration broke `roundtrip::flat_opc_round_trip_equals_docx`
        // (#3541).
        let flat = crate::read(&package(markup())).expect("read without a declaration");
        assert_eq!(part(&crate::write(&flat).expect("write"), THEME_PART), saved, "a theme without a declaration is kept differently");

        // Design › Colours and › Fonts change the theme the part was read into: the part is then
        // built from the model, with the edit in it.
        let mut edited = doc.clone();
        edited.settings.theme_colors[4] = Rgb(1, 2, 3);
        edited.settings.major_font = "Georgia".into();
        let written = String::from_utf8_lossy(&part(&crate::write(&edited).expect("write"), THEME_PART)).into_owned();
        assert!(written.contains(r#"<a:accent1><a:srgbClr val="010203"/></a:accent1>"#), "{written}");
        assert!(written.contains(r#"<a:latin typeface="Georgia"/>"#), "{written}");
        assert_ne!(written, THEME, "an edited theme is not the file's own");
    }
}
