use serde_json::{json, Map, Value};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;

const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_METADATA_JSON: usize = 64 * 1024;
const MAX_IMAGE_PIXELS: u64 = 80_000_000;
const MAX_PDF_BYTES: u64 = 60 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 500;
const MAX_ARCHIVE_TOTAL: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRY: u64 = 8 * 1024 * 1024;
const MAX_EXIF_FIELDS: usize = 80;

#[derive(Debug, Default)]
pub struct Extraction {
    pub metadata: Map<String, Value>,
    pub text: Option<String>,
    pub thumbnail: Option<Vec<u8>>,
    pub partial: bool,
    pub error: Option<String>,
}

impl Extraction {
    pub fn metadata_json(&self) -> String {
        let value = Value::Object(self.metadata.clone());
        let mut encoded = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
        if encoded.len() > MAX_METADATA_JSON {
            encoded.truncate(MAX_METADATA_JSON);
            if let Some(cut) = encoded.rfind('"') {
                encoded.truncate(cut + 1);
            }
            encoded.push_str(",\"_truncated\":true}");
        }
        encoded
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

    let decoded = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)
        .and_then(|reader| reader.decode());
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

    loop {
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
                    if text_tags.is_empty() || text_tags.contains(&current.as_str()) {
                        value.push_str(&content);
                    }
                    if text_tags.is_empty() || text_tags.contains(&current.as_str()) {
                        text.push_str(&content);
                        text.push(' ');
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
                    value.push_str(decoded);
                    text.push_str(decoded);
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.local_name().as_ref());
                if wanted_fields.contains(&name.as_str()) && !value.trim().is_empty() {
                    fields.insert(name.clone(), json!(truncate_chars(value.trim(), 2000)));
                }
                if paragraph_tags.contains(&name.as_str()) {
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
                text.push_str(&extracted);
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
                    text.push_str(&extracted);
                    text.push('\n');
                }
            }
        }
        "xlsx" => {
            if let Some(strings) = read_zip_entry(&mut archive, "xl/sharedStrings.xml", &mut budget)
            {
                let (_, extracted) =
                    xml_metadata(&String::from_utf8_lossy(&strings), &[], &["t"], &[]);
                text.push_str(&extracted);
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
                text.push_str(&extracted);
            }
        }
        "epub" => {
            for name in names {
                if (name.ends_with(".xhtml") || name.ends_with(".html") || name.ends_with(".htm"))
                    && name != "nav.xhtml"
                {
                    if let Some(page) = read_zip_entry(&mut archive, &name, &mut budget) {
                        let html = String::from_utf8_lossy(&page);
                        text.push_str(&strip_html(&html));
                        text.push('\n');
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

/// Runs all extraction for a file. CPU-heavy and blocking; callers must use
/// `spawn_blocking` (the worker does).
pub fn extract(path: &Path, name: &str, mime: &str) -> Extraction {
    let mut extraction = Extraction::default();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            extraction.error = Some(err.to_string());
            extraction.partial = true;
            return extraction;
        }
    };
    let head = &bytes[..bytes.len().min(8192)];

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

    if is_image(mime) && mime != "image/svg+xml" {
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
        let (pdf_info, text, partial) = pdf_metadata(&bytes);
        if !pdf_info.is_empty() {
            extraction
                .metadata
                .insert("pdf".into(), Value::Object(pdf_info));
        }
        extraction.text = text;
        extraction.partial |= partial;
    } else if matches!(
        mime,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            | "application/vnd.oasis.opendocument.text"
            | "application/vnd.oasis.opendocument.spreadsheet"
            | "application/vnd.oasis.opendocument.presentation"
            | "application/epub+zip"
    ) {
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
                }),
            );
            extraction.text = Some(truncate_chars(&decoded, MAX_TEXT_BYTES));
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
}
