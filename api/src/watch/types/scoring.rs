pub use nats_jobs::MatchTarget;

/// Result of a pattern match
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub score: f64,
    pub matched_pattern: String,
}
