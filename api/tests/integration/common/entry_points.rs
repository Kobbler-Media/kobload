//! Drives one want through every path that scores releases for it, so a test
//! can show a matching rule reaches all of them.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::stream_fixtures::hash;
use super::tv_watch::{TvWatch, usenet_release};
use kobload_api::jobs::handlers::{SearchGates, backlog_search_pattern};
use kobload_api::repositories::watch as watch_repo;
use kobload_api::watch::holds::process_expired_holds;
use kobload_api::watch::{Release, process_releases};

const HELD_GUID: &str = "held-before-the-final-search";

#[derive(Clone, Copy, Debug)]
pub enum Grabber {
    Rss,
    Backlog,
    HoldExpiry,
    WantRoute,
}

impl Grabber {
    pub const ALL: [Self; 4] = [Self::Rss, Self::Backlog, Self::HoldExpiry, Self::WantRoute];

    pub fn name(self) -> &'static str {
        match self {
            Self::Rss => "rss",
            Self::Backlog => "backlog",
            Self::HoldExpiry => "hold expiry",
            Self::WantRoute => "want",
        }
    }
}

pub fn torrent(title: &str, hash_char: char) -> Release {
    let mut release = usenet_release(title, title);
    let info_hash = hash(hash_char);
    release.protocol = "torrent".to_string();
    release.download_url = None;
    release.magnet_url = Some(format!("magnet:?xt=urn:btih:{info_hash}"));
    release.info_hash = Some(info_hash);
    release
}

/// These paths grab one release per want, so each call makes a want of its
/// own and offers `release` as the only thing the indexer has.
pub async fn grab_alone(
    tv: &TvWatch,
    grabber: Grabber,
    want: &nats_jobs::MediaWant,
    release: &Release,
) -> Vec<String> {
    tv.indexer.set_search_results(vec![release.clone()]);
    let state = &tv.server.app_state;
    let pattern_id = match grabber {
        Grabber::Rss => {
            let pattern = tv.pattern(want).await;
            process_releases(
                state,
                &tv.source,
                std::slice::from_ref(release),
                std::slice::from_ref(&pattern),
            )
            .await;
            pattern.id
        }
        Grabber::Backlog => {
            let pattern = tv.pattern(want).await;
            backlog_search_pattern(state, &pattern, SearchGates::AUTOMATIC).await;
            pattern.id
        }
        Grabber::HoldExpiry => {
            let pattern = tv.pattern(want).await;
            hold_unrelated_release(tv, pattern.id).await;
            process_expired_holds(state).await;
            pattern.id
        }
        Grabber::WantRoute => {
            let response = tv
                .server
                .admin_client()
                .post_json(
                    "/api/v1/want",
                    &json!({
                        "source_system": "kobbler",
                        "source_id": Uuid::now_v7().to_string(),
                        "callback_url": "http://localhost:1111/callback",
                        "patterns": kobload_api::media::search_terms_from_media_want(want),
                        "media_want": want,
                        "media_type": "tv",
                        "auto_download": true,
                        "min_score": 0.75,
                    }),
                )
                .await;
            response.assert_ok();
            let body: Value = response.json();
            body["watch_pattern_id"].as_str().unwrap().parse().unwrap()
        }
    };
    let grabbed = tv
        .grabbed(pattern_id)
        .await
        .into_iter()
        .filter(|guid| !guid.starts_with(HELD_GUID))
        .collect();
    // The next want searches the same query, which would otherwise be served
    // this release from the search cache.
    sqlx::query("DELETE FROM search_cache")
        .execute(&tv.server.pool)
        .await
        .unwrap();
    grabbed
}

/// A hold whose final search finds nothing better grabs the held release
/// itself, so that grab is left out of what the search accepted.
async fn hold_unrelated_release(tv: &TvWatch, pattern_id: Uuid) {
    watch_repo::create_match(
        &tv.server.pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: Some(tv.source.id),
            release_guid: &format!("{HELD_GUID}-{pattern_id}"),
            release_title: "Kitsune.Days.S01E13.1080p.WEB.x264-TOR",
            release_data: Some(&json!({})),
            match_score: 0.9,
            matched_pattern: "Kitsune Days S01E13",
            action: "held",
            download_id: None,
            hold_until: Some(OffsetDateTime::now_utc() - time::Duration::minutes(1)),
            held: true,
            quality_score: Some(0.3),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .unwrap();
}

async fn route_titles(tv: &TvWatch, path: &str, request: Value, key: &str) -> BTreeSet<String> {
    let response = tv.server.admin_client().post_json(path, &request).await;
    response.assert_ok();
    let body: Value = response.json();
    body[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["title"]
                .as_str()
                .or(r["name"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// What the search routes and the stream offer for `want`, from whatever the
/// indexer currently returns.
pub async fn searched(
    tv: &TvWatch,
    want: &nats_jobs::MediaWant,
) -> Vec<(&'static str, BTreeSet<String>)> {
    let want_json = serde_json::to_value(want).unwrap();
    vec![
        (
            "search-and-download",
            route_titles(
                tv,
                "/api/v1/search-and-download",
                json!({"patterns": [], "media_want": want_json, "media_type": "tv", "min_score": 0.75}),
                "results",
            )
            .await,
        ),
        (
            "providers/releases",
            route_titles(
                tv,
                "/api/v1/providers/releases/search",
                json!({"media_want": want_json, "min_score": 0.75}),
                "candidates",
            )
            .await,
        ),
        ("stream", tv.stream_titles(want).await.into_iter().collect()),
    ]
}
