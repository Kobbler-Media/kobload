//! Archive handling for zip, rar, and 7z files.
//!
//! Archives are scanned during download completion to list their contents.
//! Files inside archives use the `!` delimiter in paths: `archive.zip!/inner/path.epub`

use std::io::Read;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};

use futures::future;
use futures::{Stream, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::bytes::Bytes;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::download_clients::ScannedFile;

/// Archive delimiter used in paths (e.g., `archive.zip!/inner/path.epub`)
pub const ARCHIVE_DELIMITER: &str = "!";

/// Supported archive extensions
pub const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz"];

/// Extensions that are compressed single-file wrappers (not multi-file archives)
pub const COMPRESSED_SINGLE_EXTENSIONS: &[&str] = &["gz", "bz2", "xz"];

/// Safety limits to prevent zip bombs and recursive nightmares
pub mod limits {
    /// Maximum nesting depth for archive expansion (ZIP→RAR→file is 2 levels)
    pub const MAX_NESTING_DEPTH: usize = 3;
    /// Maximum total size to extract when reassembling split RARs (500 MB)
    pub const MAX_TEMP_EXTRACTION_BYTES: u64 = 500 * 1024 * 1024;
    /// Maximum files per archive listing
    pub const MAX_FILES_PER_ARCHIVE: usize = 10_000;
}

/// A detected split RAR archive set.
///
/// Patterns detected:
/// - Old style: name.rar + name.r00, name.r01, ..., name.rNN
/// - New style: name.part1.rar, name.part2.rar, ..., name.partN.rar
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitRarSet {
    /// The main file to pass to unrar (first volume)
    pub main_file: String,
    /// Additional volume files (in order)
    pub volumes: Vec<String>,
}

impl SplitRarSet {
    /// Returns all files in this set (main file + volumes) in extraction order
    pub fn all_files(&self) -> Vec<&str> {
        std::iter::once(self.main_file.as_str())
            .chain(self.volumes.iter().map(|s| s.as_str()))
            .collect()
    }
}

/// Detect split RAR sets from a list of filenames.
///
/// Returns sets where main_file is the first volume (the one to pass to unrar)
/// and volumes contains any additional parts.
///
/// Patterns detected:
/// - Old style: name.rar + name.r00, name.r01, ..., name.rNN
/// - New style: name.part1.rar, name.part2.rar, ..., name.partN.rar
pub fn detect_split_rar_sets(filenames: &[&str]) -> Vec<SplitRarSet> {
    use std::collections::HashMap;

    let mut sets = Vec::new();

    // Group files by their base name (without extension)
    let mut rar_files: HashMap<String, Vec<&str>> = HashMap::new();

    for &filename in filenames {
        let lower = filename.to_lowercase();

        // Check for new-style split RAR first: name.partN.rar
        // (Must check before old-style since new-style also ends with .rar)
        if is_new_style_rar_part(&lower) {
            let base = get_new_style_rar_base(filename);
            rar_files.entry(base).or_default().push(filename);
        }
        // Check for old-style split RAR: name.rar or name.rXX
        else if lower.ends_with(".rar") || is_old_style_rar_volume(&lower) {
            // Get base name (everything before .rar or .rXX)
            let base = get_rar_base_name(filename);
            rar_files.entry(base).or_default().push(filename);
        }
    }

    // Process each group
    for (base, mut files) in rar_files {
        // Need at least 2 files to be a split set
        if files.len() < 2 {
            continue;
        }

        // Check if this is old-style or new-style
        let is_new_style = files
            .iter()
            .any(|f| is_new_style_rar_part(&f.to_lowercase()));

        if is_new_style {
            // Sort by part number
            files.sort_by_key(|f| get_new_style_part_number(f).unwrap_or(0));

            if !files.is_empty() {
                sets.push(SplitRarSet {
                    main_file: files[0].to_string(),
                    volumes: files[1..].iter().map(|s| s.to_string()).collect(),
                });
            }
        } else {
            // Old style: .rar is first, then .r00, .r01, etc.
            let main_file = files.iter().find(|f| f.to_lowercase().ends_with(".rar"));
            let mut volumes: Vec<_> = files
                .iter()
                .filter(|f| is_old_style_rar_volume(&f.to_lowercase()))
                .copied()
                .collect();

            // Sort volumes by number (.r00, .r01, etc.)
            volumes.sort_by_key(|f| get_old_style_volume_number(f).unwrap_or(0));

            // Only create a set if we have both a .rar and volume files
            if let Some(&main) = main_file
                && !volumes.is_empty()
            {
                sets.push(SplitRarSet {
                    main_file: main.to_string(),
                    volumes: volumes.iter().map(|s| s.to_string()).collect(),
                });
            }
        }

        // Skip files that are part of a split set from being reported as base
        // (This is implicitly handled by the grouping)
        let _ = base; // Silence unused warning
    }

    sets
}

/// Check if filename matches old-style RAR volume pattern (.r00, .r01, etc.)
fn is_old_style_rar_volume(lower_filename: &str) -> bool {
    if lower_filename.len() < 4 {
        return false;
    }
    let ext_start = lower_filename.len() - 4;
    let ext = &lower_filename[ext_start..];
    ext.starts_with(".r") && ext[2..].chars().all(|c| c.is_ascii_digit())
}

/// Check if filename matches new-style RAR part pattern (.part1.rar, .part2.rar, etc.)
fn is_new_style_rar_part(lower_filename: &str) -> bool {
    if !lower_filename.ends_with(".rar") {
        return false;
    }
    // Look for .partN.rar pattern
    let without_rar = &lower_filename[..lower_filename.len() - 4];
    if let Some(part_idx) = without_rar.rfind(".part") {
        let part_num = &without_rar[part_idx + 5..];
        return !part_num.is_empty() && part_num.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// Get base name for old-style RAR (removes .rar or .rXX extension)
fn get_rar_base_name(filename: &str) -> String {
    let lower = filename.to_lowercase();
    if lower.ends_with(".rar") || is_old_style_rar_volume(&lower) {
        filename[..filename.len() - 4].to_string()
    } else {
        filename.to_string()
    }
}

/// Get base name for new-style RAR (removes .partN.rar)
fn get_new_style_rar_base(filename: &str) -> String {
    let lower = filename.to_lowercase();
    if !lower.ends_with(".rar") {
        return filename.to_string();
    }
    let without_rar = &filename[..filename.len() - 4];
    if let Some(part_idx) = without_rar.to_lowercase().rfind(".part") {
        return without_rar[..part_idx].to_string();
    }
    filename.to_string()
}

/// Get the part number from new-style RAR filename
fn get_new_style_part_number(filename: &str) -> Option<u32> {
    let lower = filename.to_lowercase();
    if !lower.ends_with(".rar") {
        return None;
    }
    let without_rar = &lower[..lower.len() - 4];
    if let Some(part_idx) = without_rar.rfind(".part") {
        let part_num = &without_rar[part_idx + 5..];
        return part_num.parse().ok();
    }
    None
}

/// Get the volume number from old-style RAR filename (.r00 -> 0, .r01 -> 1, etc.)
fn get_old_style_volume_number(filename: &str) -> Option<u32> {
    let lower = filename.to_lowercase();
    if !is_old_style_rar_volume(&lower) {
        return None;
    }
    let ext = &lower[lower.len() - 2..];
    ext.parse().ok()
}

/// A split archive set using numeric extensions (.001, .002, etc.)
/// Covers: file.7z.001, file.zip.001, file.001, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumericSplitSet {
    /// The first volume (e.g., "file.7z.001" or "file.001")
    pub first_volume: String,
    /// All subsequent volumes in order (e.g., ["file.7z.002", "file.7z.003"])
    pub other_volumes: Vec<String>,
    /// The base archive type if known (e.g., "7z", "zip", "rar")
    pub archive_type: Option<String>,
}

impl NumericSplitSet {
    /// Get all volume files in order
    pub fn all_files(&self) -> Vec<&str> {
        std::iter::once(self.first_volume.as_str())
            .chain(self.other_volumes.iter().map(|s| s.as_str()))
            .collect()
    }
}

/// Check if filename matches numeric split pattern (.001, .002, etc.)
/// Returns Some((base_name, volume_number)) if it matches
pub fn parse_numeric_split(filename: &str) -> Option<(String, u32)> {
    let lower = filename.to_lowercase();

    // Match patterns: file.7z.001, file.zip.001, file.rar.001, or just file.001
    // The extension must be 3 digits
    if lower.len() < 4 {
        return None;
    }

    let ext = &lower[lower.len() - 4..];
    if !ext.starts_with('.') {
        return None;
    }

    let digits = &ext[1..];
    if digits.len() != 3 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let volume_num: u32 = digits.parse().ok()?;
    let base = filename[..filename.len() - 4].to_string();

    Some((base, volume_num))
}

/// Detect numeric split archive sets (file.7z.001, file.zip.001, file.001, etc.)
pub fn detect_numeric_split_sets(filenames: &[&str]) -> Vec<NumericSplitSet> {
    use std::collections::HashMap;

    // Group files by their base name
    let mut groups: HashMap<String, Vec<(&str, u32)>> = HashMap::new();

    for &filename in filenames {
        if let Some((base, vol_num)) = parse_numeric_split(filename) {
            groups.entry(base).or_default().push((filename, vol_num));
        }
    }

    let mut sets = Vec::new();

    for (base, mut files) in groups {
        // Need at least 2 files to be a split set
        if files.len() < 2 {
            continue;
        }

        // Sort by volume number
        files.sort_by_key(|(_, num)| *num);

        // First volume should be .001
        if files[0].1 != 1 {
            continue;
        }

        // Check for gaps in sequence
        let mut has_gaps = false;
        for (i, (_, num)) in files.iter().enumerate() {
            if *num != (i + 1) as u32 {
                has_gaps = true;
                break;
            }
        }
        if has_gaps {
            continue;
        }

        // Determine archive type from base name
        let archive_type = if base.to_lowercase().ends_with(".7z") {
            Some("7z".to_string())
        } else if base.to_lowercase().ends_with(".zip") {
            Some("zip".to_string())
        } else if base.to_lowercase().ends_with(".rar") {
            Some("rar".to_string())
        } else {
            None
        };

        let first_volume = files[0].0.to_string();
        let other_volumes = files[1..].iter().map(|(f, _)| f.to_string()).collect();

        sets.push(NumericSplitSet {
            first_volume,
            other_volumes,
            archive_type,
        });
    }

    sets
}

/// Check if a file extension indicates an archive
pub fn is_archive_extension(ext: &str) -> bool {
    ARCHIVE_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

/// Check if a path is a tar.* compound extension (e.g., file.tar.gz)
pub fn is_tar_compressed(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".tar.gz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tbz2")
        || lower.ends_with(".txz")
}

/// Check if a file inside an archive is itself an archive (nested).
pub fn is_nested_archive(inner_path: &str) -> bool {
    let ext = Path::new(inner_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    is_archive_extension(&ext) || is_tar_compressed(inner_path)
}

/// Check if a path represents an archive-nested file.
///
/// Unlike a simple `path.contains("!")`, this validates that the `!` delimiter
/// is preceded by a recognized archive extension (e.g., `.zip!`, `.rar!`).
/// This avoids false positives from filenames containing `!` (e.g., "Teen Titans Go!").
pub fn is_archive_nested(path: &str) -> bool {
    split_archive_path(path).is_some()
}

/// Split an archive path into (archive_path, inner_path) at the first valid
/// archive delimiter.
///
/// A valid delimiter is a `!` character immediately preceded by a recognized
/// archive extension (e.g., `foo.zip!bar.epub` → `("foo.zip", "bar.epub")`).
/// Returns None if no valid archive delimiter is found.
pub fn split_archive_path(path: &str) -> Option<(&str, &str)> {
    let mut search_from = 0;
    while let Some(pos) = path[search_from..].find(ARCHIVE_DELIMITER) {
        let abs_pos = search_from + pos;
        let before = &path[..abs_pos];
        if let Some(ext) = Path::new(before).extension().and_then(|e| e.to_str())
            && ARCHIVE_EXTENSIONS
                .iter()
                .any(|ae| ae.eq_ignore_ascii_case(ext))
        {
            return Some((before, &path[abs_pos + 1..]));
        }
        search_from = abs_pos + 1;
    }
    None
}

/// Split an archive path at the *last* valid archive delimiter.
///
/// For multi-level paths like `outer.zip!inner.rar!file.epub`, returns
/// `("outer.zip!inner.rar", "file.epub")`.
pub fn rsplit_archive_path(path: &str) -> Option<(&str, &str)> {
    let mut last_match: Option<(&str, &str)> = None;
    let mut search_from = 0;
    while let Some(pos) = path[search_from..].find(ARCHIVE_DELIMITER) {
        let abs_pos = search_from + pos;
        let before = &path[..abs_pos];
        if let Some(ext) = Path::new(before).extension().and_then(|e| e.to_str())
            && ARCHIVE_EXTENSIONS
                .iter()
                .any(|ae| ae.eq_ignore_ascii_case(ext))
        {
            last_match = Some((before, &path[abs_pos + 1..]));
        }
        search_from = abs_pos + 1;
    }
    last_match
}

/// Count the number of valid archive delimiters in a path.
///
/// For `outer.zip!inner.rar!file.epub` returns 2.
/// For `Teen Titans Go! S09/file.mkv` returns 0.
pub fn count_archive_delimiters(path: &str) -> usize {
    let mut count = 0;
    let mut search_from = 0;
    while let Some(pos) = path[search_from..].find(ARCHIVE_DELIMITER) {
        let abs_pos = search_from + pos;
        let before = &path[..abs_pos];
        if let Some(ext) = Path::new(before).extension().and_then(|e| e.to_str())
            && ARCHIVE_EXTENSIONS
                .iter()
                .any(|ae| ae.eq_ignore_ascii_case(ext))
        {
            count += 1;
        }
        search_from = abs_pos + 1;
    }
    count
}

/// List contents of an archive file
pub async fn list_archive_contents(
    archive_path: &str,
    archive_relative_path: &str,
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> Vec<ScannedFile> {
    // If path contains our delimiter, it's a nested archive that requires extraction first.
    // This function only handles direct filesystem paths. Nested archives should be handled
    // by extract_files_to_temp + list_archive_contents, or via the split RAR handlers.
    if is_archive_nested(archive_path) {
        tracing::debug!(
            archive = %archive_path,
            "Skipping nested archive path in list_archive_contents (requires extraction)"
        );
        return Vec::new();
    }

    let ext = Path::new(archive_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    if is_tar_compressed(archive_path) || ext == "tar" {
        return list_tar_contents(archive_path, archive_relative_path).await;
    }

    match ext.as_str() {
        "zip" => list_zip_contents(archive_path, archive_relative_path).await,
        "rar" => list_rar_contents(archive_path, archive_relative_path, unrar_path).await,
        "7z" => list_7z_contents(archive_path, archive_relative_path, sevenz_path).await,
        "gz" | "bz2" | "xz" => list_compressed_single(archive_path, archive_relative_path, &ext),
        _ => Vec::new(),
    }
}

/// List contents of a ZIP file (native Rust implementation)
async fn list_zip_contents(archive_path: &str, archive_relative_path: &str) -> Vec<ScannedFile> {
    let archive_path = archive_path.to_string();
    let archive_relative_path = archive_relative_path.to_string();

    // Run in blocking task since zip crate is sync
    tokio::task::spawn_blocking(move || {
        let file = match std::fs::File::open(&archive_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };

        let mut archive = match zip::ZipArchive::new(file) {
            Ok(a) => a,
            Err(_) => return Vec::new(),
        };

        let mut files = Vec::new();
        for i in 0..archive.len() {
            let file = match archive.by_index(i) {
                Ok(f) => f,
                Err(_) => continue,
            };

            // Skip directories
            if file.is_dir() {
                continue;
            }

            let inner_path = file.name().to_string();
            let size = file.size() as i64;
            let extension = Path::new(&inner_path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();

            // Build the archive path with delimiter
            let relative_path = format!(
                "{}{}{}",
                archive_relative_path, ARCHIVE_DELIMITER, inner_path
            );

            files.push(ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: format!("{}{}{}", archive_path, ARCHIVE_DELIMITER, inner_path),
                relative_path,
                size,
                extension,
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            });
        }

        files
    })
    .await
    .unwrap_or_default()
}

/// List contents of a RAR file (using unrar CLI)
async fn list_rar_contents(
    archive_path: &str,
    archive_relative_path: &str,
    unrar_path: Option<&str>,
) -> Vec<ScannedFile> {
    let unrar = unrar_path.unwrap_or("unrar");

    // Run: unrar lt archive.rar (technical list format - includes size)
    let output = match Command::new(unrar)
        .args(["lt", archive_path])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                archive = %archive_path,
                error = %e,
                "Failed to run unrar - is it installed?"
            );
            return Vec::new();
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            archive = %archive_path,
            status = ?output.status,
            stderr = %stderr,
            "unrar failed to list archive contents"
        );
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the technical listing format:
    //   Name: filename.ext
    //   Type: File
    //   Size: 12345
    //   ...
    let mut files = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_size: i64 = 0;
    let mut current_type: Option<String> = None;

    for line in stdout.lines() {
        let line = line.trim();

        if let Some(name) = line.strip_prefix("Name: ") {
            // Save previous entry if it was a file
            if let Some(ref prev_name) = current_name
                && current_type.as_deref() == Some("File")
                && !prev_name.ends_with('/')
            {
                let extension = Path::new(prev_name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();

                let relative_path = format!(
                    "{}{}{}",
                    archive_relative_path, ARCHIVE_DELIMITER, prev_name
                );

                files.push(ScannedFile {
                    id: Uuid::now_v7(),
                    absolute_path: format!("{}{}{}", archive_path, ARCHIVE_DELIMITER, prev_name),
                    relative_path,
                    size: current_size,
                    extension,
                    split_rar_volumes: None,
                    distributed_rar_sources: None,
                    distributed_numeric_sources: None,
                    numeric_split_type: None,
                    score: None,
                    classification: None,
                    file_metadata: None,
                    mime_type: None,
                });
            }
            // Start new entry
            current_name = Some(name.to_string());
            current_size = 0;
            current_type = None;
        } else if let Some(type_str) = line.strip_prefix("Type: ") {
            current_type = Some(type_str.to_string());
        } else if let Some(size_str) = line.strip_prefix("Size: ") {
            current_size = size_str.parse().unwrap_or(0);
        }
    }

    // Don't forget the last entry
    if let Some(ref name) = current_name
        && current_type.as_deref() == Some("File")
        && !name.ends_with('/')
    {
        let extension = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let relative_path = format!("{}{}{}", archive_relative_path, ARCHIVE_DELIMITER, name);

        files.push(ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: format!("{}{}{}", archive_path, ARCHIVE_DELIMITER, name),
            relative_path,
            size: current_size,
            extension,
            split_rar_volumes: None,
            distributed_rar_sources: None,
            distributed_numeric_sources: None,
            numeric_split_type: None,
            score: None,
            classification: None,
            file_metadata: None,
            mime_type: None,
        });
    }

    files
}

/// List contents of a 7z file (using 7z CLI)
async fn list_7z_contents(
    archive_path: &str,
    archive_relative_path: &str,
    sevenz_path: Option<&str>,
) -> Vec<ScannedFile> {
    let sevenz = sevenz_path.unwrap_or("7z");

    // Run: 7z l -slt archive.7z (technical list format)
    let output = match Command::new(sevenz)
        .args(["l", "-slt", archive_path])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                archive = %archive_path,
                error = %e,
                "Failed to run 7z - is it installed?"
            );
            return Vec::new();
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            archive = %archive_path,
            status = ?output.status,
            stderr = %stderr,
            "7z failed to list archive contents"
        );
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the technical listing format
    let mut files = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_size: i64 = 0;
    let mut is_dir = false;

    for line in stdout.lines() {
        let line = line.trim();

        if line.starts_with("Path = ") {
            // Save previous entry if it was a file
            if let Some(path) = current_path.take()
                && !is_dir
                && !path.is_empty()
            {
                let extension = Path::new(&path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();

                let relative_path =
                    format!("{}{}{}", archive_relative_path, ARCHIVE_DELIMITER, path);

                files.push(ScannedFile {
                    id: Uuid::now_v7(),
                    absolute_path: format!("{}{}{}", archive_path, ARCHIVE_DELIMITER, path),
                    relative_path,
                    size: current_size,
                    extension,
                    split_rar_volumes: None,
                    distributed_rar_sources: None,
                    distributed_numeric_sources: None,
                    numeric_split_type: None,
                    score: None,
                    classification: None,
                    file_metadata: None,
                    mime_type: None,
                });
            }

            current_path = Some(line.strip_prefix("Path = ").unwrap_or("").to_string());
            current_size = 0;
            is_dir = false;
        } else if line.starts_with("Size = ") {
            current_size = line
                .strip_prefix("Size = ")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if line.starts_with("Attributes = ") && line.contains('D') {
            is_dir = true;
        }
    }

    // Don't forget the last entry
    if let Some(path) = current_path
        && !is_dir
        && !path.is_empty()
    {
        let extension = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let relative_path = format!("{}{}{}", archive_relative_path, ARCHIVE_DELIMITER, path);

        files.push(ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: format!("{}{}{}", archive_path, ARCHIVE_DELIMITER, path),
            relative_path,
            size: current_size,
            extension,
            split_rar_volumes: None,
            distributed_rar_sources: None,
            distributed_numeric_sources: None,
            numeric_split_type: None,
            score: None,
            classification: None,
            file_metadata: None,
            mime_type: None,
        });
    }

    files
}

/// Chunk size for pumping extracted data. Also the most a single read holds in memory.
const STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Chunks queued between a blocking extractor and the consumer. Bounds the
/// in-flight memory of one extraction to `STREAM_CHUNK_BYTES * STREAM_CHANNEL_DEPTH`.
const STREAM_CHANNEL_DEPTH: usize = 4;

/// Extracted bytes, produced incrementally rather than buffered whole. Entries
/// can be multi-GB media files, and buffering them pinned that much RSS for the
/// life of the process (glibc rarely returns large freed arenas).
pub type ByteStream = Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

/// A stream of an archive entry that also owns the temp dirs its outer layers
/// were unpacked into, so they live until the last byte has been read.
pub struct EntryStream {
    // Declared before `_temp_dirs` so the extractor (and any child process)
    // is dropped before the files it reads are deleted.
    inner: ByteStream,
    _temp_dirs: Vec<tempfile::TempDir>,
}

impl EntryStream {
    fn new(inner: ByteStream) -> Self {
        Self {
            inner,
            _temp_dirs: Vec::new(),
        }
    }

    fn holding(inner: ByteStream, temp_dirs: Vec<tempfile::TempDir>) -> Self {
        Self {
            inner,
            _temp_dirs: temp_dirs,
        }
    }
}

impl Stream for EntryStream {
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx)
    }
}

