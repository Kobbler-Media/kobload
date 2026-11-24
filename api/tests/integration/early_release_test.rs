//! Providers list binge-dropped and streaming seasons on dates they never
//! follow, so episode and season wants accept a release of any episode once the
//! season has started. Active per-episode searching stays near the listed date
//! to protect indexer budgets.

use std::collections::{BTreeSet, HashMap};

use nats_jobs::{EpisodeRef, MediaWant, StructuralRejectReason};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::common::entry_points::{Grabber, grab_alone, post_want, searched, torrent};
use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};
use kobload_api::jobs::handlers::{
    BacklogSearchHandler, SearchGates, backlog_search_pattern, get_patterns_due_for_backlog,
};
use kobload_api::jobs::worker::JobHandler;
use kobload_api::services::want::koblem_review::{review_wanted_title, worth_reviewing};
use kobload_api::services::want::{ScoringConfig, SearchContext, score_release, structural_reject};
use kobload_api::watch::Release;

const GB: i64 = 1_000_000_000;
const UNAIRED: Option<StructuralRejectReason> = Some(StructuralRejectReason::UnairedEpisode);
const TOO_OLD: Option<StructuralRejectReason> = Some(StructuralRejectReason::PublishDateTooOld);
const E04: &str = "Kitsune.Days.S02E04.1080p.WEB-DL.x264-GRP";

#[derive(Clone, Copy, Debug)]
enum Kind {
    Episode,
    Season,
}

const KINDS: [Kind; 2] = [Kind::Episode, Kind::Season];

fn day(offset: i64) -> String {
    (OffsetDateTime::now_utc().date() + Duration::days(offset)).to_string()
}

fn episode(air_in: Option<i64>, premiere_in: Option<i64>) -> MediaWant {
    let mut want = episode_want("Kitsune Days", "2", "4", None);
    if let MediaWant::TvEpisode {
        air_date,
        season_premiere,
        ..
    } = &mut want
    {
        *air_date = air_in.map(day);
        *season_premiere = premiere_in.map(day);
    }
    want
}

fn season(episodes: &[(u32, Option<i64>)]) -> MediaWant {
    MediaWant::TvSeason {
        show_name: "Kitsune Days".to_string(),
        season: "2".to_string(),
        year: None,
        episodes_needed: Some(
            episodes
                .iter()
                .map(|(number, air_in)| EpisodeRef {
                    season: "2".to_string(),
                    episode: number.to_string(),
                    sort_order: Some(*number as i32),
                    air_date: air_in.map(day),
                    in_library: Some(false),
                    library_video_height: None,
                    title: None,
                })
                .collect(),
        ),
        total_episodes: Some(episodes.len() as u32),
        pack_preference: "auto".to_string(),
        last_air_date: episodes.iter().filter_map(|(_, d)| *d).max().map(day),
        identifiers: None,
        alternate_titles: None,
        quality_profile_id: None,
        quality_overrides: None,
    }
}

fn want(kind: Kind, air_in: i64, premiere_in: i64) -> MediaWant {
    match kind {
        Kind::Episode => episode(Some(air_in), Some(premiere_in)),
        Kind::Season => season(&[(1, Some(premiere_in)), (4, Some(air_in))]),
    }
}

fn offered(title: &str, published: i64) -> Release {
    let mut release = usenet_release(title, title);
    release.size = 2 * GB;
    release.publish_date = day(published);
    release
}

struct Listing {
    name: &'static str,
    air_in: i64,
    premiere_in: i64,
    published: i64,
    rejected: Option<StructuralRejectReason>,
}

const LISTINGS: &[Listing] = &[
    Listing {
        name: "friday release of a monday listing",
        air_in: 3,
        premiere_in: -21,
        published: 0,
        rejected: None,
    },
    Listing {
        name: "binge drop, listed weeks ahead",
        air_in: 28,
        premiere_in: -1,
        published: 0,
        rejected: None,
    },
    Listing {
        name: "premiere tomorrow counts as started",
        air_in: 14,
        premiere_in: 1,
        published: 0,
        rejected: None,
    },
    Listing {
        name: "season not started",
        air_in: 14,
        premiere_in: 2,
        published: 0,
        rejected: UNAIRED,
    },
    Listing {
        name: "already aired",
        air_in: -1,
        premiere_in: -21,
        published: 0,
        rejected: None,
    },
    Listing {
        name: "posted before its listing, found after it",
        air_in: -1,
        premiere_in: -21,
        published: -6,
        rejected: None,
    },
    Listing {
        name: "posted before the premiere",
        air_in: 3,
        premiere_in: -21,
        published: -30,
        rejected: TOO_OLD,
    },
];

