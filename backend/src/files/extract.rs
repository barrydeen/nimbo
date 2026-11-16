use serde_json::{json, Map, Value};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;

const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TEXT_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_METADATA_JSON: usize = 64 * 1024;
const MAX_METADATA_STRING: usize = 512;
const MAX_IMAGE_PIXELS: u64 = 80_000_000;
const MAX_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PDF_BYTES: u64 = 60 * 1024 * 1024;
const MAX_ARCHIVE_INPUT_BYTES: u64 = 96 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 500;
const MAX_ARCHIVE_CD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_TOTAL: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRY: u64 = 8 * 1024 * 1024;
const MAX_EXIF_FIELDS: usize = 80;
const MAX_XML_EVENTS: usize = 1_000_000;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Extraction {
    pub metadata: Map<String, Value>,
    pub text: Option<String>,
    pub thumbnail: Option<Vec<u8>>,
    pub partial: bool,
    pub error: Option<String>,
}

impl Extraction {
    /// Serializes extracted metadata. Oversized metadata is pruned structurally
    /// (bounded strings, arrays, objects and top-level keys) so the result is
    /// always valid JSON and never cut inside a UTF-8 character.
    pub fn metadata_json(&self) -> String {
        prune_metadata(&self.metadata)
    }
}

fn take_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

fn shrink_value(value: &mut Value, depth: usize) {
    match value {
        Value::String(text) => {
            if text.len() > MAX_METADATA_STRING {
                let cut = take_bytes(text, MAX_METADATA_STRING).len();
                text.truncate(cut);
            }
        }
        Value::Array(items) => {
            items.truncate(32);
            for item in items.iter_mut() {
                shrink_value(item, depth + 1);
            }
        }
        Value::Object(map) => {
            if depth > 6 {
                map.clear();
                return;
            }
            if map.len() > 32 {
                let mut keys: Vec<String> = map.keys().cloned().collect();
                keys.sort();
                keys.truncate(32);
                map.retain(|key, _| keys.contains(key));
            }
            for item in map.values_mut() {
                shrink_value(item, depth + 1);
            }
        }
        _ => {}
    }
}

fn prune_metadata(map: &Map<String, Value>) -> String {
    const MARKER: &str = ",\"_truncated\":true}";
    let mut value = Value::Object(map.clone());
    shrink_value(&mut value, 0);
    let mut truncated = false;
    loop {
        let Ok(encoded) = serde_json::to_string(&value) else {
            return "{}".to_string();
        };
        if !truncated && encoded.len() <= MAX_METADATA_JSON {
            return encoded;
        }
        if truncated && encoded.len() + MARKER.len() <= MAX_METADATA_JSON {
            return format!("{}{}", &encoded[..encoded.len() - 1], MARKER);
        }
        let Some(object) = value.as_object_mut() else {
            return "{\"_truncated\":true}".to_string();
        };
        // Drop the largest remaining top-level field first.
        let largest = object
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    serde_json::to_vec(value)
                        .map(|bytes| bytes.len())
                        .unwrap_or(0),
                )
            })
            .max_by_key(|(_, size)| *size)
            .map(|(key, _)| key);
        match largest {
            Some(key) if !object.is_empty() => {
                object.remove(&key);
                truncated = true;
            }
            _ => return "{\"_truncated\":true}".to_string(),
        }
        if object.is_empty() {
            return "{\"_truncated\":true}".to_string();
        }
    }
}