/// Write a stream to `dest`, returning the number of bytes written.
async fn write_stream_to_path(mut stream: ByteStream, dest: &Path) -> std::io::Result<u64> {
    let mut file = tokio::fs::File::create(dest).await?;
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.flush().await?;
    Ok(written)
}

/// Concatenate `sources` in order into `dest` without loading any of them whole.
pub async fn concat_files_to_path(
    sources: &[std::path::PathBuf],
    dest: &Path,
) -> std::io::Result<u64> {
    let mut out = tokio::fs::File::create(dest).await?;
    let mut written = 0u64;
    for source in sources {
        let mut input = tokio::fs::File::open(source).await?;
        written += tokio::io::copy(&mut input, &mut out).await?;
    }
    out.flush().await?;
    Ok(written)
}

/// Hands a blocking extractor's reader to the async side in bounded chunks.
struct ChunkSink {
    opened: Option<oneshot::Sender<std::io::Result<()>>>,
    tx: mpsc::Sender<std::io::Result<Bytes>>,
}

impl ChunkSink {
    /// Pump `reader` into the channel. The first chunk is read before the
    /// open is acknowledged, so a corrupt entry or bad compressed header still
    /// surfaces as an open error (a 404) rather than a truncated 200.
    fn pump(&mut self, reader: &mut dyn Read) -> std::io::Result<()> {
        let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if let Some(opened) = self.opened.take() {
                let _ = opened.send(Ok(()));
            }
            if n == 0 {
                return Ok(());
            }
            if self
                .tx
                .blocking_send(Ok(Bytes::copy_from_slice(&buf[..n])))
                .is_err()
            {
                // Consumer went away (client disconnected); stop extracting.
                return Ok(());
            }
        }
    }
}

