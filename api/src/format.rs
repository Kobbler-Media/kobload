use std::path::Path;

/// Detect file format from magic bytes (file content header).
/// Returns a normalized format string (e.g., "epub", "pdf") and MIME type.
pub fn detect_from_bytes(buf: &[u8]) -> Option<FormatInfo> {
    let kind = infer::get(buf)?;
    let mime = kind.mime_type();
    let ext = normalize_extension(kind.extension());
    Some(FormatInfo {
        extension: ext.to_string(),
        mime_type: mime.to_string(),
    })
}

/// Detect file format from a file path using magic bytes.
pub fn detect_from_file(path: &Path) -> Option<FormatInfo> {
    let buf = std::fs::read(path).ok()?;
    detect_from_bytes(&buf)
}

/// Detect format from filename/title text (for search results before download).
pub fn detect_format_from_text(file_name: Option<&str>, title: &str) -> Option<String> {
    if let Some(name) = file_name
        && let Some(fmt) = detect_from_extension(name)
    {
        return Some(fmt);
    }
    detect_from_extension(title)
}

/// Detect MIME type from a file extension string.
pub fn mime_from_extension(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        "epub" => Some("application/epub+zip"),
        "pdf" => Some("application/pdf"),
        "mobi" | "azw3" | "azw" => Some("application/x-mobipocket-ebook"),
        "cbz" => Some("application/vnd.comicbook+zip"),
        "cbr" => Some("application/vnd.comicbook-rar"),
        "djvu" => Some("image/vnd.djvu"),
        "fb2" => Some("application/x-fictionbook+xml"),
        "mp3" => Some("audio/mpeg"),
        "m4b" | "m4a" => Some("audio/mp4"),
        "flac" => Some("audio/flac"),
        "ogg" => Some("audio/ogg"),
        "opus" => Some("audio/opus"),
        "zip" => Some("application/zip"),
        "rar" => Some("application/vnd.rar"),
        "7z" => Some("application/x-7z-compressed"),
        "tar" => Some("application/x-tar"),
        "gz" => Some("application/gzip"),
        "bz2" => Some("application/x-bzip2"),
        "xz" => Some("application/x-xz"),
        "mkv" => Some("video/x-matroska"),
        "mp4" => Some("video/mp4"),
        "avi" => Some("video/x-msvideo"),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct FormatInfo {
    pub extension: String,
    pub mime_type: String,
}

fn normalize_extension(ext: &str) -> &str {
    match ext {
        "azw" => "azw3",
        "m4a" => "m4b",
        _ => ext,
    }
}

fn detect_from_extension(text: &str) -> Option<String> {
    let lower = text.to_lowercase();

    let formats = [
        ("epub", "epub"),
        ("pdf", "pdf"),
        ("mobi", "mobi"),
        ("azw3", "azw3"),
        ("azw", "azw3"),
        ("cbz", "cbz"),
        ("cbr", "cbr"),
        ("djvu", "djvu"),
        ("fb2", "fb2"),
        ("mp3", "mp3"),
        ("m4b", "m4b"),
        ("m4a", "m4b"),
        ("flac", "flac"),
        ("ogg", "ogg"),
        ("opus", "opus"),
        ("mkv", "mkv"),
        ("mp4", "mp4"),
        ("avi", "avi"),
    ];

    for (pattern, normalized) in formats {
        if lower.contains(&format!(".{pattern}"))
            || lower.contains(&format!("[{pattern}]"))
            || lower.contains(&format!("({pattern})"))
            || lower.ends_with(&format!(" {pattern}"))
            || lower.ends_with(pattern)
        {
            return Some(normalized.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_from_extension() {
        assert_eq!(
            detect_from_extension("Some Book.epub"),
            Some("epub".to_string())
        );
        assert_eq!(
            detect_from_extension("Book Name [EPUB]"),
            Some("epub".to_string())
        );
        assert_eq!(
            detect_from_extension("Audiobook.m4a"),
            Some("m4b".to_string())
        );
        assert_eq!(
            detect_from_extension("Document.pdf"),
            Some("pdf".to_string())
        );
        assert_eq!(detect_from_extension("Movie.mkv"), Some("mkv".to_string()));
        assert_eq!(detect_from_extension("No format here"), None);
    }

    #[test]
    fn test_detect_format_from_text() {
        assert_eq!(
            detect_format_from_text(Some("book.epub"), "Some Title"),
            Some("epub".to_string())
        );
        assert_eq!(
            detect_format_from_text(None, "Book Title [EPUB]"),
            Some("epub".to_string())
        );
    }

    #[test]
    fn test_detect_from_bytes_pdf() {
        let pdf_header = b"%PDF-1.4 fake content";
        let info = detect_from_bytes(pdf_header);
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.extension, "pdf");
        assert_eq!(info.mime_type, "application/pdf");
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension("epub"), Some("application/epub+zip"));
        assert_eq!(mime_from_extension("pdf"), Some("application/pdf"));
        assert_eq!(mime_from_extension("unknown"), None);
    }
}