/// Sniffs a file's MIME type from magic bytes, falling back to its extension.
pub fn detect_mime(name: &str, head: &[u8]) -> String {
    let sniffed = infer::get(head).map(|kind| kind.mime_type().to_string());
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();

    let office = match extension.as_str() {
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        "doc" => Some("application/msword"),
        "xls" => Some("application/vnd.ms-excel"),
        "ppt" => Some("application/vnd.ms-powerpoint"),
        "odt" => Some("application/vnd.oasis.opendocument.text"),
        "ods" => Some("application/vnd.oasis.opendocument.spreadsheet"),
        "odp" => Some("application/vnd.oasis.opendocument.presentation"),
        "epub" => Some("application/epub+zip"),
        _ => None,
    };
    if let Some(mime) = office {
        // OOXML/ODF/EPUB are ZIP containers; the extension is more specific than
        // the generic sniffed archive type.
        return mime.to_string();
    }
    if extension == "svg" {
        return "image/svg+xml".to_string();
    }

    sniffed
        .or_else(|| mime_guess::from_path(name).first_raw().map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

pub fn is_image(mime: &str) -> bool {
    mime.starts_with("image/")
}

pub fn is_audio(mime: &str) -> bool {
    mime.starts_with("audio/")
}

pub fn is_texty(name: &str, mime: &str) -> bool {
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    const TEXT_EXTENSIONS: &[&str] = &[
        "txt", "md", "markdown", "rst", "log", "csv", "tsv", "json", "jsonl", "ndjson", "yaml",
        "yml", "toml", "ini", "cfg", "conf", "xml", "html", "htm", "css", "scss", "sass", "less",
        "js", "jsx", "mjs", "cjs", "ts", "tsx", "py", "rb", "rs", "go", "java", "kt", "kts", "c",
        "h", "cc", "cpp", "hpp", "cs", "php", "swift", "sh", "bash", "zsh", "fish", "sql",
        "graphql", "gql", "vue", "svelte", "astro", "lua", "r", "tex", "diff", "patch", "srt",
        "vtt", "env",
    ];
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/x-httpd-php"
                | "application/x-sh"
                | "application/sql"
                | "application/graphql"
                | "application/x-yaml"
                | "application/toml"
                | "application/x-ndjson"
                | "image/svg+xml"
        )
        || TEXT_EXTENSIONS.contains(&extension.as_str())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect()
    }
}

/// Appends only up to `budget` bytes so repeated archive entries cannot grow the
/// extracted text without bound.
fn push_text(text: &mut String, extra: &str, budget: usize) {
    if text.len() >= budget {
        return;
    }
    text.push_str(take_bytes(extra, budget - text.len()));
}

fn decode_text(bytes: &[u8]) -> (String, &'static str) {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        return (String::from_utf16_lossy(&units), "utf-16le");
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        return (String::from_utf16_lossy(&units), "utf-16be");
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), "utf-8"),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), "utf-8-lossy"),
    }
}

fn html_regexes() -> &'static (regex::Regex, regex::Regex, regex::Regex) {
    static RE: OnceLock<(regex::Regex, regex::Regex, regex::Regex)> = OnceLock::new();
    RE.get_or_init(|| {
        (
            regex::Regex::new(r"(?is)<script\b[^>]*>.*?</script>").expect("regex"),
            regex::Regex::new(r"(?is)<style\b[^>]*>.*?</style>").expect("regex"),
            regex::Regex::new(r"(?s)<[^>]+>").expect("regex"),
        )
    })
}