/// Run a sync extractor on the blocking pool and expose its output as a stream.
/// `extract` opens the entry and calls `sink.pump`; any error before the first
/// chunk is returned from here, any error after it ends the stream with `Err`.
async fn stream_blocking<F>(extract: F) -> std::io::Result<ByteStream>
where
    F: FnOnce(&mut ChunkSink) -> std::io::Result<()> + Send + 'static,
{
    let (opened_tx, opened_rx) = oneshot::channel();
    let (tx, rx) = mpsc::channel(STREAM_CHANNEL_DEPTH);

    tokio::task::spawn_blocking(move || {
        let mut sink = ChunkSink {
            opened: Some(opened_tx),
            tx,
        };
        let result = extract(&mut sink);
        match sink.opened.take() {
            Some(opened) => {
                let _ = opened.send(result);
            }
            None => {
                if let Err(e) = result {
                    let _ = sink.tx.blocking_send(Err(e));
                }
            }
        }
    });

    opened_rx
        .await
        .map_err(|_| std::io::Error::other("extraction task ended unexpectedly"))??;
    Ok(Box::pin(ReceiverStream::new(rx)))
}

/// Stream a CLI extractor's stdout. Waits for the first chunk (or exit) before
/// returning, so a missing entry or unreadable archive is still an error here;
/// a non-zero exit after data has flowed ends the stream with `Err`.
async fn stream_command(mut cmd: Command, tool: &'static str) -> std::io::Result<ByteStream> {
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Client disconnects drop the stream; don't leave unrar/7z running.
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other(format!("{tool}: stdout not captured")))?;

    let mut first = vec![0u8; STREAM_CHUNK_BYTES];
    let n = stdout.read(&mut first).await?;
    if n == 0 {
        let status = child.wait().await?;
        if !status.success() {
            return Err(std::io::Error::other(format!("{tool} extraction failed")));
        }
        return Ok(Box::pin(futures::stream::empty()));
    }
    first.truncate(n);

    let head = futures::stream::once(future::ready(Ok(Bytes::from(first))));
    let body = ReaderStream::with_capacity(stdout, STREAM_CHUNK_BYTES);
    let exit = futures::stream::once(async move {
        match child.wait().await {
            Ok(status) if status.success() => None,
            Ok(_) => Some(Err(std::io::Error::other(format!(
                "{tool} extraction failed"
            )))),
            Err(e) => Some(Err(e)),
        }
    })
    .filter_map(future::ready);

    Ok(Box::pin(head.chain(body).chain(exit)))
}

