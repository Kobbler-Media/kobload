//! A search is only as complete as the indexers that answered it: one that failed must not
//! be cached as if it had nothing, nor hold a viewer past the deadline once another answered.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use uuid::Uuid;

use crate::common::tv_watch::episode_want;
use crate::common::usenet::UsenetRig;
use kobload_api::indexers::IndexerContext;
use kobload_api::repositories::watch as watch_repo;
use kobload_api::services::search::{IndexerSearch, SearchIndexersParams, search_indexers};
use kobload_api::watch::Release;

const SHORT_TIMEOUT: Duration = Duration::from_millis(150);

struct TwoIndexers {
    rig: UsenetRig,
    answering: Uuid,
    other: Uuid,
    title: String,
}

async fn two_indexers(show: &str) -> TwoIndexers {
    let rig = UsenetRig::start().await;
    let other = rig.tv.source.id;
    let answering = rig.add_indexer("Answering Newznab", -1).await;
    let title = format!("{}.S01E05.1080p.WEB-DL.x264-NZB", show.replace(' ', "."));
    rig.tv.indexer.set_source_results(
        answering,
        vec![rig.nzb_from(answering, "g-answering", &title)],
    );
    rig.tv.indexer.set_source_results(other, Vec::new());
    TwoIndexers {
        rig,
        answering,
        other,
        title,
    }
}

async fn searched(rig: &UsenetRig, media_type_id: Option<Uuid>) -> Result<IndexerSearch, String> {
    let ctx = IndexerContext {
        pool: &rig.tv.server.pool,
        http_client: &rig.tv.server.app_state.http_client,
    };
    search_indexers(&SearchIndexersParams {
        ctx: &ctx,
        indexers: rig.tv.server.app_state.indexers.as_ref(),
        pool: &rig.tv.server.pool,
        query: "anything",
        limit: 10,
        external_ids: &[],
        media_want: None,
        media_type_id,
        per_source_timeout: Some(SHORT_TIMEOUT),
        season: None,
        episode: None,
        source_filter: None,
        on_source_answered: None,
    })
    .await
}

fn titles(releases: &[Release]) -> Vec<&str> {
    releases.iter().map(|r| r.title.as_str()).collect()
}

#[tokio::test]
async fn test_the_search_reports_whether_every_source_answered() {
    enum Other {
        NoResults,
        Status(StatusCode),
        Hangs,
    }
    for (name, other_does, complete) in [
        ("zero results is an answer", Other::NoResults, true),
        ("429", Other::Status(StatusCode::TOO_MANY_REQUESTS), false),
        (
            "500",
            Other::Status(StatusCode::INTERNAL_SERVER_ERROR),
            false,
        ),
        ("503", Other::Status(StatusCode::SERVICE_UNAVAILABLE), false),
        ("timeout", Other::Hangs, false),
    ] {
        let setup = two_indexers(&format!("Complete {name}")).await;
        match other_does {
            Other::NoResults => {}
            Other::Status(status) => setup.rig.tv.indexer.set_source_error(setup.other, status),
            Other::Hangs => setup
                .rig
                .tv
                .indexer
                .set_source_delay(setup.other, SHORT_TIMEOUT * 4),
        }

        let found = searched(&setup.rig, None).await.unwrap();

        assert_eq!(found.complete, complete, "{name}");
        assert_eq!(titles(&found.releases), [setup.title.as_str()], "{name}");
        setup.rig.tv.server.cleanup().await;
    }
}

#[tokio::test]
async fn test_a_source_in_backoff_leaves_the_search_incomplete() {
    let setup = two_indexers("Backoff Show").await;
    watch_repo::record_rate_limited(&setup.rig.tv.server.pool, setup.other, Some(600))
        .await
        .unwrap();

    let found = searched(&setup.rig, None).await.unwrap();

    assert!(!found.complete);
    assert_eq!(titles(&found.releases), [setup.title.as_str()]);
    setup.rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_failed_indexer_is_asked_again_and_a_clean_empty_one_is_not() {
    for (name, failure, cached) in [
        ("clean", None, true),
        ("limited", Some(StatusCode::TOO_MANY_REQUESTS), false),
        ("down", Some(StatusCode::BAD_GATEWAY), false),
    ] {
        let show = format!("Cache {name} Show");
        let setup = two_indexers(&show).await;
        if let Some(status) = failure {
            setup.rig.tv.indexer.set_source_error(setup.other, status);
        }
        let want = episode_want(&show, "1", "5", None);

        let first = setup.rig.tv.stream_options(&want).await;
        assert_eq!(first.len(), 1, "{name}: {first:?}");
        let after_first = setup.rig.tv.indexer.searches();
        let second = setup.rig.tv.stream_options(&want).await;

        assert_eq!(second.len(), 1, "{name}: {second:?}");
        assert_eq!(
            setup.rig.tv.indexer.searches() == after_first,
            cached,
            "{name}"
        );
        setup.rig.tv.server.cleanup().await;
    }
}

#[tokio::test]
async fn test_a_category_lookup_error_keeps_the_other_sources_answers() {
    let setup = two_indexers("Category Show").await;
    let pool = &setup.rig.tv.server.pool;
    sqlx::query("UPDATE indexer_sources SET category_ids = '{5000}' WHERE id = $1")
        .bind(setup.answering)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE indexer_category_mappings RENAME TO unreadable_mappings")
        .execute(pool)
        .await
        .unwrap();

    let found = searched(&setup.rig, Some(Uuid::now_v7())).await.unwrap();

    assert!(!found.complete);
    assert_eq!(titles(&found.releases), [setup.title.as_str()]);

    sqlx::query("UPDATE indexer_sources SET category_ids = NULL WHERE id = $1")
        .bind(setup.answering)
        .execute(pool)
        .await
        .unwrap();
    let every_source_failed = searched(&setup.rig, Some(Uuid::now_v7())).await;
    assert!(every_source_failed.is_err());
    sqlx::query("ALTER TABLE unreadable_mappings RENAME TO indexer_category_mappings")
        .execute(pool)
        .await
        .unwrap();
    setup.rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_usenet_only_title_gets_partial_results_at_the_deadline() {
    let show = "Usenet Only Partial";
    let setup = two_indexers(show).await;
    setup
        .rig
        .tv
        .indexer
        .set_source_delay(setup.other, Duration::from_secs(9));
    setup
        .rig
        .tv
        .indexer
        .set_source_delay(setup.answering, Duration::from_millis(50));

    let started = Instant::now();
    let options = setup.rig.options(show, "5").await;
    let took = started.elapsed();

    assert!(took < Duration::from_secs(6), "waited {took:?}");
    assert_eq!(options.len(), 1, "{options:?}");
    assert_eq!(options[0].release_title, setup.title);
    setup.rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_usenet_only_title_waits_past_the_deadline_until_one_indexer_answers() {
    let show = "Usenet Only Waiting";
    let setup = two_indexers(show).await;
    let indexer = &setup.rig.tv.indexer;
    indexer.set_source_delay(setup.answering, Duration::from_secs(5));
    indexer.set_source_delay(setup.other, Duration::from_secs(12));

    let started = Instant::now();
    let options = setup.rig.options(show, "5").await;
    let took = started.elapsed();

    assert!(took >= Duration::from_secs(5), "cut at {took:?}");
    assert!(took < Duration::from_secs(8), "waited {took:?}");
    assert_eq!(options.len(), 1, "{options:?}");
    setup.rig.tv.server.cleanup().await;
}