fn strip_html(input: &str) -> String {
    let (scripts, styles, tags) = html_regexes();
    let without_scripts = scripts.replace_all(input, " ");
    let without_styles = styles.replace_all(&without_scripts, " ");
    let without_tags = tags.replace_all(&without_styles, " ");
    without_tags
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn image_metadata(bytes: &[u8]) -> (Map<String, Value>, Option<Vec<u8>>, Option<String>, bool) {
    let mut info = Map::new();
    let mut thumbnail = None;
    let mut warning = None;
    let mut partial = false;

    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| err.to_string());
    let reader = match reader {
        Ok(reader) => reader,
        Err(err) => return (info, None, Some(err), true),
    };
    if let Some(format) = reader.format() {
        info.insert("format".into(), json!(format!("{format:?}")));
    }

    let dimensions = {
        let mut limited = image::ImageReader::new(Cursor::new(bytes));
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(20_000);
        limits.max_image_height = Some(20_000);
        limits.max_alloc = Some(256 * 1024 * 1024);
        limited.limits(limits);
        match limited
            .with_guessed_format()
            .map_err(image::ImageError::IoError)
            .and_then(|reader| reader.into_dimensions())
        {
            Ok(dimensions) => dimensions,
            Err(err) => return (info, None, Some(err.to_string()), true),
        }
    };
    info.insert("width".into(), json!(dimensions.0));
    info.insert("height".into(), json!(dimensions.1));

    let mut orientation = 1u8;
    let mut exif_fields = Map::new();
    if let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) {
        if let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
            if let Some(value) = field.value.get_uint(0) {
                orientation = value as u8;
            }
        }
        for field in exif.fields().take(MAX_EXIF_FIELDS) {
            let key = field.tag.to_string();
            if exif_fields.contains_key(&key) {
                continue;
            }
            let value = field.display_value().with_unit(&exif).to_string();
            if !value.is_empty() && value.len() <= 500 {
                exif_fields.insert(key, json!(value));
            }
        }
    }
    if !exif_fields.is_empty() {
        info.insert("exif".into(), Value::Object(exif_fields));
    }

    if (dimensions.0 as u64) * (dimensions.1 as u64) > MAX_IMAGE_PIXELS {
        return (
            info,
            None,
            Some("image is too large to decode".into()),
            true,
        );
    }

    let decoded = {
        let mut reader = match image::ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
            Ok(reader) => reader,
            Err(err) => return (info, None, Some(err.to_string()), true),
        };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(20_000);
        limits.max_image_height = Some(20_000);
        limits.max_alloc = Some(256 * 1024 * 1024);
        reader.limits(limits);
        reader.decode()
    };
    match decoded {
        Ok(mut image) => {
            if let Some(orientation) = image::metadata::Orientation::from_exif(orientation) {
                image.apply_orientation(orientation);
            }
            let thumb = image.thumbnail(512, 512).to_rgb8();
            let mut output = Vec::new();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 82);
            if let Err(err) = encoder.encode_image(&thumb) {
                warning = Some(err.to_string());
            } else {
                thumbnail = Some(output);
            }
        }
        Err(err) => {
            warning = Some(err.to_string());
            partial = true;
        }
    }

    (info, thumbnail, warning, partial)
}

fn pdf_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) && bytes.len().is_multiple_of(2) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn pdf_metadata(bytes: &[u8]) -> (Map<String, Value>, Option<String>, bool) {
    let mut info = Map::new();
    let mut text = None;
    let mut partial = false;

    // Gate before the first (lopdf) parser, not only before pdf_extract.
    if bytes.len() as u64 > MAX_PDF_BYTES {
        info.insert(
            "text_skipped".into(),
            json!("file is too large for PDF parsing"),
        );
        return (info, None, true);
    }

    let doc = match lopdf::Document::load_mem(bytes) {
        Ok(doc) => doc,
        Err(err) => {
            info.insert("error".into(), json!(err.to_string()));
            return (info, None, true);
        }
    };

    info.insert("pages".into(), json!(doc.get_pages().len()));
    if doc.is_encrypted() {
        info.insert("encrypted".into(), json!(true));
    }

    if let Ok(info_object) = doc.trailer.get(b"Info") {
        let dict = match info_object {
            lopdf::Object::Reference(id) => doc
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_dict().ok()),
            lopdf::Object::Dictionary(dict) => Some(dict),
            _ => None,
        };
        if let Some(dict) = dict {
            for (key, value) in dict.iter() {
                let key = String::from_utf8_lossy(key)
                    .trim_start_matches('/')
                    .to_string();
                let rendered = if let Ok(value) = value.as_str() {
                    Some(pdf_string(value))
                } else {
                    value.as_i64().ok().map(|number| number.to_string())
                };
                if let Some(value) = rendered {
                    if !value.is_empty() && value.len() <= 2000 {
                        info.insert(key, json!(value));
                    }
                }
            }
        }
    }

    if bytes.len() as u64 <= MAX_PDF_BYTES {
        let extraction = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pdf_extract::extract_text_from_mem(bytes)
        }));
        match extraction {
            Ok(Ok(extracted)) => text = Some(truncate_chars(&extracted, MAX_TEXT_BYTES)),
            Ok(Err(err)) => {
                info.insert("text_error".into(), json!(err.to_string()));
                partial = true;
            }
            Err(_) => {
                info.insert("text_error".into(), json!("pdf text extraction panicked"));
                partial = true;
            }
        }
    } else {
        info.insert(
            "text_skipped".into(),
            json!("file is too large for text extraction"),
        );
        partial = true;
    }

    (info, text, partial)
}