/// Stream a single file out of an archive.
pub async fn stream_file(
    archive_path: &str,
    inner_path: &str,
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> std::io::Result<EntryStream> {
    open_entry(archive_path, inner_path, unrar_path, sevenz_path)
        .await
        .map(EntryStream::new)
}

/// Extract a single file from an archive straight to `dest` on disk.
pub async fn extract_file_to_path(
    archive_path: &str,
    inner_path: &str,
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
    dest: &Path,
) -> std::io::Result<u64> {
    let stream = open_entry(archive_path, inner_path, unrar_path, sevenz_path).await?;
    write_stream_to_path(stream, dest).await
}

async fn open_entry(
    archive_path: &str,
    inner_path: &str,
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> std::io::Result<ByteStream> {
    let ext = Path::new(archive_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    if is_tar_compressed(archive_path) || ext == "tar" {
        return stream_from_tar(archive_path, inner_path).await;
    }

    match ext.as_str() {
        "zip" => stream_from_zip(archive_path, inner_path).await,
        "rar" => stream_from_rar(archive_path, inner_path, unrar_path).await,
        "7z" => stream_from_7z(archive_path, inner_path, sevenz_path).await,
        "gz" | "bz2" | "xz" => stream_decompress_single(archive_path, &ext).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported archive format",
        )),
    }
}

async fn stream_from_zip(archive_path: &str, inner_path: &str) -> std::io::Result<ByteStream> {
    let archive_path = archive_path.to_string();
    let inner_path = inner_path.to_string();

    stream_blocking(move |sink| {
        let file = std::fs::File::open(&archive_path)?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut zip_file = archive
            .by_name(&inner_path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;

        sink.pump(&mut zip_file)
    })
    .await
}

async fn stream_from_rar(
    archive_path: &str,
    inner_path: &str,
    unrar_path: Option<&str>,
) -> std::io::Result<ByteStream> {
    // unrar p -inul: print the entry to stdout, no messages
    let mut cmd = Command::new(unrar_path.unwrap_or("unrar"));
    cmd.args(["p", "-inul", archive_path, inner_path]);
    stream_command(cmd, "unrar").await
}

async fn stream_from_7z(
    archive_path: &str,
    inner_path: &str,
    sevenz_path: Option<&str>,
) -> std::io::Result<ByteStream> {
    // 7z e -so: extract the entry to stdout
    let mut cmd = Command::new(sevenz_path.unwrap_or("7z"));
    cmd.args(["e", "-so", archive_path, inner_path]);
    stream_command(cmd, "7z").await
}

/// Extract multiple files from an archive to a temp directory.
///
/// Returns (TempDir, HashMap<inner_path, extracted_path>)
/// The TempDir handle must be kept alive while using the extracted files.
pub async fn extract_files_to_temp(
    archive_path: &str,
    inner_paths: &[&str],
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> std::io::Result<(
    tempfile::TempDir,
    std::collections::HashMap<String, std::path::PathBuf>,
)> {
    use std::collections::HashMap;

    let temp_dir = tempfile::tempdir()?;
    let mut extracted = HashMap::new();

    for &inner_path in inner_paths {
        // Use just the filename, not the full inner path, to avoid directory issues
        let filename = Path::new(inner_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(inner_path);
        let dest_path = temp_dir.path().join(filename);

        extract_file_to_path(archive_path, inner_path, unrar_path, sevenz_path, &dest_path).await?;
        extracted.insert(inner_path.to_string(), dest_path);
    }

    Ok((temp_dir, extracted))
}

/// Information about a distributed split RAR set (volumes spread across multiple archives).
#[derive(Debug, Clone)]
pub struct DistributedSplitRar {
    /// The base name of the RAR (e.g., "bbgwja0" for bbgwja0.rar, bbgwja0.r00, etc.)
    pub base_name: String,
    /// Main RAR file path (archive!/file.rar)
    pub main_file_path: String,
    /// Volume file paths in order (archive!/file.r00, archive!/file.r01, etc.)
    pub volume_paths: Vec<String>,
}

impl DistributedSplitRar {
    /// Get all file paths (main + volumes) in extraction order.
    pub fn all_paths(&self) -> Vec<&str> {
        std::iter::once(self.main_file_path.as_str())
            .chain(self.volume_paths.iter().map(|s| s.as_str()))
            .collect()
    }
}

/// Detect distributed split RAR sets where volumes are spread across multiple archives.
///
/// Takes a list of files that have already been expanded (e.g., "archive.zip!/file.rar")
/// and finds cases where split RAR volumes are in different outer archives.
pub fn detect_distributed_split_rars(
    files: &[crate::download_clients::ScannedFile],
) -> Vec<DistributedSplitRar> {
    use std::collections::HashMap;

    // Group files by their inner filename (the RAR volume name)
    // Key: base name (e.g., "bbgwja0"), Value: Vec of (inner_filename, full_path)
    let mut rar_groups: HashMap<String, Vec<(String, String)>> = HashMap::new();

    for file in files {
        // Only look at files inside archives (validated archive delimiter)
        if !is_archive_nested(&file.relative_path) {
            continue;
        }

        // Extract the inner filename
        let inner_name = rsplit_archive_path(&file.relative_path)
            .map(|(_, inner)| inner)
            .unwrap_or(&file.relative_path)
            .rsplit('/')
            .next()
            .unwrap_or(&file.relative_path);

        let inner_lower = inner_name.to_lowercase();

        // Check if this is a RAR volume
        if inner_lower.ends_with(".rar") || is_old_style_rar_volume(&inner_lower) {
            let base = get_rar_base_name(inner_name);
            rar_groups
                .entry(base)
                .or_default()
                .push((inner_name.to_string(), file.absolute_path.clone()));
        }
    }

    let mut distributed_sets = Vec::new();

    for (base_name, mut files) in rar_groups {
        // Need at least 2 files to be a split set
        if files.len() < 2 {
            continue;
        }

        // Check if these files come from DIFFERENT outer archives
        let outer_archives: std::collections::HashSet<&str> = files
            .iter()
            .filter_map(|(_, path)| split_archive_path(path).map(|(outer, _)| outer))
            .collect();

        // If all files are from the same archive, skip (handled by existing logic)
        if outer_archives.len() <= 1 {
            continue;
        }

        // This is a distributed split RAR!
        // Sort files: main .rar first, then .r00, .r01, etc.
        files.sort_by_key(|(name, _)| {
            let lower = name.to_lowercase();
            if lower.ends_with(".rar") {
                (0, 0) // Main RAR comes first
            } else {
                (1, get_old_style_volume_number(name).unwrap_or(9999))
            }
        });

        // Find the main RAR file
        let main_idx = files
            .iter()
            .position(|(name, _)| name.to_lowercase().ends_with(".rar"));

        if let Some(main_idx) = main_idx {
            let (_, main_path) = files.remove(main_idx);
            let volume_paths: Vec<String> = files.into_iter().map(|(_, path)| path).collect();

            // Re-sort volumes by number
            let mut volume_paths = volume_paths;
            volume_paths.sort_by_key(|path| {
                let name = path.rsplit('/').next().unwrap_or(path);
                get_old_style_volume_number(name).unwrap_or(9999)
            });

            distributed_sets.push(DistributedSplitRar {
                base_name,
                main_file_path: main_path,
                volume_paths,
            });
        }
    }

    distributed_sets
}

/// Information about a distributed numeric split set (e.g., .7z.001 spread across multiple archives).
#[derive(Debug, Clone)]
pub struct DistributedNumericSplit {
    /// The base name (e.g., "file.7z" for file.7z.001)
    pub base_name: String,
    /// Archive type if known (e.g., "7z", "zip")
    pub archive_type: Option<String>,
    /// First volume path (archive!/file.7z.001)
    pub first_volume_path: String,
    /// Other volume paths in order
    pub other_volume_paths: Vec<String>,
}

impl DistributedNumericSplit {
    /// Get all file paths in extraction order.
    pub fn all_paths(&self) -> Vec<&str> {
        std::iter::once(self.first_volume_path.as_str())
            .chain(self.other_volume_paths.iter().map(|s| s.as_str()))
            .collect()
    }
}

/// Detect distributed numeric split sets where volumes are spread across multiple archives.
/// Handles .7z.001/.7z.002, .zip.001/.zip.002, .001/.002 patterns.
pub fn detect_distributed_numeric_splits(
    files: &[crate::download_clients::ScannedFile],
) -> Vec<DistributedNumericSplit> {
    use std::collections::HashMap;

    // Group files by their base name (without the .NNN extension)
    // Key: base name, Value: Vec of (volume_number, full_path)
    let mut groups: HashMap<String, Vec<(u32, String)>> = HashMap::new();

    for file in files {
        // Only look at files inside archives (validated archive delimiter)
        if !is_archive_nested(&file.relative_path) {
            continue;
        }

        // Extract the inner filename
        let inner_name = rsplit_archive_path(&file.relative_path)
            .map(|(_, inner)| inner)
            .unwrap_or(&file.relative_path)
            .rsplit('/')
            .next()
            .unwrap_or(&file.relative_path);

        // Check if this is a numeric split
        if let Some((base, vol_num)) = parse_numeric_split(inner_name) {
            groups
                .entry(base)
                .or_default()
                .push((vol_num, file.absolute_path.clone()));
        }
    }

    let mut distributed_sets = Vec::new();

    for (base_name, mut files) in groups {
        // Need at least 2 files to be a split set
        if files.len() < 2 {
            continue;
        }

        // Check if these files come from DIFFERENT outer archives
        let outer_archives: std::collections::HashSet<&str> = files
            .iter()
            .filter_map(|(_, path)| split_archive_path(path).map(|(outer, _)| outer))
            .collect();

        // If all files are from the same archive, skip (handled by existing logic)
        if outer_archives.len() <= 1 {
            continue;
        }

        // Sort by volume number
        files.sort_by_key(|(num, _)| *num);

        // First volume should be .001
        if files[0].0 != 1 {
            continue;
        }

        // Check for gaps in sequence
        let mut has_gaps = false;
        for (i, (num, _)) in files.iter().enumerate() {
            if *num != (i + 1) as u32 {
                has_gaps = true;
                break;
            }
        }
        if has_gaps {
            continue;
        }

        // Determine archive type from base name
        let archive_type = if base_name.to_lowercase().ends_with(".7z") {
            Some("7z".to_string())
        } else if base_name.to_lowercase().ends_with(".zip") {
            Some("zip".to_string())
        } else if base_name.to_lowercase().ends_with(".rar") {
            Some("rar".to_string())
        } else {
            None
        };

        let first_volume_path = files[0].1.clone();
        let other_volume_paths = files[1..].iter().map(|(_, path)| path.clone()).collect();

        distributed_sets.push(DistributedNumericSplit {
            base_name,
            archive_type,
            first_volume_path,
            other_volume_paths,
        });
    }

    distributed_sets
}

/// Extract files from multiple different archives to a temp directory.
///
/// Unlike `extract_files_to_temp` which extracts multiple files from ONE archive,
/// this extracts one file each from MULTIPLE archives (for distributed split RARs).
pub async fn extract_from_multiple_archives_to_temp(
    archive_inner_paths: &[&str], // Each is "archive.zip!/inner/file.rar"
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> std::io::Result<(
    tempfile::TempDir,
    std::collections::HashMap<String, std::path::PathBuf>,
)> {
    use std::collections::HashMap;

    let temp_dir = tempfile::tempdir()?;
    let mut extracted = HashMap::new();

    for &full_path in archive_inner_paths {
        // Split into archive and inner path
        let Some((archive_path, inner_path)) = split_archive_path(full_path) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Invalid archive path: {full_path}"),
            ));
        };

        // Use just the filename
        let filename = Path::new(inner_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(inner_path);
        let dest_path = temp_dir.path().join(filename);

        extract_file_to_path(archive_path, inner_path, unrar_path, sevenz_path, &dest_path).await?;
        extracted.insert(full_path.to_string(), dest_path);
    }

    Ok((temp_dir, extracted))
}

/// Stream a file out of a distributed split RAR (volumes spread across multiple archives).
///
/// Takes the final file path (e.g., "dir/a.zip!/file.rar!/book.epub") and the list
/// of all source archive paths containing RAR volumes.
pub async fn stream_from_distributed_rar(
    final_path: &str,
    distributed_sources: &[String],
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> std::io::Result<EntryStream> {
    if distributed_sources.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "No distributed sources provided",
        ));
    }

    // Extract the inner file path (the path within the RAR)
    // final_path is like "dir/a.zip!/file.rar!/book.epub"
    // We need to get "book.epub" (or deeper path)
    let parts: Vec<&str> = final_path.split(ARCHIVE_DELIMITER).collect();
    if parts.len() < 3 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Invalid distributed RAR path: {final_path}"),
        ));
    }

    // The inner file is everything after the RAR delimiter
    // parts[0] = "dir/a.zip", parts[1] = "file.rar", parts[2..] = path inside RAR
    let inner_file_path = parts[2..].join(ARCHIVE_DELIMITER);

    // Extract all RAR volumes from their respective archives to temp
    let source_refs: Vec<&str> = distributed_sources.iter().map(|s| s.as_str()).collect();
    let (temp_dir, extracted) =
        extract_from_multiple_archives_to_temp(&source_refs, unrar_path, sevenz_path).await?;

    // Find the main RAR file (the .rar, not .r00, etc.)
    let main_rar_path = extracted
        .iter()
        .find(|(src, _)| src.to_lowercase().ends_with(".rar"))
        .map(|(_, path)| path.to_string_lossy().into_owned());

    let Some(main_rar_path) = main_rar_path else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No main RAR file found in distributed sources",
        ));
    };

    let stream = open_entry(&main_rar_path, &inner_file_path, unrar_path, sevenz_path).await?;
    Ok(EntryStream::holding(stream, vec![temp_dir]))
}

