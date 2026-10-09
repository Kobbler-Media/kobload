//! Streaming interpretation endpoint for bulk file parsing.
//!
//! This endpoint allows callers to stream file paths and receive parsed
//! interpretations in real-time. Useful for scanning large directories
//! without loading everything into memory.

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use serde::{Deserialize, Serialize};

use crate::auth::require_auth;
use crate::parsing::{
    FileClassification, FileRole, ParsedRelease, PathParsed, parse_path, parse_release,
};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/interpret", post(interpret_single))
        .route("/api/v1/interpret/stream", post(interpret_stream))
}

/// Request to interpret a single release name or path.
#[derive(Debug, Deserialize)]
pub struct InterpretRequest {
    /// The release name or file path to interpret.
    pub name: String,
    /// Media kind: "tv", "movies", "ebooks", "audiobooks", "music", "games", "software", "xxx", "misc"
    pub media_kind: String,
    /// If true, interpret as a file path with folder structure. If false, interpret as release name.
    #[serde(default)]
    pub as_path: bool,
}

/// Response containing the parsed interpretation.
#[derive(Debug, Serialize)]
pub struct InterpretResponse {
    /// The original input name/path.
    pub input: String,
    /// The parsed release information.
    pub parsed: ParsedRelease,
    /// If parsed as path, includes folder context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_parsed: Option<PathParsed>,
}

/// Interpret a single release name or path.
async fn interpret_single(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InterpretRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let response = if request.as_path {
        let path_parsed = parse_path(&request.name, &request.media_kind);
        InterpretResponse {
            input: request.name,
            parsed: path_parsed.resolved.clone(),
            path_parsed: Some(path_parsed),
        }
    } else {
        let parsed = parse_release(&request.name, &request.media_kind);
        InterpretResponse {
            input: request.name,
            parsed,
            path_parsed: None,
        }
    };

    Json(response).into_response()
}

/// Request item for streaming interpretation.
#[derive(Debug, Deserialize)]
pub struct StreamInterpretItem {
    /// The file path to interpret.
    pub path: String,
    /// Media kind for this item.
    pub media_kind: String,
    /// Optional file size in bytes.
    #[serde(default)]
    pub size: Option<i64>,
    /// Optional file extension (if not obvious from path).
    #[serde(default)]
    pub extension: Option<String>,
}

/// Response item for streaming interpretation.
#[derive(Debug, Serialize)]
pub struct StreamInterpretResponse {
    /// The original path.
    pub path: String,
    /// The parsed release information.
    pub parsed: ParsedRelease,
    /// File classification if determinable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification: Option<FileClassification>,
    /// Any error that occurred during parsing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Batch interpretation endpoint.
///
/// Accepts NDJSON (newline-delimited JSON) where each line is a `StreamInterpretItem`.
/// Returns NDJSON where each line is a `StreamInterpretResponse`.
///
/// Example request:
/// ```text
/// {"media_kind": "tv", "path": "Show Name/Season 01/S01E01 - Pilot.mkv", "size": 1500000000}
/// {"media_kind": "tv", "path": "Show Name/Season 01/S01E02 - Second.mkv", "size": 1400000000}
/// ```
///
/// Example response:
/// ```text
/// {"path": "Show Name/Season 01/S01E01 - Pilot.mkv", "parsed": {...}}
/// {"path": "Show Name/Season 01/S01E02 - Second.mkv", "parsed": {...}}
/// ```
async fn interpret_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Parse body as UTF-8 string
    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid UTF-8: {}", e)).into_response();
        }
    };

    // Process each line
    let mut output = String::new();
    for line in body_str.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<StreamInterpretItem>(line) {
            Ok(item) => {
                let path_parsed = parse_path(&item.path, &item.media_kind);

                // Create a simple classification based on the file
                let classification = classify_from_item(&item, &path_parsed.resolved);

                StreamInterpretResponse {
                    path: item.path,
                    parsed: path_parsed.resolved,
                    classification: Some(classification),
                    error: None,
                }
            }
            Err(e) => StreamInterpretResponse {
                path: line.to_string(),
                parsed: parse_release("", "misc"),
                classification: None,
                error: Some(format!("JSON parse error: {}", e)),
            },
        };

        // Serialize response to JSON line
        match serde_json::to_string(&response) {
            Ok(json) => {
                output.push_str(&json);
                output.push('\n');
            }
            Err(e) => {
                output.push_str(&format!("{{\"error\":\"{}\"}}\n", e));
            }
        }
    }

    (
        StatusCode::OK,
        [("content-type", "application/x-ndjson")],
        output,
    )
        .into_response()
}

