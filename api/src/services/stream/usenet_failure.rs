use reqwest::StatusCode;

use crate::debrid::DebridError;

/// Damage reported for a usenet stream's bytes, by the engine before its headers or by a
/// client after a body abort; it means the copy itself is bad.
pub const DAMAGE_REASONS: &[&str] = &["missing_articles", "corrupt_articles"];

pub fn is_damage(reason: &str) -> bool {
    DAMAGE_REASONS.contains(&reason.trim().to_ascii_lowercase().as_str())
}

// kobbler's StreamFailure::classify reads these: "not cached" → NotCached, "blocked" →
// Blocked, "not a video" → NotVideo, "rate limit" → RateLimited. The engine's code stays at
// the end in parentheses for logs and the attempt log.
pub fn engine_refusal(code: &str) -> String {
    let text = match code {
        "missing_articles" => {
            "Usenet release is not cached: articles are missing on every provider"
        }
        "corrupt_articles" => {
            "Usenet release is not cached: its articles are corrupt on every provider"
        }
        "retention" => "Usenet release is not cached: it is older than every provider's retention",
        "invalid_nzb" => "Usenet release is not cached: the indexer's NZB is invalid",
        "password" => "Usenet release is blocked: its archive is password-protected",
        "unsupported" => {
            "Resolved release is not a video the engine can stream: its archive is compressed"
        }
        "unimportable" => {
            "Resolved release is not a video the engine can stream: nothing in it can be imported"
        }
        "nested_nzb" => "Resolved release is not a video: the NZB holds another NZB",
        "providers_unavailable" => "No usenet provider can serve this right now",
        _ => "The usenet engine refused this release",
    };
    format!("{text} ({code})")
}

pub fn import_failed(error: &DebridError) -> String {
    match error {
        DebridError::Status(_, code) => engine_refusal(code),
        DebridError::NotFound => "The usenet engine lost the NZB it was sent".to_string(),
        other => format!("The usenet engine is unreachable: {other}"),
    }
}

pub fn indexer_refused(status: StatusCode) -> String {
    match status.as_u16() {
        429 => format!("The indexer is rate limiting NZB grabs ({status})"),
        401 | 403 => format!("The indexer refused the NZB grab: authentication failed ({status})"),
        404 | 410 => {
            format!("Usenet release is not cached: the indexer no longer has its NZB ({status})")
        }
        _ if status.is_server_error() => {
            format!("The indexer is unavailable for NZB grabs ({status})")
        }
        _ => format!("The indexer refused the NZB grab ({status})"),
    }
}
