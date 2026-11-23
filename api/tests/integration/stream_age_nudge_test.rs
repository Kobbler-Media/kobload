//! Newer posts are likelier to be intact, so the shared freshness bonus breaks near ties in
//! stream ranking without ever overriding quality. Newznab dates are RFC 2822.

use time::format_description::well_known::{Rfc2822, Rfc3339};
use time::{Duration, OffsetDateTime};

use crate::common::TestServer;
use crate::common::stream_fixtures::{create_provider, option, usenet_option};
use crate::common::tv_watch::{torrent_release, usenet_release};
use kobload_api::services::stream::{
    KnownAvailability, RankCandidate, RankPreferences, rank_options,
};
use kobload_api::services::want::freshness_tiebreaker;
use nats_jobs::types::StreamOption;

fn days_ago(days: i64) -> Option<String> {
    (OffsetDateTime::now_utc() - Duration::days(days))
        .format(&Rfc2822)
        .ok()
}

fn rfc3339_days_ago(days: i64) -> Option<String> {
    (OffsetDateTime::now_utc() - Duration::days(days))
        .format(&Rfc3339)
        .ok()
}

#[derive(Clone)]
struct Side {
    title: &'static str,
    score: f64,
    date: Option<String>,
    torrent: bool,
}

fn nzb(title: &'static str, score: f64, date: Option<String>) -> Side {
    Side {
        title,
        score,
        date,
        torrent: false,
    }
}

fn torrent(title: &'static str, score: f64, date: Option<String>) -> Side {
    Side {
        title,
        score,
        date,
        torrent: true,
    }
}

const OLD: &str = "Movie.2024.1080p.WEB-DL.x264-OLD";
const NEW: &str = "Movie.2024.1080p.WEB-DL.x264-NEW";
const UHD: &str = "Movie.2024.2160p.WEB-DL.x265-UHD";

fn scored(side: &Side) -> f64 {
    let mut release = if side.torrent {
        torrent_release("g", side.title, 'a')
    } else {
        usenet_release("g", side.title)
    };
    release.publish_date = side.date.clone().unwrap_or_default();
    side.score + freshness_tiebreaker(&release)
}

#[test]
fn freshness_reads_what_newznab_sends_and_stays_a_nudge() {
    let bonus = |protocol_torrent: bool, date: &str| {
        let mut release = if protocol_torrent {
            torrent_release("g", OLD, 'a')
        } else {
            usenet_release("g", OLD)
        };
        release.publish_date = date.to_string();
        freshness_tiebreaker(&release)
    };
    let real = "Sat, 21 Nov 2026 00:40:21 +0100";
    assert!(
        bonus(false, real) > 0.0,
        "a real newznab date earns the bonus"
    );
    assert!(
        (bonus(false, real) - bonus(false, "2026-11-20T23:40:21Z")).abs() < 1e-9,
        "both formats date the same instant alike"
    );
    for days in [-30, 0, 3, 1000, 6000] {
        let date = days_ago(days).unwrap();
        let usenet = bonus(false, &date);
        assert!((0.0..=0.04).contains(&usenet), "{days}: {usenet}");
        assert!(
            (bonus(true, &date) - usenet / 2.0).abs() < 1e-9,
            "{days}: torrents earn half"
        );
    }
    let at = |days| bonus(false, &days_ago(days).unwrap());
    assert!(at(3) - at(1000) > 0.02, "days vs years clearly differs");
    assert!(at(1000) - at(1100) < 0.001, "1000 vs 1100 days barely does");
    assert_eq!(bonus(false, "last tuesday"), 0.0);
    assert_eq!(bonus(false, ""), 0.0);
}