#[test]
fn both_want_kinds_share_one_verdict() {
    let mut failures = Vec::new();
    for listing in LISTINGS {
        for kind in KINDS {
            let want = want(kind, listing.air_in, listing.premiere_in);
            for context in [SearchContext::Structured, SearchContext::Rss] {
                let verdict =
                    structural_reject(&offered(E04, listing.published), Some(&want), context);
                if verdict != listing.rejected {
                    failures.push(format!(
                        "{} {kind:?} {context:?}: {verdict:?}",
                        listing.name
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn no_episode_only_guards_before_the_listed_date() {
    let cases = [
        ("cam", "Kitsune.Days.S02E04.1080p.HDCAM.x264-GRP", 2 * GB),
        ("tiny", E04, 20_000_000),
        ("unknown size", E04, 0),
        (
            "other episode title",
            "Kitsune.Days.S02E04.Young.Blood.1080p.WEB-DL.x264-GRP",
            2 * GB,
        ),
    ];
    for (name, title, size) in cases {
        for kind in KINDS {
            let mut release = offered(title, 0);
            release.size = size;
            let verdict = structural_reject(
                &release,
                Some(&want(kind, 3, -21)),
                SearchContext::Structured,
            );
            assert_eq!(verdict, None, "{name} {kind:?}");
        }
    }
}

#[test]
fn unknown_premiere_keeps_each_kinds_existing_gate() {
    let cases = [
        ("episode, listed in 3 days", episode(Some(3), None), UNAIRED),
        ("episode, listed tomorrow", episode(Some(1), None), None),
        ("episode, no dates", episode(None, None), None),
        ("season, no dates", season(&[(3, None), (4, None)]), None),
        (
            "season, undated behind an aired episode",
            season(&[(3, Some(-7)), (4, None)]),
            None,
        ),
        (
            "season, undated behind a future episode",
            season(&[(1, Some(-14)), (3, Some(7)), (4, None)]),
            UNAIRED,
        ),
    ];
    for (name, want, expected) in cases {
        let verdict = structural_reject(&offered(E04, 0), Some(&want), SearchContext::Rss);
        assert_eq!(verdict, expected, "{name}");
    }
}

#[test]
fn a_season_that_has_not_started_never_reaches_koblem() {
    let patterns = vec!["Kitsune Days S02E04".to_string()];
    let reviewed = |want: &MediaWant| {
        let config = ScoringConfig::new(&[], &patterns, 0.5, None, None).with_media_want(want);
        let scored = score_release(&offered(E04, 0), &config);
        worth_reviewing(&scored, &review_wanted_title(Some(want)))
    };
    for kind in KINDS {
        assert!(!reviewed(&want(kind, 14, 2)), "not started {kind:?}");
        assert!(reviewed(&want(kind, 3, -21)), "started {kind:?}");
    }
    assert!(!reviewed(&episode(Some(3), None)), "unknown premiere");
}

async fn grab_failures(kind: Kind, grabber: Grabber) -> Vec<String> {
    let tv = TvWatch::start().await;
    let mut failures = Vec::new();
    for (i, (name, air_in, premiere_in, expected)) in [
        ("friday for monday", 3, -21, true),
        ("binge drop", 28, -1, true),
        ("not started", 14, 2, false),
    ]
    .into_iter()
    .enumerate()
    {
        let title = format!("Kitsune.Days.S02E04.1080p.WEB-DL.x264-G{i}");
        let wanted = want(kind, air_in, premiere_in);
        let grabbed = grab_alone(&tv, grabber, &wanted, &offered(&title, 0)).await;
        if grabbed.is_empty() == expected {
            failures.push(format!("{} {kind:?} {name}: {grabbed:?}", grabber.name()));
        }
    }
    tv.server.cleanup().await;
    failures
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_grabber_applies_the_rule_for_both_kinds() {
    let runs = KINDS
        .into_iter()
        .flat_map(|kind| Grabber::ALL.map(move |grabber| grab_failures(kind, grabber)));
    let failures: Vec<String> = futures::future::join_all(runs)
        .await
        .into_iter()
        .flatten()
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test]
async fn search_routes_and_stream_follow_the_rule() {
    for (name, wanted, offered_titles) in [
        ("started", episode(Some(3), Some(-21)), 1),
        ("binge drop", episode(Some(28), Some(-1)), 1),
        ("not started", episode(Some(14), Some(2)), 0),
    ] {
        let tv = TvWatch::start().await;
        let mut release = torrent(E04, 'a');
        release.size = 2 * GB;
        release.publish_date = day(0);
        tv.indexer.set_search_results(vec![release]);
        for (entry, titles) in searched(&tv, &wanted).await {
            assert_eq!(titles.len(), offered_titles, "{name}: {entry} {titles:?}");
        }
        tv.server.cleanup().await;
    }
}

fn title_of(episode: u32) -> String {
    format!("Kitsune.Days.S02E{episode:02}.1080p.WEB-DL.x264-GRP")
}

async fn searched_season_grabs(grabber: Grabber, season_query: &[u32]) -> BTreeSet<String> {
    let tv = TvWatch::start().await;
    let wanted = season(&[
        (1, Some(-7)),
        (2, Some(2)),
        (3, Some(3)),
        (4, Some(10)),
        (5, Some(17)),
    ]);
    tv.indexer
        .set_per_episode_results((1..=5).map(|e| offered(&title_of(e), 0)).collect());
    tv.indexer.set_search_results(
        season_query
            .iter()
            .map(|e| offered(&title_of(*e), 0))
            .collect(),
    );
    let pattern_id = match grabber {
        Grabber::Backlog => {
            let pattern = tv.pattern(&wanted).await;
            backlog_search_pattern(&tv.server.app_state, &pattern, SearchGates::AUTOMATIC).await;
            pattern.id
        }
        _ => post_want(&tv, &wanted).await,
    };
    let grabbed = tv.grabbed(pattern_id).await.into_iter().collect();
    tv.server.cleanup().await;
    grabbed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_episode_search_stops_at_the_first_episode_outside_the_window() {
    let expected: BTreeSet<String> = [1, 2, 3].map(title_of).into();
    for grabber in [Grabber::Backlog, Grabber::WantRoute] {
        let grabbed = searched_season_grabs(grabber, &[1]).await;
        assert_eq!(grabbed, expected, "{}", grabber.name());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_far_future_episode_in_a_season_query_is_still_grabbed() {
    let expected: BTreeSet<String> = [1, 2, 3, 5].map(title_of).into();
    for grabber in [Grabber::Backlog, Grabber::WantRoute] {
        let grabbed = searched_season_grabs(grabber, &[1, 5]).await;
        assert_eq!(grabbed, expected, "{}", grabber.name());
    }
}

struct DueCase {
    name: &'static str,
    want: MediaWant,
    not_before_days: Option<i32>,
    searched_hours_ago: i32,
    interval_hours: Option<i32>,
    due: bool,
}

fn movie() -> MediaWant {
    serde_json::from_value(serde_json::json!({
        "kind": "movie", "title": "Kitsune", "year": 2026,
        "identifiers": null, "quality_profile_id": null, "quality_overrides": null,
    }))
    .unwrap()
}

fn due_cases() -> Vec<DueCase> {
    let case = |name, want, searched_hours_ago, due| DueCase {
        name,
        want,
        not_before_days: None,
        searched_hours_ago,
        interval_hours: None,
        due,
    };
    let next_in_window = || season(&[(1, Some(-7)), (2, Some(3))]);
    let next_far_out = || season(&[(1, Some(-7)), (2, Some(10))]);
    vec![
        case(
            "episode in window, 13h",
            episode(Some(3), Some(-21)),
            13,
            true,
        ),
        case(
            "episode in window, 11h",
            episode(Some(3), Some(-21)),
            11,
            false,
        ),
        DueCase {
            interval_hours: Some(1),
            ..case(
                "fast interval is slowed",
                episode(Some(3), Some(-21)),
                11,
                false,
            )
        },
        DueCase {
            interval_hours: Some(48),
            ..case(
                "slower interval is kept",
                episode(Some(3), Some(-21)),
                13,
                false,
            )
        },
        case(
            "episode past window",
            episode(Some(4), Some(-21)),
            100,
            false,
        ),
        case("season next in window, 13h", next_in_window(), 13, true),
        case("season next in window, 11h", next_in_window(), 11, false),
        DueCase {
            interval_hours: Some(24),
            ..case(
                "season at the column default, 13h",
                next_in_window(),
                13,
                true,
            )
        },
        DueCase {
            interval_hours: Some(48),
            ..case("season with a slower interval", next_in_window(), 13, false)
        },
        case("season next far out, 13h", next_far_out(), 13, false),
        case("season next far out, 25h", next_far_out(), 25, true),
        case("aired, 13h", episode(Some(-1), None), 13, false),
        case("aired, 25h", episode(Some(-1), None), 25, true),
        case(
            "air day is normal cadence",
            episode(Some(0), None),
            13,
            false,
        ),
        case("no air date, 25h", episode(None, None), 25, true),
        DueCase {
            not_before_days: Some(2),
            ..case("unstarted episode", episode(Some(2), Some(2)), 100, false)
        },
        DueCase {
            not_before_days: Some(2),
            ..case(
                "unstarted season",
                season(&[(1, Some(2)), (2, Some(9))]),
                100,
                false,
            )
        },
        DueCase {
            not_before_days: Some(2),
            ..case("movie keeps its release date", movie(), 100, false)
        },
    ]
}

#[tokio::test]
async fn backlog_due_query_applies_the_window_and_its_cadence() {
    let tv = TvWatch::start().await;
    let pool = &tv.server.pool;
    let mut expected = BTreeSet::new();
    let mut names = HashMap::<Uuid, &str>::new();
    for case in due_cases() {
        let pattern = tv.pattern(&case.want).await;
        sqlx::query(
            "UPDATE watch_patterns SET
                search_not_before = NOW() + ($2::int * INTERVAL '1 day'),
                last_backlog_search_at = NOW() - ($3::int * INTERVAL '1 hour'),
                backlog_search_interval_hours = $4
             WHERE id = $1",
        )
        .bind(pattern.id)
        .bind(case.not_before_days)
        .bind(case.searched_hours_ago)
        .bind(case.interval_hours)
        .execute(pool)
        .await
        .unwrap();
        names.insert(pattern.id, case.name);
        if case.due {
            expected.insert(case.name);
        }
    }

    let due: BTreeSet<&str> = get_patterns_due_for_backlog(pool, 100)
        .await
        .unwrap()
        .iter()
        .map(|p| names[&p.id])
        .collect();

    assert_eq!(due, expected);
    tv.server.cleanup().await;
}

async fn run_backlog(tv: &TvWatch) -> BTreeSet<String> {
    tv.indexer.take_searched_episodes();
    BacklogSearchHandler::new(tv.server.app_state.clone())
        .execute(
            Uuid::now_v7(),
            serde_json::json!({}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    tv.indexer.take_searched_episodes()
}

async fn age_searches(tv: &TvWatch, id: Uuid, full_hours: i32, near_air_hours: Option<i32>) {
    sqlx::query(
        "UPDATE watch_patterns SET
            last_backlog_search_at = NOW() - ($2::int * INTERVAL '1 hour'),
            last_near_air_search_at = NOW() - ($3::int * INTERVAL '1 hour')
         WHERE id = $1",
    )
    .bind(id)
    .bind(full_hours)
    .bind(near_air_hours)
    .execute(&tv.server.pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_near_air_episode_runs_at_the_early_cadence() {
    let names = |eps: &[&str]| -> BTreeSet<String> { eps.iter().map(|e| e.to_string()).collect() };
    for interval in [None, Some(24)] {
        let tv = TvWatch::start().await;
        let wanted = season(&[(1, Some(-30)), (2, Some(-23)), (3, Some(-16)), (4, Some(2))]);
        tv.indexer.set_search_results(vec![offered(
            "Kitsune.Days.S02E09.1080p.WEB-DL.x264-GRP",
            0,
        )]);
        let pattern = tv.pattern(&wanted).await;
        sqlx::query("UPDATE watch_patterns SET backlog_search_interval_hours = $2 WHERE id = $1")
            .bind(pattern.id)
            .bind(interval)
            .execute(&tv.server.pool)
            .await
            .unwrap();

        age_searches(&tv, pattern.id, 13, None).await;
        assert_eq!(
            run_backlog(&tv).await,
            names(&["S02E04"]),
            "{interval:?}: 12h run"
        );
        let due = get_patterns_due_for_backlog(&tv.server.pool, 10)
            .await
            .unwrap();
        assert!(
            due.is_empty(),
            "{interval:?}: the 12h run restarts its clock"
        );

        age_searches(&tv, pattern.id, 25, Some(1)).await;
        assert_eq!(
            run_backlog(&tv).await,
            names(&["S02E01", "S02E02", "S02E03", "S02E04"]),
            "{interval:?}: the normal interval still covers old gaps"
        );
        tv.server.cleanup().await;
    }
}

fn specials(mut want: MediaWant) -> MediaWant {
    match &mut want {
        MediaWant::TvEpisode { season, .. } => *season = "0".to_string(),
        MediaWant::TvSeason {
            season,
            episodes_needed,
            ..
        } => {
            *season = "00".to_string();
            for ep in episodes_needed.iter_mut().flatten() {
                ep.season = "0".to_string();
            }
        }
        _ => {}
    }
    want
}

#[test]
fn specials_have_no_premiere_and_keep_the_strict_own_date_rule() {
    const SPECIAL: &str = "Kitsune.Days.S00E04.1080p.WEB-DL.x264-GRP";
    let cases = [
        ("unaired, old premiere", 14, -400, 0, UNAIRED),
        ("posted long before its date", -1, -400, -30, TOO_OLD),
        ("aired and freshly posted", -1, -400, 0, None),
        ("inside the leniency window", 1, -400, 0, None),
    ];
    let mut failures = Vec::new();
    for (name, air_in, premiere_in, published, expected) in cases {
        for kind in KINDS {
            let wanted = specials(want(kind, air_in, premiere_in));
            for context in [SearchContext::Structured, SearchContext::Rss] {
                let verdict =
                    structural_reject(&offered(SPECIAL, published), Some(&wanted), context);
                if verdict != expected {
                    failures.push(format!("{name} {kind:?} {context:?}: {verdict:?}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
