//! What a stream search listed and why, kept for the debrid log, which groups the attempts
//! on its releases under it.

use std::collections::HashMap;

use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use super::attempt_log;
use crate::repositories::stream_searches::{self, SearchOption};
use nats_jobs::types::{MediaWant, StreamOption, StreamSearchRequest};

// One play asks for the same title several times within seconds: the page's own search, the
// server's prewarm, and the library page a discovered title turns into once it is added.
const REPEAT_SECS: i32 = 120;

pub struct ListedSearch {
    pub id: Uuid,
    pub title: String,
    pub options: Vec<SearchOption>,
}

impl ListedSearch {
    /// `why` holds each option's ranking reasons by option id.
    pub fn new(
        id: Uuid,
        request: &StreamSearchRequest,
        options: &[StreamOption],
        why: &mut HashMap<String, Vec<String>>,
    ) -> Self {
        Self {
            id,
            title: title(request),
            options: options
                .iter()
                .enumerate()
                .map(|(i, o)| option(i + 1, o, why.remove(&o.id).unwrap_or_default()))
                .collect(),
        }
    }

    /// Recorded before warming starts, so its probes find the search to join. False for a
    /// repeat of a recent search, which only refreshes that one: its warming already ran,
    /// and what it held back is not going to find the account any quieter now.
    pub async fn record(self, pool: &PgPool) -> bool {
        let repeat = stream_searches::refresh(pool, &self.title, &self.options, REPEAT_SECS).await;
        let (search, fresh) = match repeat {
            Ok(Some(search)) => (Ok(search), false),
            Ok(None) => (
                stream_searches::insert(pool, self.id, &self.title, &self.options).await,
                true,
            ),
            Err(e) => (Err(e), true),
        };
        match search {
            Ok(search) => attempt_log::announce_search(search),
            Err(e) => warn!(error = %e, "Failed to record a stream search"),
        }
        fresh
    }
}

fn option(rank: usize, o: &StreamOption, why: Vec<String>) -> SearchOption {
    let label = |v: Option<String>| v.map(|s| s.trim_matches('"').to_string());
    SearchOption {
        rank,
        release_key: o.release_key.clone(),
        file_id: o.files.first().map(|f| f.id),
        release_title: o.release_title.clone(),
        size: o
            .files
            .first()
            .map(|f| f.size)
            .filter(|s| *s > 0)
            .unwrap_or(o.size),
        indexer: o.indexer.clone(),
        source_type: label(serde_json::to_string(&o.source_type).ok()).unwrap_or_default(),
        group: label(o.group.and_then(|g| serde_json::to_string(&g).ok())),
        availability: label(o.availability.and_then(|a| serde_json::to_string(&a).ok())),
        total_score: o.total_score,
        why,
    }
}

fn title(request: &StreamSearchRequest) -> String {
    let numbered = |s: &str| {
        s.parse::<u32>()
            .map_or_else(|_| s.to_string(), |n| format!("{n:02}"))
    };
    match &request.media_want {
        Some(MediaWant::Movie { title, year, .. }) => match year {
            Some(y) => format!("{title} ({y})"),
            None => title.clone(),
        },
        Some(MediaWant::TvEpisode {
            show_name,
            season,
            episode,
            ..
        }) => format!("{show_name} S{}E{}", numbered(season), numbered(episode)),
        Some(MediaWant::TvSeason {
            show_name, season, ..
        }) => format!("{show_name} S{}", numbered(season)),
        _ => request.query.clone(),
    }
}