/// Stream a file out of a distributed numeric split archive (e.g., .7z.001 spread across archives).
///
/// Takes the final file path and the list of all source archive paths containing split volumes.
pub async fn stream_from_distributed_numeric_split(
    final_path: &str,
    distributed_sources: &[String],
    archive_type: Option<&str>,
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> std::io::Result<EntryStream> {
    if distributed_sources.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "No distributed sources provided",
        ));
    }

    // Extract the inner file path (the path within the reassembled archive)
    let parts: Vec<&str> = final_path.split(ARCHIVE_DELIMITER).collect();
    if parts.len() < 3 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Invalid distributed numeric split path: {final_path}"),
        ));
    }

    // parts[0] = "dir/a.zip", parts[1] = "file.7z.001", parts[2..] = path inside archive
    let inner_file_path = parts[2..].join(ARCHIVE_DELIMITER);

    // Extract all split volumes from their respective archives to temp
    let source_refs: Vec<&str> = distributed_sources.iter().map(|s| s.as_str()).collect();
    let (temp_dir, extracted) =
        extract_from_multiple_archives_to_temp(&source_refs, unrar_path, sevenz_path).await?;

    // Sort extracted files by volume number
    let mut volume_paths: Vec<_> = extracted
        .iter()
        .filter_map(|(src, path)| {
            let filename = src.rsplit('/').next().unwrap_or(src);
            parse_numeric_split(filename).map(|(_, num)| (num, path.clone()))
        })
        .collect();
    volume_paths.sort_by_key(|(num, _)| *num);

    if volume_paths.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No split volumes found",
        ));
    }

    // Reassemble the split archive on disk
    let paths: Vec<_> = volume_paths.into_iter().map(|(_, p)| p).collect();
    let archive_ext = archive_type.unwrap_or("bin");
    let reassembled_path = temp_dir.path().join(format!("reassembled.{archive_ext}"));
    concat_files_to_path(&paths, &reassembled_path).await?;

    let stream = open_entry(
        &reassembled_path.to_string_lossy(),
        &inner_file_path,
        unrar_path,
        sevenz_path,
    )
    .await?;
    Ok(EntryStream::holding(stream, vec![temp_dir]))
}

