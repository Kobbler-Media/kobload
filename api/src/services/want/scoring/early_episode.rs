use nats_jobs::EpisodeRef;
use time::{Date, Duration};

use super::{AIR_DATE_LENIENCY_DAYS, parse_iso_date};

pub(crate) const EARLY_RELEASE_DAYS: i64 = 3;
pub(crate) const EARLY_SEARCH_INTERVAL_HOURS: i32 = 12;

fn is_past_leniency(date: Date, today: Date) -> bool {
    date - Duration::days(AIR_DATE_LENIENCY_DAYS) <= today
}

// Providers list binge-dropped seasons on a weekly schedule they never follow,
// so a known premiere overrides the episode's own date. Fakes that exploit this
// are caught by the engine's pre-download content check.
pub(super) fn may_be_out(own: Option<Date>, premiere: Option<Date>, today: Date) -> bool {
    match premiere {
        Some(premiere) => is_past_leniency(premiere, today),
        None => own.is_none_or(|d| is_past_leniency(d, today)),
    }
}

pub(crate) fn episode_held_back(
    air_date: Option<&str>,
    season_premiere: Option<&str>,
    today: Date,
) -> bool {
    !may_be_out(
        air_date.and_then(parse_iso_date),
        season_premiere.and_then(parse_iso_date),
        today,
    )
}

pub(super) fn known_premiere(season_premiere: Option<&str>) -> Option<&str> {
    season_premiere.filter(|p| parse_iso_date(p).is_some())
}

pub(crate) fn early_search_horizon(today: Date) -> Date {
    today + Duration::days(EARLY_RELEASE_DAYS)
}

// Acceptance is wider than this on purpose: a far-future episode is taken when
// RSS or a season query turns it up, but never costs a search of its own.
pub(crate) fn episodes_to_search<'a>(
    episodes: impl IntoIterator<Item = &'a EpisodeRef>,
    today: Date,
) -> Vec<&'a EpisodeRef> {
    let mut sorted: Vec<&EpisodeRef> = episodes.into_iter().collect();
    sorted.sort_by_key(|ep| {
        ep.sort_order
            .unwrap_or(ep.episode.parse::<i32>().unwrap_or(999))
    });
    let horizon = early_search_horizon(today);
    let mut passed_future = false;
    sorted
        .into_iter()
        .take_while(|ep| match ep.air_date.as_deref().and_then(parse_iso_date) {
            Some(date) => {
                passed_future |= date > today;
                date <= horizon
            }
            None => !passed_future,
        })
        .collect()
}