/// Create a simple file classification from the stream item.
fn classify_from_item(item: &StreamInterpretItem, _parsed: &ParsedRelease) -> FileClassification {
    let path_lower = item.path.to_lowercase();
    let extension = item
        .extension
        .as_deref()
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| {
            std::path::Path::new(&item.path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default()
        });

    // Detect role from filename patterns
    let role = if path_lower.contains("-sample") || path_lower.contains(".sample.") {
        FileRole::Sample
    } else if path_lower.contains("-trailer") || path_lower.contains(".trailer.") {
        FileRole::Trailer
    } else if matches!(
        extension.as_str(),
        "srt" | "sub" | "idx" | "ass" | "ssa" | "vtt"
    ) {
        FileRole::Subtitle
    } else if matches!(extension.as_str(), "nfo" | "sfv" | "txt") {
        FileRole::Metadata
    } else if matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "gif" | "bmp") {
        FileRole::Artwork
    } else {
        FileRole::MainContent
    };

    FileClassification {
        role,
        parsed: None, // Would require full file list context
        confidence: 0.8,
        group_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpret_request_deserialize() {
        let json = r#"{"name": "Show.S01E05.1080p", "media_kind": "tv"}"#;
        let req: InterpretRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, "Show.S01E05.1080p");
        assert_eq!(req.media_kind, "tv");
        assert!(!req.as_path);
    }

    #[test]
    fn test_interpret_request_with_path_flag() {
        let json = r#"{"name": "Show/Season 01/S01E05.mkv", "media_kind": "tv", "as_path": true}"#;
        let req: InterpretRequest = serde_json::from_str(json).unwrap();
        assert!(req.as_path);
    }

    #[test]
    fn test_stream_item_deserialize() {
        let json = r#"{"path": "Show/S01E05.mkv", "media_kind": "tv", "size": 1500000000}"#;
        let item: StreamInterpretItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.path, "Show/S01E05.mkv");
        assert_eq!(item.media_kind, "tv");
        assert_eq!(item.size, Some(1500000000));
    }

    #[test]
    fn test_classify_sample() {
        let item = StreamInterpretItem {
            path: "Movie-sample.mkv".to_string(),
            media_kind: "movies".to_string(),
            size: None,
            extension: None,
        };
        let parsed = parse_release("Movie", "movies");
        let classification = classify_from_item(&item, &parsed);
        assert_eq!(classification.role, FileRole::Sample);
    }

    #[test]
    fn test_classify_subtitle() {
        let item = StreamInterpretItem {
            path: "Movie.en.srt".to_string(),
            media_kind: "movies".to_string(),
            size: None,
            extension: Some("srt".to_string()),
        };
        let parsed = parse_release("Movie", "movies");
        let classification = classify_from_item(&item, &parsed);
        assert_eq!(classification.role, FileRole::Subtitle);
    }

    // =========================================================================
    // Sport interpret integration tests
    // =========================================================================

    /// Helper: simulate the stream interpret pipeline for a sport file path.
    fn interpret_sport_path(path: &str) -> ParsedRelease {
        parse_path(path, "sports").resolved
    }

    #[test]
    fn test_sport_interpret_simple_round_event_with_year_folder() {
        let parsed =
            interpret_sport_path("MotoGP/2026/MotoGP - R01 - Thailand Grand Prix Race.mkv");
        let sport = parsed.media.as_sport().expect("Expected Sport variant");
        assert_eq!(sport.league_name, "MotoGP");
        assert_eq!(sport.season, Some(2026));
        assert_eq!(sport.round, Some(1));
        assert_eq!(sport.event_title.as_deref(), Some("Thailand Grand Prix"));
        assert_eq!(sport.session_type.as_deref(), Some("Race"));
    }

    #[test]
    fn test_sport_interpret_simple_round_event_qualifying() {
        let parsed =
            interpret_sport_path("MotoGP/2026/MotoGP - R02 - Portugal Grand Prix Qualifying.mkv");
        let sport = parsed.media.as_sport().expect("Expected Sport variant");
        assert_eq!(sport.league_name, "MotoGP");
        assert_eq!(sport.season, Some(2026));
        assert_eq!(sport.round, Some(2));
        assert_eq!(sport.event_title.as_deref(), Some("Portugal Grand Prix"));
        assert_eq!(sport.session_type.as_deref(), Some("Qualifying"));
    }

    #[test]
    fn test_sport_interpret_simple_round_event_f1() {
        let parsed =
            interpret_sport_path("Formula 1/2025/Formula 1 - R05 - Monaco Grand Prix Race.mkv");
        let sport = parsed.media.as_sport().expect("Expected Sport variant");
        assert_eq!(sport.league_name, "Formula 1");
        assert_eq!(sport.season, Some(2025));
        assert_eq!(sport.round, Some(5));
        assert_eq!(sport.event_title.as_deref(), Some("Monaco Grand Prix"));
        assert_eq!(sport.session_type.as_deref(), Some("Race"));
    }

    #[test]
    fn test_sport_interpret_release_name_format() {
        // Standard release name in a year folder
        let parsed =
            interpret_sport_path("MotoGP/2024/MotoGP.2024.R01.Thailand.Race.1080p.WEB-DL.mkv");
        let sport = parsed.media.as_sport().expect("Expected Sport variant");
        assert_eq!(sport.league_name, "MotoGP");
        assert_eq!(sport.season, Some(2024));
        assert_eq!(sport.round, Some(1));
        assert_eq!(sport.location.as_deref(), Some("Thailand"));
        assert_eq!(sport.session_type.as_deref(), Some("Race"));
    }

    #[test]
    fn test_sport_interpret_f1_fp_session() {
        let parsed = interpret_sport_path(
            "Formula 1/2025/Formula.1.2025.R01.Australia.FP2.1080p.WEB-DL.mkv",
        );
        let sport = parsed.media.as_sport().expect("Expected Sport variant");
        assert_eq!(sport.league_name, "Formula 1");
        assert_eq!(sport.round, Some(1));
        assert_eq!(sport.session_type.as_deref(), Some("Free Practice 2"));
    }

    #[test]
    fn test_sport_interpret_teams_format() {
        let parsed = interpret_sport_path(
            "Premier League/2024/Premier.League.2024.Arsenal.vs.Chelsea.1080p.WEB-DL.mkv",
        );
        let sport = parsed.media.as_sport().expect("Expected Sport variant");
        assert_eq!(sport.league_name, "Premier League");
        assert_eq!(sport.season, Some(2024));
        assert_eq!(sport.event_title.as_deref(), Some("Arsenal vs Chelsea"));
    }

    #[test]
    fn test_sport_interpret_year_from_folder_not_filename() {
        // Filename has no year — folder provides it
        let parsed =
            interpret_sport_path("MotoGP/2026/MotoGP - R01 - Thailand Grand Prix Race.mkv");
        let sport = parsed.media.as_sport().expect("Expected Sport variant");
        assert_eq!(sport.season, Some(2026), "Year should come from folder");
        assert_eq!(sport.year, Some(2026));
    }

    #[test]
    fn test_sport_interpret_serialization_roundtrip() {
        // Verify the parsed result serializes with "type": "sport" tag
        // (this is what kobbler receives and deserializes)
        let parsed =
            interpret_sport_path("MotoGP/2026/MotoGP - R01 - Thailand Grand Prix Race.mkv");
        let json = serde_json::to_value(&parsed).unwrap();
        let media = json.get("media").unwrap();
        assert_eq!(media.get("type").unwrap().as_str(), Some("sport"));
        assert_eq!(media.get("league_name").unwrap().as_str(), Some("MotoGP"));
        assert_eq!(media.get("round").unwrap().as_u64(), Some(1));
        assert_eq!(media.get("session_type").unwrap().as_str(), Some("Race"));
        assert_eq!(
            media.get("event_title").unwrap().as_str(),
            Some("Thailand Grand Prix")
        );
        assert_eq!(media.get("season").unwrap().as_u64(), Some(2026));
    }
}