/// Stream a file through multiple archive layers.
///
/// Handles paths like "outer.zip!/inner.rar!/file.epub": each intermediate
/// archive is unpacked to a temp file (archives need a seekable file, not a
/// stream), and only the innermost entry is streamed.
/// Also handles split RAR reassembly when sibling volumes are provided.
pub async fn stream_nested_file(
    path: &str,
    split_rar_volumes: Option<&[String]>,
    unrar_path: Option<&str>,
    sevenz_path: Option<&str>,
) -> std::io::Result<EntryStream> {
    let parts: Vec<&str> = path.split(ARCHIVE_DELIMITER).collect();

    if parts.len() < 2 {
        // Not an archive path, read directly
        let file = tokio::fs::File::open(path).await?;
        return Ok(EntryStream::new(Box::pin(ReaderStream::new(file))));
    }

    let last = parts.len() - 1;
    let mut temp_dirs: Vec<tempfile::TempDir> = Vec::new();
    let mut current_archive = parts[0].to_string();
    let mut next = 1;

    // A split RAR in the outermost archive can only be read by unrar with all
    // of its volumes alongside it.
    if last > 1
        && is_split_rar_main_file(parts[1])
        && let Some(volumes) = split_rar_volumes
    {
        let mut all_inner_paths: Vec<&str> = vec![parts[1]];
        all_inner_paths.extend(volumes.iter().map(|s| s.as_str()));

        let (temp_dir, extracted) =
            extract_files_to_temp(parts[0], &all_inner_paths, unrar_path, sevenz_path).await?;

        current_archive = extracted
            .get(parts[1])
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "main RAR file not extracted")
            })?
            .to_string_lossy()
            .into_owned();
        temp_dirs.push(temp_dir);
        next = 2;
    }

    for &inner_path in &parts[next..last] {
        // The temp file's extension is how the next level picks its extractor
        let ext = Path::new(inner_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let temp_dir = tempfile::tempdir()?;
        let temp_archive_path = temp_dir.path().join(format!("archive.{ext}"));
        extract_file_to_path(
            &current_archive,
            inner_path,
            unrar_path,
            sevenz_path,
            &temp_archive_path,
        )
        .await?;

        current_archive = temp_archive_path.to_string_lossy().into_owned();
        temp_dirs.push(temp_dir);
    }

    let stream = open_entry(&current_archive, parts[last], unrar_path, sevenz_path).await?;
    Ok(EntryStream::holding(stream, temp_dirs))
}

/// Check if a filename looks like the main file of a split RAR set
fn is_split_rar_main_file(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    // Old style: ends with .rar (and there might be .r00, .r01 siblings)
    // New style: ends with .part1.rar
    lower.ends_with(".rar")
}

/// List contents of a RAR archive by path (public version that handles archive paths)
pub async fn list_rar_contents_by_path(
    rar_path: &std::path::Path,
    prefix: &str,
    unrar_path: Option<&str>,
) -> Vec<ScannedFile> {
    list_rar_contents(&rar_path.to_string_lossy(), prefix, unrar_path).await
}

/// List contents of a tar archive (optionally compressed with gz/bz2/xz).
async fn list_tar_contents(archive_path: &str, archive_relative_path: &str) -> Vec<ScannedFile> {
    let archive_path = archive_path.to_string();
    let archive_relative_path = archive_relative_path.to_string();

    tokio::task::spawn_blocking(move || {
        let file = match std::fs::File::open(&archive_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };

        let lower = archive_path.to_lowercase();
        let reader: Box<dyn Read> = if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Box::new(flate2::read::GzDecoder::new(file))
        } else if lower.ends_with(".tar.bz2") || lower.ends_with(".tbz2") {
            Box::new(bzip2::read::BzDecoder::new(file))
        } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
            Box::new(liblzma::read::XzDecoder::new(file))
        } else {
            Box::new(file)
        };

        let mut archive = tar::Archive::new(reader);
        let entries = match archive.entries() {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut files = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let header = entry.header();
            if header.entry_type().is_dir() {
                continue;
            }

            let inner_path = match entry.path() {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => continue,
            };

            let size = header.size().unwrap_or(0) as i64;
            let extension = Path::new(&inner_path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();

            let relative_path = format!(
                "{}{}{}",
                archive_relative_path, ARCHIVE_DELIMITER, inner_path
            );

            files.push(ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: format!("{}{}{}", archive_path, ARCHIVE_DELIMITER, inner_path),
                relative_path,
                size,
                extension,
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            });
        }

        files
    })
    .await
    .unwrap_or_default()
}

/// Stream a file out of a tar archive (optionally compressed with gz/bz2/xz).
async fn stream_from_tar(archive_path: &str, inner_path: &str) -> std::io::Result<ByteStream> {
    let archive_path = archive_path.to_string();
    let inner_path = inner_path.to_string();

    stream_blocking(move |sink| {
        let file = std::fs::File::open(&archive_path)?;

        let lower = archive_path.to_lowercase();
        let reader: Box<dyn Read> = if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Box::new(flate2::read::GzDecoder::new(file))
        } else if lower.ends_with(".tar.bz2") || lower.ends_with(".tbz2") {
            Box::new(bzip2::read::BzDecoder::new(file))
        } else if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
            Box::new(liblzma::read::XzDecoder::new(file))
        } else {
            Box::new(file)
        };

        let mut archive = tar::Archive::new(reader);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_string_lossy().to_string();
            if path == inner_path {
                return sink.pump(&mut entry);
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found in tar archive",
        ))
    })
    .await
}