/// Reads the declared entry count and central-directory size from the ZIP
/// end-of-central-directory record without materialising the archive. This lets
/// the extractor reject pathological archives before `ZipArchive::new` builds
/// the central directory.
fn zip_declared_entries(bytes: &[u8]) -> Option<(usize, u64)> {
    const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    if bytes.len() < 22 {
        return None;
    }
    let window_start = bytes.len().saturating_sub(65_557);
    let mut index = bytes.len() - 4;
    let eocd = loop {
        if bytes[index..index + 4] == EOCD {
            break index;
        }
        if index == window_start {
            return None;
        }
        index -= 1;
    };
    if eocd + 22 > bytes.len() {
        return None;
    }
    let count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_size = u32::from_le_bytes([
        bytes[eocd + 12],
        bytes[eocd + 13],
        bytes[eocd + 14],
        bytes[eocd + 15],
    ]) as u64;
    // ZIP64 sentinels mean "look elsewhere"; treat as oversized for our caps.
    if count == usize::from(u16::MAX) || cd_size == u64::from(u32::MAX) {
        return Some((usize::MAX, cd_size));
    }
    Some((count, cd_size))
}

fn read_zip_entry(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    budget: &mut u64,
) -> Option<Vec<u8>> {
    let entry = archive.by_name(name).ok()?;
    if entry.size() > MAX_ARCHIVE_ENTRY || entry.size() > *budget {
        return None;
    }
    let limit = entry.size();
    *budget -= limit;
    let mut data = Vec::with_capacity(limit as usize);
    entry.take(MAX_ARCHIVE_ENTRY).read_to_end(&mut data).ok()?;
    Some(data)
}

fn local_name(name: &str) -> String {
    name.rsplit(':').next().unwrap_or("").to_string()
}

/// Collects text and selected metadata fields from an XML document. When
/// `text_tags` is empty all text nodes are collected.
fn xml_metadata(
    xml: &str,
    wanted_fields: &[&str],
    text_tags: &[&str],
    paragraph_tags: &[&str],
) -> (Map<String, Value>, String) {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut fields = Map::new();
    let mut text = String::new();
    let mut current = String::new();
    let mut value = String::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let text_budget = MAX_TEXT_BYTES * 2;
    let mut events = 0usize;

    loop {
        events += 1;
        if events > MAX_XML_EVENTS {
            break;
        }
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                current = local_name(event.local_name().as_ref());
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(event.local_name().as_ref());
                if name == "sheet" {
                    for attr in event.attributes().flatten() {
                        if local_name(attr.key.local_name().as_ref()) == "name" {
                            if let Ok(value) =
                                attr.normalized_value(quick_xml::XmlVersion::Explicit1_0)
                            {
                                fields.insert("sheet".into(), json!(value.into_owned()));
                            }
                        }
                    }
                }
            }
            Ok(Event::Text(event)) => {
                let content = event.xml_content(quick_xml::XmlVersion::Explicit1_0);
                if !content.is_empty() {
                    let wanted = text_tags.is_empty() || text_tags.contains(&current.as_str());
                    if wanted {
                        if value.len() < 100_000 {
                            value.push_str(&content);
                        }
                        if text.len() < text_budget {
                            text.push_str(&content);
                            text.push(' ');
                        }
                    }
                }
            }
            Ok(Event::GeneralRef(event)) => {
                let entity = event.xml_content(quick_xml::XmlVersion::Explicit1_0);
                let decoded = match entity.as_ref() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                };
                if !decoded.is_empty()
                    && (text_tags.is_empty() || text_tags.contains(&current.as_str()))
                {
                    if value.len() < 100_000 {
                        value.push_str(decoded);
                    }
                    if text.len() < text_budget {
                        text.push_str(decoded);
                    }
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.local_name().as_ref());
                if wanted_fields.contains(&name.as_str()) && !value.trim().is_empty() {
                    fields.insert(name.clone(), json!(truncate_chars(value.trim(), 2000)));
                }
                if paragraph_tags.contains(&name.as_str()) && text.len() < text_budget {
                    text.push('\n');
                }
                value.clear();
                current.clear();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    (fields, text)
}

