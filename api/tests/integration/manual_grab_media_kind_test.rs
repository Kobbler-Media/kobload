//! A release grabbed by hand for a want is parsed as the want's kind. The
//! pattern search modal posts no `media_kind`, so a MotoGP race was parsed as
//! a movie titled "MotoGP" and lost its location, round and session.

use serde_json::json;

use crate::common::sport::motogp_race_want;
use crate::common::tv_watch::TvWatch;

#[tokio::test]
async fn a_manual_grab_for_a_sport_pattern_is_parsed_as_sport() {
    let tv = TvWatch::start().await;
    let pattern = tv.pattern(&motogp_race_want()).await;

    let response = tv
        .server
        .admin_client()
        .post_json(
            "/api/v1/downloads",
            &json!({
                "source": "manual",
                "protocol": "usenet",
                "name": "MotoGP.2026.Round16.Japan.Race.1080p.WEB.h264-VERUM",
                "download_url": "http://example.com/download/motogp",
                "callback_url": "http://localhost:1111/callback",
                "metadata": {"origin": "manual-search", "pattern_id": pattern.id},
            }),
        )
        .await;
    response.assert_ok();
    let id: uuid::Uuid = response.json::<serde_json::Value>()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (media_kind,): (Option<String>,) =
        sqlx::query_as("SELECT media_kind FROM downloads WHERE id = $1")
            .bind(id)
            .fetch_one(&tv.server.pool)
            .await
            .unwrap();
    assert_eq!(media_kind.as_deref(), Some("sports"));
    tv.server.cleanup().await;
}