/// For compressed single files (file.gz, file.bz2, file.xz), report the inner file.
fn list_compressed_single(
    archive_path: &str,
    archive_relative_path: &str,
    _ext: &str,
) -> Vec<ScannedFile> {
    // The inner file name is the archive name without the compression extension
    let inner_name = Path::new(archive_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();

    let extension = Path::new(&inner_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let relative_path = format!(
        "{}{}{}",
        archive_relative_path, ARCHIVE_DELIMITER, inner_name
    );

    vec![ScannedFile {
        id: Uuid::now_v7(),
        absolute_path: format!("{}{}{}", archive_path, ARCHIVE_DELIMITER, inner_name),
        relative_path,
        size: 0, // Unknown without decompressing
        extension,
        split_rar_volumes: None,
        distributed_rar_sources: None,
        distributed_numeric_sources: None,
        numeric_split_type: None,
        score: None,
        classification: None,
        file_metadata: None,
        mime_type: None,
    }]
}

/// Stream the decompressed contents of a single-file archive (file.gz, file.bz2, file.xz).
async fn stream_decompress_single(archive_path: &str, ext: &str) -> std::io::Result<ByteStream> {
    let archive_path = archive_path.to_string();
    let ext = ext.to_string();

    stream_blocking(move |sink| {
        let file = std::fs::File::open(&archive_path)?;

        match ext.as_str() {
            "gz" => sink.pump(&mut flate2::read::GzDecoder::new(file)),
            "bz2" => sink.pump(&mut bzip2::read::BzDecoder::new(file)),
            "xz" => sink.pump(&mut liblzma::read::XzDecoder::new(file)),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("decompression not supported for .{ext}"),
            )),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_archive_extension() {
        assert!(is_archive_extension("zip"));
        assert!(is_archive_extension("ZIP"));
        assert!(is_archive_extension("rar"));
        assert!(is_archive_extension("7z"));
        assert!(is_archive_extension("tar"));
        assert!(is_archive_extension("gz"));
        assert!(is_archive_extension("bz2"));
        assert!(is_archive_extension("xz"));
        assert!(!is_archive_extension("epub"));
        assert!(!is_archive_extension("pdf"));
    }

    #[test]
    fn test_is_tar_compressed() {
        assert!(is_tar_compressed("file.tar.gz"));
        assert!(is_tar_compressed("file.tar.bz2"));
        assert!(is_tar_compressed("file.tar.xz"));
        assert!(is_tar_compressed("file.tgz"));
        assert!(!is_tar_compressed("file.tar"));
        assert!(!is_tar_compressed("file.gz"));
    }

    #[test]
    fn test_is_nested_archive() {
        assert!(is_nested_archive("inner.zip"));
        assert!(is_nested_archive("inner.tar.gz"));
        assert!(!is_nested_archive("book.epub"));
    }

    #[test]
    fn test_split_archive_path() {
        assert_eq!(
            split_archive_path("archive.zip!/inner/path.epub"),
            Some(("archive.zip", "/inner/path.epub"))
        );
        assert_eq!(split_archive_path("regular/path.epub"), None);

        // Archive extensions
        assert_eq!(
            split_archive_path("file.rar!content.mkv"),
            Some(("file.rar", "content.mkv"))
        );
        assert_eq!(
            split_archive_path("file.7z!content.mkv"),
            Some(("file.7z", "content.mkv"))
        );

        // Full paths with directories
        assert_eq!(
            split_archive_path("/mnt/downloads/release/archive.zip!video.mkv"),
            Some(("/mnt/downloads/release/archive.zip", "video.mkv"))
        );
    }

    #[test]
    fn test_split_archive_path_exclamation_in_filename() {
        // "Teen Titans Go!" has a `!` but it's NOT an archive delimiter
        assert_eq!(
            split_archive_path("Teen Titans Go! S09E39 Torso Soap.mkv"),
            None
        );
        assert_eq!(
            split_archive_path(
                "/mnt/download/nzb/complete/Teen Titans Go! S09E39/Teen Titans Go! S09E39.mkv"
            ),
            None
        );

        // Exclamation in directory name, no archive
        assert_eq!(split_archive_path("Wow! What A Show/episode.mkv"), None);

        // Exclamation in filename AND a real archive delimiter
        assert_eq!(
            split_archive_path("Teen Titans Go!/archive.zip!video.mkv"),
            Some(("Teen Titans Go!/archive.zip", "video.mkv"))
        );
    }

    #[test]
    fn test_is_archive_nested() {
        assert!(is_archive_nested("archive.zip!file.epub"));
        assert!(is_archive_nested("dir/archive.rar!file.mkv"));
        assert!(!is_archive_nested("regular/path.epub"));
        assert!(!is_archive_nested("Teen Titans Go! S09E39.mkv"));
        assert!(!is_archive_nested(
            "/mnt/download/Teen Titans Go! S09E39/file.mkv"
        ));
    }

    #[test]
    fn test_rsplit_archive_path() {
        // Single level — same as split_archive_path
        assert_eq!(
            rsplit_archive_path("archive.zip!file.epub"),
            Some(("archive.zip", "file.epub"))
        );

        // Multi-level — splits at LAST archive delimiter
        assert_eq!(
            rsplit_archive_path("outer.zip!inner.rar!file.epub"),
            Some(("outer.zip!inner.rar", "file.epub"))
        );

        // No archive delimiter
        assert_eq!(rsplit_archive_path("regular/path.epub"), None);

        // Exclamation in filename — not an archive delimiter
        assert_eq!(rsplit_archive_path("Teen Titans Go! S09.mkv"), None);
    }

    #[test]
    fn test_count_archive_delimiters() {
        assert_eq!(count_archive_delimiters("regular/path.epub"), 0);
        assert_eq!(count_archive_delimiters("archive.zip!file.epub"), 1);
        assert_eq!(count_archive_delimiters("outer.zip!inner.rar!file.epub"), 2);
        // Exclamation in filename doesn't count
        assert_eq!(count_archive_delimiters("Teen Titans Go! S09E39.mkv"), 0);
        // Mix: exclamation in name + real archive delimiter
        assert_eq!(
            count_archive_delimiters("Teen Titans Go!/archive.zip!file.epub"),
            1
        );
    }

    #[test]
    fn test_detect_split_rar_no_volumes() {
        // Single .rar without .r00 files should return empty
        let files = vec!["book.rar", "readme.txt"];
        assert!(detect_split_rar_sets(&files).is_empty());
    }

    #[test]
    fn test_detect_split_rar_single_rar_not_split() {
        // A single .rar file with no volumes is not a split set
        let files = vec!["archive.rar"];
        assert!(detect_split_rar_sets(&files).is_empty());
    }

    #[test]
    fn test_detect_split_rar_old_style() {
        // Old style: name.rar + name.r00, r01, ...
        let files = vec![
            "bbgwja0.rar",
            "bbgwja0.r00",
            "bbgwja0.r01",
            "bbgwja0.r02",
            "readme.txt",
        ];
        let sets = detect_split_rar_sets(&files);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].main_file, "bbgwja0.rar");
        assert_eq!(
            sets[0].volumes,
            vec!["bbgwja0.r00", "bbgwja0.r01", "bbgwja0.r02"]
        );
    }

    #[test]
    fn test_detect_split_rar_old_style_many_volumes() {
        // Old style with double-digit volume numbers
        let files = vec![
            "archive.rar",
            "archive.r00",
            "archive.r01",
            "archive.r02",
            "archive.r03",
            "archive.r04",
            "archive.r05",
            "archive.r06",
            "archive.r07",
            "archive.r08",
            "archive.r09",
            "archive.r10",
        ];
        let sets = detect_split_rar_sets(&files);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].main_file, "archive.rar");
        assert_eq!(sets[0].volumes.len(), 11);
        // Verify ordering
        assert_eq!(sets[0].volumes[0], "archive.r00");
        assert_eq!(sets[0].volumes[10], "archive.r10");
    }

    #[test]
    fn test_detect_split_rar_new_style() {
        // New style: name.part1.rar, part2.rar, ...
        let files = vec!["book.part1.rar", "book.part2.rar", "book.part3.rar"];
        let sets = detect_split_rar_sets(&files);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].main_file, "book.part1.rar");
        assert_eq!(sets[0].volumes, vec!["book.part2.rar", "book.part3.rar"]);
    }

    #[test]
    fn test_detect_split_rar_new_style_unordered() {
        // New style parts provided out of order
        let files = vec!["data.part3.rar", "data.part1.rar", "data.part2.rar"];
        let sets = detect_split_rar_sets(&files);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].main_file, "data.part1.rar");
        assert_eq!(sets[0].volumes, vec!["data.part2.rar", "data.part3.rar"]);
    }

    #[test]
    fn test_detect_split_rar_multiple_sets() {
        // Multiple independent split RAR sets
        let files = vec![
            "set_a.rar",
            "set_a.r00",
            "set_a.r01",
            "set_b.part1.rar",
            "set_b.part2.rar",
        ];
        let sets = detect_split_rar_sets(&files);
        assert_eq!(sets.len(), 2);

        // Find each set by main file
        let set_a = sets.iter().find(|s| s.main_file == "set_a.rar").unwrap();
        let set_b = sets
            .iter()
            .find(|s| s.main_file == "set_b.part1.rar")
            .unwrap();

        assert_eq!(set_a.volumes, vec!["set_a.r00", "set_a.r01"]);
        assert_eq!(set_b.volumes, vec!["set_b.part2.rar"]);
    }

    #[test]
    fn test_detect_split_rar_case_insensitive() {
        // Mixed case should still be detected
        let files = vec!["Archive.RAR", "Archive.R00", "Archive.R01"];
        let sets = detect_split_rar_sets(&files);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].main_file, "Archive.RAR");
    }

    #[test]
    fn test_split_rar_set_all_files() {
        let set = SplitRarSet {
            main_file: "test.rar".to_string(),
            volumes: vec!["test.r00".to_string(), "test.r01".to_string()],
        };
        let all = set.all_files();
        assert_eq!(all, vec!["test.rar", "test.r00", "test.r01"]);
    }

    #[test]
    fn test_is_old_style_rar_volume() {
        assert!(is_old_style_rar_volume(".r00"));
        assert!(is_old_style_rar_volume("file.r00"));
        assert!(is_old_style_rar_volume("file.r99"));
        assert!(!is_old_style_rar_volume("file.rar"));
        assert!(!is_old_style_rar_volume("file.rxx"));
        assert!(!is_old_style_rar_volume("file.r0")); // Too short
    }

    #[test]
    fn test_is_new_style_rar_part() {
        assert!(is_new_style_rar_part("file.part1.rar"));
        assert!(is_new_style_rar_part("file.part10.rar"));
        assert!(is_new_style_rar_part("name.with.dots.part2.rar"));
        assert!(!is_new_style_rar_part("file.rar")); // No part number
        assert!(!is_new_style_rar_part("file.partx.rar")); // Non-numeric part
        assert!(!is_new_style_rar_part("file.part1.zip")); // Not RAR
    }

    #[test]
    fn test_detect_distributed_split_rars() {
        use crate::download_clients::ScannedFile;
        use uuid::Uuid;

        // Simulate the BitBook release pattern: split RAR distributed across multiple ZIPs
        let files = vec![
            ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: "/dl/book/a.zip!/bbgwja0.rar".to_string(),
                relative_path: "book/a.zip!/bbgwja0.rar".to_string(),
                size: 10_000_000,
                extension: "rar".to_string(),
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            },
            ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: "/dl/book/b.zip!/bbgwja0.r00".to_string(),
                relative_path: "book/b.zip!/bbgwja0.r00".to_string(),
                size: 10_000_000,
                extension: "r00".to_string(),
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            },
            ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: "/dl/book/c.zip!/bbgwja0.r01".to_string(),
                relative_path: "book/c.zip!/bbgwja0.r01".to_string(),
                size: 10_000_000,
                extension: "r01".to_string(),
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            },
            // Include a non-RAR file to ensure it's ignored
            ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: "/dl/book/a.zip!/readme.txt".to_string(),
                relative_path: "book/a.zip!/readme.txt".to_string(),
                size: 1000,
                extension: "txt".to_string(),
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            },
        ];

        let sets = detect_distributed_split_rars(&files);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].base_name, "bbgwja0");
        assert_eq!(sets[0].main_file_path, "/dl/book/a.zip!/bbgwja0.rar");
        assert_eq!(sets[0].volume_paths.len(), 2);
        assert!(
            sets[0]
                .volume_paths
                .contains(&"/dl/book/b.zip!/bbgwja0.r00".to_string())
        );
        assert!(
            sets[0]
                .volume_paths
                .contains(&"/dl/book/c.zip!/bbgwja0.r01".to_string())
        );
    }

    #[test]
    fn test_detect_distributed_split_rars_same_archive_ignored() {
        use crate::download_clients::ScannedFile;
        use uuid::Uuid;

        // Split RAR in same archive should NOT be detected as distributed
        // (it's handled by the regular split RAR logic)
        let files = vec![
            ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: "/dl/book/archive.zip!/bbgwja0.rar".to_string(),
                relative_path: "book/archive.zip!/bbgwja0.rar".to_string(),
                size: 10_000_000,
                extension: "rar".to_string(),
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            },
            ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: "/dl/book/archive.zip!/bbgwja0.r00".to_string(),
                relative_path: "book/archive.zip!/bbgwja0.r00".to_string(),
                size: 10_000_000,
                extension: "r00".to_string(),
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            },
        ];

        let sets = detect_distributed_split_rars(&files);
        assert_eq!(sets.len(), 0); // Should not detect - all in same archive
    }

    #[test]
    fn test_parse_numeric_split() {
        // Standard .001 patterns
        assert_eq!(
            parse_numeric_split("file.7z.001"),
            Some(("file.7z".to_string(), 1))
        );
        assert_eq!(
            parse_numeric_split("file.7z.002"),
            Some(("file.7z".to_string(), 2))
        );
        assert_eq!(
            parse_numeric_split("file.zip.001"),
            Some(("file.zip".to_string(), 1))
        );
        assert_eq!(
            parse_numeric_split("archive.001"),
            Some(("archive".to_string(), 1))
        );
        assert_eq!(
            parse_numeric_split("file.rar.001"),
            Some(("file.rar".to_string(), 1))
        );

        // Non-matching patterns
        assert_eq!(parse_numeric_split("file.7z"), None); // No numeric ext
        assert_eq!(parse_numeric_split("file.01"), None); // Only 2 digits
        assert_eq!(parse_numeric_split("file.0001"), None); // 4 digits
        assert_eq!(parse_numeric_split("file.abc"), None); // Not numeric
    }

    #[test]
    fn test_detect_numeric_split_sets() {
        let files = vec![
            "archive.7z.001",
            "archive.7z.002",
            "archive.7z.003",
            "other.zip.001",
            "other.zip.002",
            "readme.txt",
        ];

        let sets = detect_numeric_split_sets(&files);
        assert_eq!(sets.len(), 2);

        let sevenz_set = sets
            .iter()
            .find(|s| s.archive_type == Some("7z".to_string()));
        assert!(sevenz_set.is_some());
        let sevenz_set = sevenz_set.unwrap();
        assert_eq!(sevenz_set.first_volume, "archive.7z.001");
        assert_eq!(
            sevenz_set.other_volumes,
            vec!["archive.7z.002", "archive.7z.003"]
        );

        let zip_set = sets
            .iter()
            .find(|s| s.archive_type == Some("zip".to_string()));
        assert!(zip_set.is_some());
        let zip_set = zip_set.unwrap();
        assert_eq!(zip_set.first_volume, "other.zip.001");
        assert_eq!(zip_set.other_volumes, vec!["other.zip.002"]);
    }

    #[test]
    fn test_detect_numeric_split_sets_missing_001() {
        // If .001 is missing, should not detect as a set
        let files = vec!["archive.7z.002", "archive.7z.003"];
        let sets = detect_numeric_split_sets(&files);
        assert_eq!(sets.len(), 0);
    }

    #[test]
    fn test_detect_numeric_split_sets_gap_in_sequence() {
        // If there's a gap, should not detect
        let files = vec!["archive.7z.001", "archive.7z.003"]; // Missing .002
        let sets = detect_numeric_split_sets(&files);
        assert_eq!(sets.len(), 0);
    }
}
