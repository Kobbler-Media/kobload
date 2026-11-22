//! A download client answering a poll with a 5xx is having a bad moment, not reporting a
//! failed download: the poll error is recorded and the download keeps its status.

use axum::http::StatusCode;
use serde_json::json;
use usenet_proto::JobState;
use uuid::Uuid;

use crate::common::sab::SabFixture;
use crate::common::usenet_jobs::EngineFixture;

#[derive(Debug, sqlx::FromRow)]
struct Polled {
    status: String,
    error: Option<String>,
    last_poll_error: Option<String>,
}

async fn polled(pool: &sqlx::PgPool, id: Uuid) -> Polled {
    sqlx::query_as("SELECT status, error, last_poll_error FROM downloads WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn assert_transient(client: &str, row: &Polled) {
    assert_eq!(row.status, "downloading", "{client}: {row:?}");
    assert_eq!(row.error, None, "{client}: {row:?}");
    assert!(
        row.last_poll_error
            .as_deref()
            .is_some_and(|e| e.contains("503")),
        "{client}: the 5xx is recorded as the poll error: {row:?}"
    );
}

#[tokio::test]
async fn a_5xx_from_the_usenet_engine_on_poll_is_transient() {
    let fx = EngineFixture::start().await;
    let client = fx.add_usenet_client(true).await;
    let id = fx.in_flight(client, "g-503", "g-503-job").await;
    fx.seed_job("g-503-job", JobState::Downloading);
    fx.answer_polls_with(Some(StatusCode::SERVICE_UNAVAILABLE));

    fx.poll().await;
    assert_transient("usenet", &polled(&fx.server.pool, id).await);

    fx.answer_polls_with(None);
    fx.poll().await;
    let row = polled(&fx.server.pool, id).await;
    assert_eq!(row.status, "downloading");
    assert_eq!(row.last_poll_error, None, "a good poll clears it");
    fx.cleanup().await;
}

#[tokio::test]
async fn a_5xx_from_sabnzbd_on_poll_is_transient() {
    let fx = SabFixture::start(true).await;
    let id = fx.download("nzo", "downloading", json!({})).await;
    *fx.sab.answer.lock().unwrap() = Some(StatusCode::SERVICE_UNAVAILABLE);

    kobload_api::polling::poll_once(&fx.server.app_state).await;
    assert_transient("sabnzbd", &polled(&fx.server.pool, id).await);
    fx.cleanup().await;
}