fn archive_text(extension: &str, bytes: &[u8]) -> (Map<String, Value>, Option<String>, bool) {
    let mut info = Map::new();
    let mut text = String::new();
    let mut partial = false;
    match zip_declared_entries(bytes) {
        Some((count, cd_size)) if count > MAX_ARCHIVE_ENTRIES || cd_size > MAX_ARCHIVE_CD_BYTES => {
            if count != usize::MAX {
                info.insert("entries".into(), json!(count));
            }
            info.insert("entry_limit".into(), json!(true));
            return (info, None, true);
        }
        _ => {}
    }
    let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
        Ok(archive) => archive,
        Err(err) => {
            info.insert("error".into(), json!(err.to_string()));
            return (info, None, true);
        }
    };
    info.insert("entries".into(), json!(archive.len()));
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        info.insert("entry_limit".into(), json!(true));
        partial = true;
    }

    let names: Vec<String> = archive
        .file_names()
        .filter_map(|name| name.ok().map(|n| n.into_owned()))
        .take(MAX_ARCHIVE_ENTRIES)
        .collect();
    let mut budget = MAX_ARCHIVE_TOTAL;

    let core = read_zip_entry(&mut archive, "docProps/core.xml", &mut budget);
    if let Some(core) = core {
        let (fields, _) = xml_metadata(
            &String::from_utf8_lossy(&core),
            &[
                "title",
                "creator",
                "subject",
                "description",
                "keywords",
                "lastModifiedBy",
                "created",
                "modified",
                "revision",
                "category",
                "contentStatus",
                "language",
            ],
            &[],
            &[],
        );
        for (key, value) in fields {
            info.insert(key, value);
        }
    }
    let app = read_zip_entry(&mut archive, "docProps/app.xml", &mut budget);
    if let Some(app) = app {
        let (fields, _) = xml_metadata(
            &String::from_utf8_lossy(&app),
            &[
                "Application",
                "AppVersion",
                "Company",
                "Manager",
                "Pages",
                "Words",
                "Characters",
                "Paragraphs",
                "Slides",
                "Worksheets",
            ],
            &[],
            &[],
        );
        for (key, value) in fields {
            info.insert(key, value);
        }
    }

    let meta_entry = match extension {
        "odt" | "ods" | "odp" => Some("meta.xml"),
        "epub" => None,
        _ => None,
    };
    if let Some(name) = meta_entry {
        if let Some(meta) = read_zip_entry(&mut archive, name, &mut budget) {
            let (fields, _) = xml_metadata(
                &String::from_utf8_lossy(&meta),
                &[
                    "title",
                    "creator",
                    "subject",
                    "description",
                    "language",
                    "generator",
                ],
                &[],
                &[],
            );
            for (key, value) in fields {
                info.insert(key, value);
            }
        }
    }

    match extension {
        "docx" => {
            if let Some(document) = read_zip_entry(&mut archive, "word/document.xml", &mut budget) {
                let (_, extracted) = xml_metadata(
                    &String::from_utf8_lossy(&document),
                    &[],
                    &["t"],
                    &["p", "tr"],
                );
                push_text(&mut text, &extracted, MAX_TEXT_BYTES * 2);
            }
        }
        "pptx" => {
            let mut slides: Vec<String> = names
                .iter()
                .filter(|name| name.starts_with("ppt/slides/slide") && name.ends_with(".xml"))
                .cloned()
                .collect();
            slides.sort();
            for slide in slides {
                if let Some(slide) = read_zip_entry(&mut archive, &slide, &mut budget) {
                    let (_, extracted) =
                        xml_metadata(&String::from_utf8_lossy(&slide), &[], &["t"], &["p"]);
                    push_text(&mut text, &extracted, MAX_TEXT_BYTES * 2);
                    push_text(&mut text, "\n", MAX_TEXT_BYTES * 2);
                }
            }
        }
        "xlsx" => {
            if let Some(strings) = read_zip_entry(&mut archive, "xl/sharedStrings.xml", &mut budget)
            {
                let (_, extracted) =
                    xml_metadata(&String::from_utf8_lossy(&strings), &[], &["t"], &[]);
                push_text(&mut text, &extracted, MAX_TEXT_BYTES * 2);
            }
            if let Some(workbook) = read_zip_entry(&mut archive, "xl/workbook.xml", &mut budget) {
                let (_, extracted) =
                    xml_metadata(&String::from_utf8_lossy(&workbook), &[], &[], &[]);
                let _ = extracted;
            }
        }
        "odt" | "ods" | "odp" => {
            if let Some(content) = read_zip_entry(&mut archive, "content.xml", &mut budget) {
                let (_, extracted) = xml_metadata(
                    &String::from_utf8_lossy(&content),
                    &[],
                    &[],
                    &["p", "h", "table-row"],
                );
                push_text(&mut text, &extracted, MAX_TEXT_BYTES * 2);
            }
        }
        "epub" => {
            for name in names {
                if (name.ends_with(".xhtml") || name.ends_with(".html") || name.ends_with(".htm"))
                    && name != "nav.xhtml"
                {
                    if let Some(page) = read_zip_entry(&mut archive, &name, &mut budget) {
                        let html = String::from_utf8_lossy(&page);
                        let stripped = strip_html(&html);
                        push_text(&mut text, &stripped, MAX_TEXT_BYTES * 2);
                        push_text(&mut text, "\n", MAX_TEXT_BYTES * 2);
                    }
                }
            }
        }
        _ => {}
    }

    let text = text.trim();
    let text = if text.is_empty() {
        None
    } else {
        Some(truncate_chars(text, MAX_TEXT_BYTES))
    };
    (info, text, partial)
}

