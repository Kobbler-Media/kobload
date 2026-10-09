//! Parse all media files in a directory through kobload's parse_path.
//!
//! Usage:
//!   cargo run -p kobload-api --bin parse-dir -- <directory> <media_kind> [--json] [--md]
//!
//! Examples:
//!   cargo run -p kobload-api --bin parse-dir -- ./test-data/TV tv --md
//!   cargo run -p kobload-api --bin parse-dir -- ./test-data/Movies movies --json --md
//!   cargo run -p kobload-api --bin parse-dir -- /mnt/books ebooks --json

use kobload_api::parsing::{ParsedMedia, parse_path};
use serde_json::json;
use std::path::{Path, PathBuf};

const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m2ts", // video
    "epub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr", // ebooks
    "mp3", "flac", "m4a", "ogg", "opus", "wav", "wma", "m4b", // audio
];

fn collect_media_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_media_files(&path));
            } else if is_media_file(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn is_media_file(path: &Path) -> bool {
    let name = path.to_string_lossy().to_lowercase();
    if name.ends_with(".kepub.epub") {
        return true;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn format_tv(tv: &kobload_api::parsing::TvParsed) -> String {
    let mut parts = vec![format!("show=`{}`", tv.show_name)];
    if let Some(s) = tv.season {
        parts.push(format!("season={s}"));
    }
    if !tv.episodes.is_empty() {
        parts.push(format!("episodes={:?}", tv.episodes));
    }
    if let Some(ep) = tv.absolute_episode {
        parts.push(format!("absolute={ep}"));
    }
    if let Some(ref t) = tv.episode_title {
        parts.push(format!("ep_title=`{t}`"));
    }
    if let Some(y) = tv.year {
        parts.push(format!("year={y}"));
    }
    if tv.is_daily {
        parts.push(format!(
            "daily, air_date={}",
            tv.air_date.as_deref().unwrap_or("?")
        ));
    }
    if tv.is_pack {
        parts.push("pack".to_string());
    }
    if tv.is_special {
        parts.push(format!("special({:?})", tv.special_type));
    }
    parts.join(", ")
}

fn format_movie(m: &kobload_api::parsing::MovieParsed) -> String {
    let mut parts = vec![format!("title=`{}`", m.title)];
    if let Some(y) = m.year {
        parts.push(format!("year={y}"));
    }
    if let Some(ref e) = m.edition {
        parts.push(format!("edition=`{e}`"));
    }
    if let Some(p) = m.part {
        parts.push(format!("part={p}"));
    }
    if m.is_3d {
        parts.push("3D".to_string());
    }
    parts.join(", ")
}

fn format_book(b: &kobload_api::parsing::BookParsed) -> String {
    let mut parts = vec![format!("title=`{}`", b.title)];
    if let Some(ref a) = b.author {
        parts.push(format!("author=`{a}`"));
    }
    if let Some(ref s) = b.series {
        parts.push(format!("series=`{s}`"));
    }
    if let Some(ref sp) = b.series_position {
        parts.push(format!("pos={sp}"));
    }
    if let Some(ref f) = b.format_hint {
        parts.push(format!("format=`{f}`"));
    }
    if b.is_collection {
        parts.push("collection".to_string());
    }
    if let Some(y) = b.year {
        parts.push(format!("year={y}"));
    }
    parts.join(", ")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!(
            "Usage: {} <directory> <media_kind> [--json] [--md]",
            args[0]
        );
        eprintln!();
        eprintln!("media_kind: tv, movies, ebooks, audiobooks, music");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --json  Write <directory>.json");
        eprintln!("  --md    Write <directory>.md");
        eprintln!("  (default: both if neither specified)");
        std::process::exit(1);
    }

    let dir = Path::new(&args[1]);
    let media_kind = &args[2];
    let want_json = args.iter().any(|a| a == "--json");
    let want_md = args.iter().any(|a| a == "--md");
    let (want_json, want_md) = if !want_json && !want_md {
        (true, true)
    } else {
        (want_json, want_md)
    };

    if !dir.exists() {
        eprintln!("Directory not found: {}", dir.display());
        std::process::exit(1);
    }

    let files = collect_media_files(dir);
    eprintln!("Found {} media files in {}", files.len(), dir.display());

    let mut md = String::new();
    md.push_str(&format!(
        "# Parse Results: {}\n\nMedia kind: `{media_kind}` | Files: {}\n\n",
        dir.display(),
        files.len()
    ));

    let mut json_results = Vec::new();

    for file in &files {
        let rel_path = file.strip_prefix(dir).unwrap_or(file);
        let rel_str = rel_path.to_string_lossy();

        let result = parse_path(&rel_str, media_kind);
        let resolved = &result.resolved;

        let media_detail = match &resolved.media {
            ParsedMedia::Tv(tv) => format_tv(tv),
            ParsedMedia::Movie(m) => format_movie(m),
            ParsedMedia::Book(b) => format_book(b),
            other => format!("{other:?}"),
        };

        md.push_str(&format!(
            "### `{rel_str}`\n- **Parsed:** {media_detail}\n- **Confidence:** {:.2}\n",
            resolved.confidence
        ));

        let ctx = &result.folder_context;
        let mut ctx_parts = Vec::new();
        if let Some(ref t) = ctx.title {
            ctx_parts.push(format!("title=`{t}`"));
        }
        if let Some(s) = ctx.season {
            ctx_parts.push(format!("season={s}"));
        }
        if let Some(y) = ctx.year {
            ctx_parts.push(format!("year={y}"));
        }
        if let Some(ref a) = ctx.artist {
            ctx_parts.push(format!("artist=`{a}`"));
        }
        if let Some(ref a) = ctx.album {
            ctx_parts.push(format!("album=`{a}`"));
        }
        if !ctx_parts.is_empty() {
            md.push_str(&format!("- **Folder context:** {}\n", ctx_parts.join(", ")));
        }
        md.push('\n');

        json_results.push(json!({
            "path": rel_str,
            "folder_context": result.folder_context,
            "file_parsed": result.file_parsed,
            "resolved": result.resolved,
        }));
    }

    let stem = dir.file_name().and_then(|n| n.to_str()).unwrap_or("output");

    if want_md {
        let md_path = dir.with_file_name(format!("{stem}-parsed.md"));
        std::fs::write(&md_path, &md).expect("Failed to write markdown");
        eprintln!("Wrote {}", md_path.display());
    }

    if want_json {
        let json_out = serde_json::to_string_pretty(&json_results).unwrap();
        let json_path = dir.with_file_name(format!("{stem}-parsed.json"));
        std::fs::write(&json_path, &json_out).expect("Failed to write JSON");
        eprintln!("Wrote {}", json_path.display());
    }
}