#[tokio::test]
async fn age_breaks_near_ties_and_never_beats_a_clearly_better_release() {
    let server = TestServer::start().await;
    let p = create_provider(&server.pool, "usenet").await;
    let cases: Vec<(&str, Side, Side, &str)> = vec![
        (
            "identical score: 3 days beats 1000",
            nzb(OLD, 1.5, days_ago(1000)),
            nzb(NEW, 1.5, days_ago(3)),
            NEW,
        ),
        (
            "near tie: 3 days beats a hair more score at 1000",
            nzb(OLD, 1.5, days_ago(1000)),
            nzb(NEW, 1.49, days_ago(3)),
            NEW,
        ),
        (
            "zero score: newer still sorts first",
            nzb(OLD, 0.0, days_ago(1000)),
            nzb(NEW, 0.0, days_ago(3)),
            NEW,
        ),
        (
            "negative score: newer still sorts first",
            nzb(OLD, -1.0, days_ago(1000)),
            nzb(NEW, -1.0, days_ago(3)),
            NEW,
        ),
        (
            "a very high score does not widen the nudge past a real gap",
            nzb(OLD, 10.05, days_ago(4000)),
            nzb(NEW, 10.0, days_ago(0)),
            OLD,
        ),
        (
            "1000 vs 1100 days barely matters",
            nzb(OLD, 1.501, days_ago(1100)),
            nzb(NEW, 1.5, days_ago(1000)),
            OLD,
        ),
        (
            "a resolution tier (0.15) beats any age",
            nzb(UHD, 1.65, days_ago(4000)),
            nzb(NEW, 1.5, days_ago(0)),
            UHD,
        ),
        (
            "the smallest profile step (preferred audio, 0.05) beats any age",
            nzb(OLD, 1.55, days_ago(4000)),
            nzb(NEW, 1.5, days_ago(0)),
            OLD,
        ),
        (
            "a missing date gets no nudge",
            nzb(OLD, 1.5001, days_ago(6000)),
            nzb(NEW, 1.5, None),
            OLD,
        ),
        (
            "a missing date gets no penalty",
            nzb(OLD, 1.5, days_ago(6000)),
            nzb(NEW, 1.5001, None),
            NEW,
        ),
        (
            "an unparseable date gets no nudge",
            nzb(OLD, 1.5001, days_ago(6000)),
            nzb(NEW, 1.5, Some("last tuesday".to_string())),
            OLD,
        ),
        (
            "a future date counts as new, not as a bigger nudge",
            nzb(OLD, 1.5001, days_ago(0)),
            nzb(NEW, 1.5, days_ago(-30)),
            OLD,
        ),
        (
            "torrents earn half: a gap usenet's nudge would close stays open",
            torrent(OLD, 1.52, days_ago(1000)),
            torrent(NEW, 1.5, days_ago(3)),
            OLD,
        ),
        (
            "the same gap on usenet closes",
            nzb(OLD, 1.52, days_ago(1000)),
            nzb(NEW, 1.5, days_ago(3)),
            NEW,
        ),
        (
            "an RFC 3339 date counts the same as RFC 2822",
            nzb(OLD, 1.5, days_ago(1000)),
            nzb(NEW, 1.49, rfc3339_days_ago(3)),
            NEW,
        ),
    ];

    for (label, a, b, first) in cases {
        for (x, y) in [(&a, &b), (&b, &a)] {
            let build = |s: &Side, n: usize| {
                let mut o: StreamOption = if s.torrent {
                    option(&p, &format!("{n:040x}"), s.title, scored(s))
                } else {
                    usenet_option(&p, &format!("g{n}"), s.title, scored(s), None)
                };
                o.publish_date = s.date.clone();
                RankCandidate {
                    option: o,
                    availability: KnownAvailability::LikelyAvailable,
                }
            };
            let out = rank_options(
                &server.pool,
                &p,
                vec![build(x, 1), build(y, 2)],
                &RankPreferences::default(),
            )
            .await;
            assert_eq!(out[0].release_title, first, "{label}");
            for o in &out {
                let side = if o.release_title == a.title { &a } else { &b };
                assert_eq!(
                    o.total_score,
                    scored(side),
                    "{label}: ranking adds no second age bonus"
                );
            }
        }
    }
    server.cleanup().await;
}