fn audio_metadata(path: &Path) -> (Map<String, Value>, Option<String>) {
    use lofty::file::AudioFile;
    use lofty::prelude::{Accessor, TaggedFileExt};

    let mut info = Map::new();
    let tagged = match lofty::read_from_path(path) {
        Ok(tagged) => tagged,
        Err(err) => {
            info.insert("error".into(), json!(err.to_string()));
            return (info, Some(err.to_string()));
        }
    };

    let properties = tagged.properties();
    let duration = properties.duration();
    info.insert("duration_secs".into(), json!(duration.as_secs()));
    info.insert(
        "duration".into(),
        json!(format!(
            "{}:{:02}",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )),
    );
    if let Some(bitrate) = properties.audio_bitrate().filter(|value| *value > 0) {
        info.insert("bitrate_kbps".into(), json!(bitrate));
    }
    if let Some(bitrate) = properties.overall_bitrate().filter(|value| *value > 0) {
        info.insert("overall_bitrate_kbps".into(), json!(bitrate));
    }
    if let Some(sample_rate) = properties.sample_rate().filter(|value| *value > 0) {
        info.insert("sample_rate_hz".into(), json!(sample_rate));
    }
    if let Some(channels) = properties.channels().filter(|value| *value > 0) {
        info.insert("channels".into(), json!(channels));
    }

    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        let fields = [
            ("title", tag.title().map(|v| v.to_string())),
            ("artist", tag.artist().map(|v| v.to_string())),
            ("album", tag.album().map(|v| v.to_string())),
            ("genre", tag.genre().map(|v| v.to_string())),
            (
                "year",
                tag.get_string(lofty::tag::ItemKey::Year)
                    .map(str::to_string),
            ),
            (
                "date",
                tag.get_string(lofty::tag::ItemKey::RecordingDate)
                    .map(str::to_string),
            ),
            ("track", tag.track().map(|v| v.to_string())),
            ("comment", tag.comment().map(|v| v.to_string())),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                if !value.trim().is_empty() && value.len() <= 2000 {
                    info.insert(key.into(), json!(value));
                }
            }
        }
    }

    (info, None)
}

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Runs all extraction for a file. CPU-heavy and blocking; callers must use
/// `spawn_blocking` (the worker does).
///
/// Each parser is gated *before* the blob is read so an oversized or unsupported
/// input never forces a whole-file allocation on the sole indexing worker.
pub fn extract(path: &Path, name: &str, mime: &str) -> Extraction {
    let mut extraction = Extraction::default();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    extraction.metadata.insert("mime".into(), json!(mime));
    extraction.metadata.insert("size".into(), json!(size));
    if let Some(extension) = name.rsplit('.').next() {
        if extension != name {
            extraction
                .metadata
                .insert("extension".into(), json!(extension.to_ascii_lowercase()));
        }
    }

    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let office = matches!(
        mime,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            | "application/vnd.oasis.opendocument.text"
            | "application/vnd.oasis.opendocument.spreadsheet"
            | "application/vnd.oasis.opendocument.presentation"
            | "application/epub+zip"
    );

    let read = |limit: u64, extraction: &mut Extraction| match read_bounded(path, limit) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            extraction.error = Some(err.to_string());
            extraction.partial = true;
            None
        }
    };

    if is_image(mime) && mime != "image/svg+xml" {
        if size > MAX_IMAGE_BYTES {
            extraction.error = Some("image is too large to inspect".into());
            extraction.partial = true;
            return extraction;
        }
        let Some(bytes) = read(size.max(1), &mut extraction) else {
            return extraction;
        };
        let (image_info, thumbnail, warning, partial) = image_metadata(&bytes);
        if !image_info.is_empty() {
            extraction
                .metadata
                .insert("image".into(), Value::Object(image_info));
        }
        extraction.thumbnail = thumbnail;
        if let Some(warning) = warning {
            extraction.error = Some(warning);
        }
        extraction.partial |= partial;
    } else if mime == "application/pdf" {
        if size > MAX_PDF_BYTES {
            extraction.error = Some("PDF is too large to parse".into());
            extraction.partial = true;
            return extraction;
        }
        let Some(bytes) = read(size.max(1), &mut extraction) else {
            return extraction;
        };
        let (pdf_info, text, partial) = pdf_metadata(&bytes);
        if !pdf_info.is_empty() {
            extraction
                .metadata
                .insert("pdf".into(), Value::Object(pdf_info));
        }
        extraction.text = text;
        extraction.partial |= partial;
    } else if office {
        if size > MAX_ARCHIVE_INPUT_BYTES {
            extraction.error = Some("archive is too large to parse".into());
            extraction.partial = true;
            return extraction;
        }
        let Some(bytes) = read(size.max(1), &mut extraction) else {
            return extraction;
        };
        let (office_info, text, partial) = archive_text(&extension, &bytes);
        if !office_info.is_empty() {
            extraction
                .metadata
                .insert("office".into(), Value::Object(office_info));
        }
        extraction.text = text;
        extraction.partial |= partial;
    } else if is_audio(mime) {
        let (audio_info, warning) = audio_metadata(path);
        if !audio_info.is_empty() {
            extraction
                .metadata
                .insert("audio".into(), Value::Object(audio_info));
        }
        if let Some(warning) = warning {
            extraction.error = Some(warning);
        }
    } else if is_texty(name, mime) {
        // Read only a bounded prefix; text is never decoded at unbounded size.
        let Some(bytes) = read(MAX_TEXT_SOURCE_BYTES + 1, &mut extraction) else {
            return extraction;
        };
        let head = &bytes[..bytes.len().min(8192)];
        // content_inspector helps reject text-looking extensions on binary data.
        let is_binary = content_inspector::inspect(head) == content_inspector::ContentType::BINARY;
        if !is_binary {
            let (decoded, encoding) = decode_text(&bytes);
            let decoded = if mime.starts_with("text/html") || mime == "image/svg+xml" {
                strip_html(&decoded)
            } else {
                decoded
            };
            let line_count = decoded.lines().count();
            let word_count = decoded.split_whitespace().count();
            extraction.metadata.insert(
                "text".into(),
                json!({
                    "encoding": encoding,
                    "lines": line_count,
                    "words": word_count,
                    "truncated": size > MAX_TEXT_SOURCE_BYTES,
                }),
            );
            extraction.text = Some(take_bytes(&decoded, MAX_TEXT_BYTES).to_string());
        } else {
            extraction
                .metadata
                .insert("text".into(), json!({ "binary": true }));
        }
    }

    extraction
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_office_extensions_over_zip_magic() {
        assert_eq!(
            detect_mime("report.docx", b"PK\x03\x04"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert_eq!(detect_mime("photo.png", b"\x89PNG\r\n\x1a\n"), "image/png");
    }

    #[test]
    fn strips_html_tags() {
        assert_eq!(
            strip_html("<p>Hello <b>world</b> &amp; friends</p>"),
            "Hello world & friends"
        );
    }

    #[test]
    fn extracts_docx_style_xml_text() {
        let xml = r#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t> world</w:t></w:r></w:p></w:body></w:document>"#;
        let (_, text) = xml_metadata(xml, &[], &["t"], &["p"]);
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
    }

    #[test]
    fn metadata_pruning_never_breaks_json_or_utf8() {
        let mut extraction = Extraction::default();
        extraction.metadata.insert(
            "huge".into(),
            json!({"text": "é".repeat(200_000), "list": vec![json!("ü".repeat(1000)); 100]}),
        );
        for index in 0..200 {
            extraction
                .metadata
                .insert(format!("field{index:03}"), json!("x".repeat(2000)));
        }
        let encoded = extraction.metadata_json();
        assert!(encoded.len() <= MAX_METADATA_JSON);
        let parsed: Value = serde_json::from_str(&encoded).expect("valid json");
        assert!(parsed.is_object());
        assert!(std::str::from_utf8(encoded.as_bytes()).is_ok());
    }

    #[test]
    fn metadata_pruning_keeps_small_payloads_intact() {
        let mut extraction = Extraction::default();
        extraction.metadata.insert("title".into(), json!("Report"));
        assert_eq!(extraction.metadata_json(), r#"{"title":"Report"}"#);
    }

    #[test]
    fn zip_eocd_entry_count_is_read_before_parsing() {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();
            for index in 0..3 {
                use std::io::Write;
                writer
                    .start_file(format!("file{index}.txt"), options)
                    .unwrap();
                writer.write_all(b"hello").unwrap();
            }
            writer.finish().unwrap();
        }
        let bytes = buffer.into_inner();
        let (count, _) = zip_declared_entries(&bytes).expect("eocd");
        assert_eq!(count, 3);
    }

    #[test]
    fn archive_entry_gate_rejects_many_entries() {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();
            for index in 0..(MAX_ARCHIVE_ENTRIES + 5) {
                writer
                    .start_file(format!("file{index}.txt"), options)
                    .unwrap();
            }
            writer.finish().unwrap();
        }
        let bytes = buffer.into_inner();
        let (info, text, partial) = archive_text("docx", &bytes);
        assert!(text.is_none());
        assert!(partial);
        assert_eq!(info.get("entry_limit"), Some(&json!(true)));
    }
}
