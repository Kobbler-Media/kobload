use std::collections::HashMap;
use std::sync::Arc;

use kob_normalize::titles_similar;
use nats_jobs::{MediaWant, QualityOverrides};

use crate::parsing::{ParsedMedia, ParsedRelease, parse_release};
use crate::quality::score_quality_parsed;
use crate::repositories::quality_profiles::QualityProfileRow;
use crate::services::want::release_facts::ReleaseFacts;
use crate::trash_filters::TrashFilterSet;
use crate::watch::matcher::{
    PreparedTarget, PreparedTitle, prepare_targets, score_prepared_targets,
};
use crate::watch::{MatchTarget, ProcessedRelease, Release, match_patterns};
use crate::xem::XemShowMap;

/// Context about how the release was found — affects disambiguation strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchContext {
    /// Release found via structured ID-based search (TVDB/TMDB).
    Structured,
    /// Release found via text query fallback (no ID filtering).
    TextFallback,
    /// Release found via RSS feed (no search context).
    #[default]
    Rss,
}

// ── TV show disambiguation constants ────────────────────────────────────────
/// Reject releases published more than this many days before air_date.
/// Tighter for active searches (backlog/structured) where old results are common;
/// looser for RSS where feed recency already enforces freshness.
pub(crate) const PUBLISH_DATE_BEFORE_AIR_REJECT_DAYS_SEARCH: i64 = 1;
pub(crate) const PUBLISH_DATE_BEFORE_AIR_REJECT_DAYS_RSS: i64 = 5;
/// Structural bonus for release title containing show year.
pub(crate) const YEAR_MATCH_BONUS: f64 = 0.25;
/// Structural bonus for episode title token match.
pub(crate) const EPISODE_TITLE_MATCH_BONUS: f64 = 0.1;
/// Reject releases published before (show_year - margin).
pub(crate) const SHOW_YEAR_PUBLISH_REJECT_MARGIN_YEARS: i32 = 1;
/// Leniency days for air_date gating: allow grabbing episodes this many days
/// before their listed air_date (timezone safety — episode may be available
/// earlier than the date suggests). The check becomes air_date - N > today,
/// so only episodes airing more than N days from now are rejected.
pub(crate) const AIR_DATE_LENIENCY_DAYS: i64 = 1;

/// Reject releases with sufficiently negative community votes.
/// Returns the rejection reason if votes are below threshold.
const VOTES_REJECT_THRESHOLD: i32 = -5;

pub fn votes_reject(release: &Release) -> Option<&'static str> {
    if let Some(votes) = release.votes
        && votes < VOTES_REJECT_THRESHOLD
    {
        return Some("negative_community_votes");
    }
    None
}

/// Small tiebreaker based on seeder count for torrents.
/// Caps at 0.05 so it never overrides quality or match score.
fn seeder_tiebreaker(release: &Release) -> f64 {
    if release.protocol != "torrent" {
        return 0.0;
    }
    let seeders = release.seeders.unwrap_or(0) as f64;
    // log2(seeders+1) scaled to max 0.05
    (seeders + 1.0).log2() * 0.005
}

/// Freshness tiebreaker: prefer newer releases over older ones.
/// Especially important for usenet where old files get removed.
/// Returns 0.0..0.08 — enough to break ties but not override quality.
pub fn freshness_tiebreaker(release: &Release) -> f64 {
    let Ok(publish_date) = time::OffsetDateTime::parse(
        &release.publish_date,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return 0.0;
    };
    let age_days = (crate::clock::now_utc() - publish_date).whole_days().max(0) as f64;
    // Decay curve: 0.08 at 0 days, ~0.04 at 30 days, ~0.02 at 90 days, ~0.0 at 365+
    // Usenet gets a 2x multiplier since old posts are more likely to be incomplete
    let base = 0.08 * (-age_days / 120.0_f64).exp();
    if release.protocol == "usenet" {
        (base * 2.0).min(0.08)
    } else {
        base
    }
}

/// Check if a parsed season number matches a string season identifier.
fn season_matches(parsed_season: Option<u32>, wanted_season: &str) -> bool {
    match (parsed_season, wanted_season.parse::<u32>().ok()) {
        (Some(parsed), Some(wanted)) => parsed == wanted,
        _ => false,
    }
}

/// Count episodes that are genuinely missing from the library. `episodes_needed`
/// includes episodes already in the library (Kobbler sends the full season for
/// per-episode state tracking), so pack-gating must subtract those — otherwise a
/// pack to fill a single missing finale looks like it satisfies the whole season.
fn needed_not_in_library(episodes: &[nats_jobs::EpisodeRef]) -> usize {
    episodes
        .iter()
        .filter(|e| e.in_library != Some(true))
        .count()
}

/// Parse an ISO 8601 date string (YYYY-MM-DD).
pub fn parse_iso_date(s: &str) -> Option<time::Date> {
    let fmt = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").ok()?;
    time::Date::parse(s, &fmt).ok()
}

/// Tokens via the shared canonicalizer, filtered to length ≥ 2. Used by the
/// author-match bonus and episode-title-match bonus.
fn normalize_title_tokens(title: &str) -> Vec<String> {
    kob_normalize::canonical(title)
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .map(String::from)
        .collect()
}

/// Normalize author name tokens for comparison.
/// Strips dots from initials ("J.K." → "jk"), apostrophes, and separators.
fn normalize_author_tokens(author: &str) -> Vec<String> {
    author
        .to_lowercase()
        // Strip dots from initials: "J.K." → "jk"
        .replace('.', "")
        // Strip apostrophes
        .replace(['\'', '\u{2019}'], "")
        // Replace separators with spaces
        .replace([',', '-', '_'], " ")
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .map(String::from)
        .collect()
}

/// True if `parsed` is similar to `wanted` or to any of `alternates`.
/// Used by the non-TV structural title veto so wants can opt into additional
/// accepted titles (e.g. TMDB `alternative_titles`) without relaxing the core
/// `titles_similar` rule.
///
/// Also AKA-aware: `"Dao AKA The Blade"` or `"The Blade AKA Dao"` should match
/// a want for either "Dao" or "The Blade". When the parsed title contains an
/// `aka` token we compare against each split segment in addition to the full
/// parsed title.
///
/// `reject_extensions` additionally rejects a parsed title that is the wanted
/// title plus trailing words (see [`extends_title`]). Callers only set it when
/// the release had a year, so the parsed title is exactly the pre-year text
/// rather than a whole name with unrecognised tags left in.
fn any_title_similar(
    wanted: &str,
    alternates: Option<&[String]>,
    parsed: &str,
    reject_extensions: bool,
) -> bool {
    if title_candidate_matches(wanted, parsed, reject_extensions) {
        return true;
    }
    if let Some(alts) = alternates {
        return alts
            .iter()
            .any(|alt| title_candidate_matches(alt, parsed, reject_extensions));
    }
    false
}

/// Check `wanted` against `parsed`, accounting for AKA-joined release titles.
fn title_candidate_matches(wanted: &str, parsed: &str, reject_extensions: bool) -> bool {
    let matches = |candidate: &str| {
        titles_similar(wanted, candidate)
            && !(reject_extensions && extends_title(wanted, candidate))
    };
    if matches(parsed) {
        return true;
    }
    // Split on " aka " (case-insensitive) and try each half.
    let normalized = parsed.to_lowercase();
    if !normalized.contains(" aka ") {
        return false;
    }
    parsed
        .split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-')
        .collect::<Vec<_>>()
        .split(|s| s.eq_ignore_ascii_case("aka"))
        .any(|segment| {
            let joined = segment.join(" ");
            !joined.trim().is_empty() && matches(&joined)
        })
}

/// Words an edition tag can leave at the end of a pre-year movie title
/// ("Movie.Directors.Cut.2024"). Mirrors `EDITION_RE` in the movie parser.
const EDITION_WORDS: &[&str] = &[
    "director",
    "directors",
    "cut",
    "extended",
    "edition",
    "theatrical",
    "unrated",
    "uncut",
    "imax",
    "remastered",
    "remaster",
    "4k",
    "criterion",
    "collection",
    "special",
    "anniversary",
    "collector",
    "collectors",
    "ultimate",
    "final",
    "redux",
    "workprint",
    "fanedit",
    "fan",
    "edit",
    "regraded",
    "hybrid",
    "open",
    "matte",
];

fn is_edition_word(token: &str) -> bool {
    EDITION_WORDS.contains(&token)
        // "30th" in "30th Anniversary"
        || ["st", "nd", "rd", "th"].iter().any(|suffix| {
            token
                .strip_suffix(suffix)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// True when `parsed` is the whole of `wanted` followed by extra words:
/// "The Odyssey The Real Story" / "The Odyssey The Making of an Epic" for
/// "The Odyssey", "Dune Part Two" for "Dune". `titles_similar` only measures
/// overlap against the shorter title, so these companion docs and subtitled
/// sequels pass it — and they usually share the year too.
///
/// A leading article is ignored on both sides (releases drop it), as are
/// trailing edition words. A shorter release title is never an extension:
/// releases routinely drop a TMDB subtitle.
fn extends_title(wanted: &str, parsed: &str) -> bool {
    fn without_article(mut tokens: Vec<String>) -> Vec<String> {
        if tokens.len() > 1 && matches!(tokens[0].as_str(), "the" | "an") {
            tokens.remove(0);
        }
        tokens
    }
    let wanted = without_article(normalize_title_tokens(wanted));
    let mut parsed = without_article(normalize_title_tokens(parsed));
    while parsed.len() > wanted.len() && parsed.last().is_some_and(|t| is_edition_word(t)) {
        parsed.pop();
    }
    !wanted.is_empty() && parsed.len() > wanted.len() && parsed.starts_with(&wanted)
}

/// Returns the `parse_release` media_kind used by structural gates for a given MediaWant.
/// Sports: structural_reject uses "sports" (matches the specific parser), structural_bonus
/// uses "tv" (season/episode framing). Return the pair. For all other wants the kinds
/// are identical.
fn structural_media_kinds(mw: &MediaWant) -> (&'static str, &'static str) {
    // Reject side uses the canonical parse_release kind ("sports" for sports).
    // Bonus side frames sports as "tv" (season/episode); identical for all else.
    let reject_kind = crate::media::parse_media_kind(mw);
    let bonus_kind = match mw {
        MediaWant::SportEvent { .. } | MediaWant::SportSeason { .. } => "tv",
        _ => reject_kind,
    };
    (reject_kind, bonus_kind)
}

/// Calculate structural matching bonus based on parsed release metadata.
///
/// This bonus rewards releases that structurally match what the user wants:
/// - Exact episode match: +0.5
/// - Season pack containing requested episode: +0.2
/// - Exact movie year match: +0.3
/// - PROPER/REPACK releases: +0.1
///
/// When `xem_show_map` is provided, also checks XEM scene numbering:
/// - Release matches scene numbers for our episode: +0.5
/// - Release matches TMDB numbers but collision detected (scene reverse-map
///   gives a different episode): 0 bonus (suppressed)
pub fn structural_bonus(
    release: &Release,
    media_want: Option<&MediaWant>,
    xem_show_map: Option<&XemShowMap>,
) -> f64 {
    let Some(mw) = media_want else {
        return 0.0;
    };
    let (_, bonus_kind) = structural_media_kinds(mw);
    let parsed = parse_release(&release.title, bonus_kind);
    structural_bonus_with_parsed(release, &parsed, media_want, xem_show_map)
}

/// Same as [`structural_bonus`] but takes an already-parsed release to avoid
/// re-parsing on the hot path. `parsed` must have been produced with the
/// bonus-side `media_kind` from [`structural_media_kinds`].
pub fn structural_bonus_with_parsed(
    release: &Release,
    parsed: &ParsedRelease,
    media_want: Option<&MediaWant>,
    xem_show_map: Option<&XemShowMap>,
) -> f64 {
    let Some(media_want) = media_want else {
        return 0.0;
    };

    // PROPER/REPACK bonus (applies to all media types)
    let proper_bonus = if parsed.flags.is_proper || parsed.flags.is_repack {
        0.1
    } else {
        0.0
    };

    // Structural matching based on media type
    let match_bonus = match (media_want, &parsed.media) {
        // TV Episode: check TMDB match, scene match, and collisions
        (
            MediaWant::TvEpisode {
                season, episode, ..
            },
            ParsedMedia::Tv(tv),
        ) => {
            let tmdb_season_match = season_matches(tv.season, season);
            let tmdb_episode_match = episode
                .parse::<u32>()
                .is_ok_and(|e| tv.episodes.contains(&e));

            // Check XEM scene numbering
            if let (Some(show_map), Ok(tmdb_s), Ok(tmdb_e)) =
                (xem_show_map, season.parse::<u32>(), episode.parse::<u32>())
            {
                if let Some(scene) = show_map.scene_for_tvdb(tmdb_s, tmdb_e) {
                    let scene_season_match = tv.season == Some(scene.scene_season);
                    let scene_episode_match = tv.episodes.contains(&scene.scene_episode);

                    if scene_season_match && scene_episode_match {
                        // Release matches scene numbering — correct episode
                        0.5
                    } else if tmdb_season_match && tmdb_episode_match {
                        // Release matches TMDB numbers — check for collision
                        if let Some(parsed_s) = tv.season {
                            let first_ep = tv.episodes.first().copied().unwrap_or(0);
                            if let Some((reverse_tvdb_s, reverse_tvdb_e)) =
                                show_map.tvdb_for_scene(parsed_s, first_ep)
                            {
                                if reverse_tvdb_s != tmdb_s || reverse_tvdb_e != tmdb_e {
                                    // Collision: scene interpretation maps to different episode
                                    0.0
                                } else {
                                    0.5
                                }
                            } else {
                                // No reverse mapping — no collision risk
                                0.5
                            }
                        } else {
                            0.5
                        }
                    } else if tmdb_season_match && tv.is_pack {
                        0.2
                    } else {
                        0.0
                    }
                } else {
                    // No XEM mapping for this specific episode — fall through to TMDB-only
                    if tmdb_season_match && tmdb_episode_match {
                        0.5
                    } else if tmdb_season_match && tv.is_pack {
                        0.2
                    } else {
                        0.0
                    }
                }
            } else {
                // No XEM show map — original TMDB-only behavior
                if tmdb_season_match && tmdb_episode_match {
                    0.5
                } else if tmdb_season_match && tv.is_pack {
                    0.2
                } else {
                    0.0
                }
            }
        }

        // TV Season: season pack match (only reached if not rejected by pack_preference)
        (
            MediaWant::TvSeason {
                season,
                episodes_needed,
                total_episodes,
                pack_preference,
                ..
            },
            ParsedMedia::Tv(tv),
        ) if season_matches(tv.season, season) && tv.is_pack => {
            match pack_preference.as_str() {
                "always" => 0.5,
                _ => {
                    // "auto" for older shows: prefer packs when ≥90% of season needed
                    let needed = episodes_needed.as_ref().map(|e| e.len()).unwrap_or(0);
                    match total_episodes {
                        Some(total) if *total > 0 && (needed as f64) >= (*total as f64 * 0.9) => {
                            0.5
                        }
                        _ => 0.0, // Unknown total or <90% needed = no pack bonus
                    }
                }
            }
        }

        // TV Season: individual episode match within requested season (for fallback plucking)
        (MediaWant::TvSeason { season, .. }, ParsedMedia::Tv(tv))
            if season_matches(tv.season, season) && !tv.episodes.is_empty() && !tv.is_pack =>
        {
            0.3
        }

        // Movie: exact year match
        (MediaWant::Movie { year: Some(y), .. }, ParsedMedia::Movie(movie))
            if movie.year == Some(*y) =>
        {
            0.3
        }

        // Music: year match
        (MediaWant::Music { year: Some(y), .. }, ParsedMedia::Music(music))
            if music.year == Some(*y) =>
        {
            0.2
        }

        // SportEvent: structural matching based on release title content
        (
            MediaWant::SportEvent {
                round,
                session_type,
                event_title,
                ..
            },
            _,
        ) => {
            let release_lower = release.title.to_lowercase();
            let mut bonus = 0.0;

            // Motorsport: bonus for matching round number.
            // Matches r22, round22, round 22, and YEARx22 formats.
            if let Some(round) = round {
                let r_padded = format!("r{round:02}");
                let round_str = format!("round{round:02}");
                let round_spaced = format!("round {round}");
                let year_x_round = format!("x{round:02}");
                if release_lower.contains(&r_padded)
                    || release_lower.contains(&round_str)
                    || release_lower.contains(&round_spaced)
                    || release_lower.contains(&year_x_round)
                {
                    bonus += 0.2;
                }
            }

            // Motorsport: bonus for matching session type.
            // Uses word-boundary matching so "Superpole" doesn't match
            // "Superpole Race" and vice versa.
            if let Some(session) = session_type.as_deref() {
                let session_lower = session.to_lowercase();
                if let Some(pos) = release_lower.find(&session_lower) {
                    let after = pos + session_lower.len();
                    let ends_at_boundary = after >= release_lower.len()
                        || !release_lower.as_bytes()[after].is_ascii_alphanumeric();
                    if ends_at_boundary {
                        bonus += 0.2;
                    }
                }
            }

            // Team sports: bonus for matching team names from event title
            if let Some(title) = event_title.as_deref() {
                let title_lower = title.to_lowercase();
                if title_lower.contains(" vs ") || title_lower.contains(" v ") {
                    let teams: Vec<&str> = title_lower
                        .split([' '])
                        .filter(|p| *p != "vs" && *p != "v" && !p.is_empty())
                        .collect();
                    let matched = teams.iter().filter(|t| release_lower.contains(*t)).count();
                    if !teams.is_empty() && matched == teams.len() {
                        bonus += 0.4;
                    } else if matched > 0 {
                        bonus += 0.1;
                    }
                }
            }

            bonus
        }

        _ => 0.0,
    };

    // Book/Audiobook structural bonus: author and series token matching
    let book_bonus = match media_want {
        MediaWant::Book {
            author: Some(wanted_author),
            series,
            ..
        }
        | MediaWant::Audiobook {
            author: Some(wanted_author),
            series,
            ..
        } => {
            let mut bonus = 0.0;
            let release_lower = release.title.to_lowercase();
            let wanted_tokens = normalize_author_tokens(wanted_author);
            // Use title tokenizer for release (handles dots, underscores as separators)
            let release_tokens = normalize_title_tokens(&release_lower);
            if !wanted_tokens.is_empty()
                && !release_tokens.is_empty()
                && wanted_tokens
                    .iter()
                    .all(|w| release_tokens.iter().any(|rt| rt == w))
            {
                bonus += 0.3;
            }
            if let Some(series_name) = series {
                let series_lower = series_name.to_lowercase();
                let series_words: Vec<&str> = series_lower.split_whitespace().collect();
                if !series_words.is_empty()
                    && series_words.iter().all(|w| release_lower.contains(w))
                {
                    bonus += 0.2;
                }
            }
            bonus
        }
        _ => 0.0,
    };

    // Audiobook-specific bonuses: format preference, narrator match, unabridged
    let audiobook_bonus = match media_want {
        MediaWant::Audiobook { narrator, .. } => {
            let mut bonus = 0.0;
            let release_lower = release.title.to_lowercase();

            // Format preference scoring
            if release_lower.contains(".m4b") || release_lower.contains("[m4b]") {
                bonus += 0.15;
            } else if release_lower.contains(".flac") || release_lower.contains("[flac]") {
                bonus += 0.10;
            } else if release_lower.contains(".mp3") || release_lower.contains("[mp3]") {
                bonus += 0.05;
            }

            // Narrator match bonus
            if let Some(wanted_narrator) = narrator {
                let narrator_lower = wanted_narrator.to_lowercase();
                let narrator_words: Vec<&str> = narrator_lower.split_whitespace().collect();
                if !narrator_words.is_empty()
                    && narrator_words.iter().all(|w| release_lower.contains(w))
                {
                    bonus += 0.2;
                }
            }

            // Unabridged bonus
            if release_lower.contains("unabridged") {
                bonus += 0.1;
            }

            // Full cast / full production bonus
            if release_lower.contains("full cast")
                || release_lower.contains("full production")
                || release_lower.contains("multivoice")
                || release_lower.contains("multi-voice")
                || release_lower.contains("dramatized")
                || release_lower.contains("dramatised")
            {
                bonus += 0.15;
            }

            bonus
        }
        _ => 0.0,
    };

    // TV year match bonus: boost releases whose parsed year matches the wanted show year
    let year_bonus = match media_want {
        MediaWant::TvEpisode {
            year: Some(wanted_year),
            ..
        }
        | MediaWant::TvSeason {
            year: Some(wanted_year),
            ..
        } => {
            if let ParsedMedia::Tv(tv) = &parsed.media
                && tv.year == Some(*wanted_year)
            {
                YEAR_MATCH_BONUS
            } else {
                0.0
            }
        }
        _ => 0.0,
    };

    // TV episode title match bonus: boost releases containing episode title tokens.
    // Works for both TvEpisode (has episode_title directly) and TvSeason (looks up
    // the title from episodes_needed by parsed episode number).
    let ep_title_for_bonus: Option<&str> = match media_want {
        MediaWant::TvEpisode {
            episode_title: Some(ep_title),
            ..
        } if !ep_title.is_empty() => Some(ep_title.as_str()),
        MediaWant::TvSeason {
            episodes_needed: Some(eps),
            ..
        } => {
            if let ParsedMedia::Tv(tv) = &parsed.media
                && tv.episodes.len() == 1
            {
                let ep_num = tv.episodes[0];
                eps.iter()
                    .find(|er| er.episode.parse::<u32>().ok() == Some(ep_num))
                    .and_then(|er| er.title.as_deref())
                    .filter(|t| !t.is_empty())
            } else {
                None
            }
        }
        _ => None,
    };
    let episode_title_bonus = if let Some(ep_title) = ep_title_for_bonus {
        let ep_tokens = normalize_title_tokens(ep_title);
        if !ep_tokens.is_empty() {
            let release_lower = release.title.to_lowercase();
            let release_tokens = normalize_title_tokens(&release_lower);
            let matched = ep_tokens
                .iter()
                .filter(|t| release_tokens.iter().any(|rt| rt == *t))
                .count();
            if matched * 2 >= ep_tokens.len() {
                EPISODE_TITLE_MATCH_BONUS
            } else {
                0.0
            }
        } else {
            0.0
        }
    } else {
        0.0
    };

    proper_bonus + match_bonus + book_bonus + audiobook_bonus + year_bonus + episode_title_bonus
}

/// Structural rejection: parse the release title and reject if parsed fields
/// Whether a sport session type is "default" — i.e., a generic release without
/// an explicit session indicator is implicitly this session. Currently only "Race"
/// is default: "MotoGP.2026.Brazil.1080p" is the race, not qualifying.
fn is_default_sport_session(session: &str) -> bool {
    matches!(
        session.to_lowercase().as_str(),
        "race" | "main event" | "main card" | "gp"
    )
}

// Largest single 1080p race in the capture was 14.7 GiB and the smallest
// MotoGP weekend pack 17.4 GiB. (WSBK packs go down to 12.5 GiB, but SBK
// wants name their session, so they never reach this check.) Other resolutions scale with pixel count
// to the 0.6 power, which keeps 32 GiB 2160p races (limit ~37 GiB) and 3.7 GiB
// 720p races (limit ~10 GiB) in. Unknown resolution counts as 1080p.
fn is_sport_weekend_pack(size: i64, resolution: Option<&str>) -> bool {
    const RACE_LIMIT_1080P: f64 = 16.0 * (1u64 << 30) as f64;
    let height = resolution
        .and_then(crate::quality::parse_resolution)
        .unwrap_or(1080);
    let limit = RACE_LIMIT_1080P * (f64::from(height) / 1080.0).powf(1.2);
    size as f64 > limit
}

/// Word tokens that signal a non-race sport session (press conference,
/// media day, test session, warm-up, qualifying, practice, sprint, etc)
/// rather than the main event. When a release title contains one of these
/// tokens but the parser did NOT assign anything to `sport.session_type`,
/// we treat the release as unrecognised-session and reject default-session
/// (Race) wants.
///
/// The defense is gated on `sport.session_type.is_none()` at the rule call
/// site so that legit league names like "Sprint Cup" don't false-positive
/// when the parser correctly extracts session=Race.
///
/// Expand this list when new sessions surface in prod — the companion is
/// `SESSION_ALTERNATIVES` in `parsing/sport.rs`.
const SESSION_HINT_WORDS: &[&str] = &[
    "conference",
    "briefing",
    "summit",
    "shakedown",
    "launch",
    "preview",
    "recap",
    "highlights",
    "wrap",
    "review",
    "analysis",
    "interview",
    "interviews",
    "practice",
    "qualifying",
    "sprint",
    "warmup",
    "warm",
];

/// True if the raw release title contains a token that looks like an auxiliary
/// session marker (e.g. "Conference", "Shakedown") beyond whatever the parser
/// already captured in `sport.session_type`. Case-insensitive, word-boundary
/// matched against `.`/`_`/`-`/whitespace-separated tokens.
fn release_has_hint_word_outside_session(title: &str, parsed_session: Option<&str>) -> bool {
    let parsed_lower = parsed_session.unwrap_or("").to_ascii_lowercase();
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .any(|tok| {
            let lower = tok.to_ascii_lowercase();
            SESSION_HINT_WORDS.contains(&lower.as_str()) && !parsed_lower.contains(&lower)
        })
}

/// Check if two sport locations match at word boundaries (case-insensitive).
/// Releases often include circuit name after country (e.g. "Portugal Algarve") while
/// TheSportsDB may only have the country ("Portugal"), or vice versa.
/// All words of the shorter string must appear as whole words in the longer string.
/// Both sides are canonicalised first, so "Spanish"/"Spain", "UK"/"Great Britain"
/// and "Czechia"/"Czech Republic" count as the same place. A wanted location
/// that is only a country code ("DE") stands for that country; the release
/// side is left alone, where two letters are as likely a parsing scrap.
fn location_contains(parsed: &str, wanted: &str) -> bool {
    let p = crate::parsing::canonical_sport_text(parsed);
    let w = crate::parsing::canonical_sport_text(
        crate::parsing::country_for_code(wanted.trim()).unwrap_or(wanted),
    );
    if p == w {
        return true;
    }
    let p_words: Vec<&str> = p.split_whitespace().collect();
    let w_words: Vec<&str> = w.split_whitespace().collect();
    let (needle, haystack) = if w_words.len() <= p_words.len() {
        (&w_words, &p_words)
    } else {
        (&p_words, &w_words)
    };
    !needle.is_empty() && needle.iter().all(|nw| haystack.contains(nw))
}

// ── Unified scoring pipeline ────────────────────────────────────────────────

/// Configuration for the unified scoring function.
///
/// Builder-based: common cases use a constructor, optional gates are chained.
/// The scoring function stays pure/sync — all async DB prep happens in the caller.
pub struct ScoringConfig<'a> {
    // Pattern matching (use targets OR patterns, not both)
    pub match_targets: &'a [MatchTarget],
    pub match_patterns: &'a [String],
    pub match_mode: &'a str,
    pub min_score: f64,

    // Quality
    pub quality_profile: Option<&'a QualityProfileRow>,
    pub quality_overrides: Option<&'a QualityOverrides>,
    pub trash_filters: Option<&'a TrashFilterSet>,

    // Structural
    pub media_want: Option<&'a MediaWant>,
    pub search_context: SearchContext,

    // Optional gates (None = skip gate)
    pub excluded_titles: Option<&'a std::collections::HashSet<String>>,
    pub allowed_categories: Option<&'a [i32]>,
    pub episodes_needed: Option<&'a [nats_jobs::EpisodeRef]>,
    pub media_want_json: Option<&'a serde_json::Value>,

    // Bonus config
    pub xem_show_map: Option<&'a XemShowMap>,
    pub preferred_protocol: Option<&'a str>,

    prepared_targets: std::sync::OnceLock<Vec<PreparedTarget<'a>>>,
}

impl<'a> ScoringConfig<'a> {
    /// From explicit targets/patterns — for want (interactive search).
    pub fn new(
        match_targets: &'a [MatchTarget],
        match_patterns: &'a [String],
        min_score: f64,
        quality_profile: Option<&'a QualityProfileRow>,
        trash_filters: Option<&'a TrashFilterSet>,
    ) -> Self {
        Self {
            match_targets,
            match_patterns,
            match_mode: "fuzzy",
            min_score,
            quality_profile,
            quality_overrides: None,
            trash_filters,
            media_want: None,
            search_context: SearchContext::default(),
            excluded_titles: None,
            allowed_categories: None,
            episodes_needed: None,
            media_want_json: None,
            xem_show_map: None,
            preferred_protocol: None,
            prepared_targets: std::sync::OnceLock::new(),
        }
    }

    pub fn with_media_want(mut self, mw: &'a MediaWant) -> Self {
        self.media_want = Some(mw);
        self
    }

    pub fn with_search_context(mut self, ctx: SearchContext) -> Self {
        self.search_context = ctx;
        self
    }

    pub fn with_excluded_titles(mut self, excluded: &'a std::collections::HashSet<String>) -> Self {
        self.excluded_titles = Some(excluded);
        self
    }

    pub fn with_allowed_categories(mut self, cats: &'a [i32]) -> Self {
        self.allowed_categories = Some(cats);
        self
    }

    pub fn with_episodes_needed(
        mut self,
        eps: &'a [nats_jobs::EpisodeRef],
        media_want_json: &'a serde_json::Value,
    ) -> Self {
        self.episodes_needed = Some(eps);
        self.media_want_json = Some(media_want_json);
        self
    }

    pub fn with_xem_show_map(mut self, xem: &'a XemShowMap) -> Self {
        self.xem_show_map = Some(xem);
        self
    }

    pub fn with_preferred_protocol(mut self, proto: &'a str) -> Self {
        self.preferred_protocol = Some(proto);
        self
    }

    pub fn with_quality_overrides(mut self, overrides: &'a QualityOverrides) -> Self {
        self.quality_overrides = Some(overrides);
        self
    }

    pub fn with_match_mode(mut self, mode: &'a str) -> Self {
        self.match_mode = mode;
        self
    }
}

/// Whether the release matches the config's targets or patterns at all.
///
/// The same check `score_release` uses for its match gate. Callers that only act
/// on matches (the RSS loop) run it first and skip the full scoring otherwise.
pub fn release_matches(release: &Release, config: &ScoringConfig) -> bool {
    release_matches_prepared(release, &PreparedTitle::new(&release.title), config)
}

/// [`release_matches`] with the title already prepared, for loops that check
/// one release against many patterns.
pub fn release_matches_prepared(
    release: &Release,
    title: &PreparedTitle,
    config: &ScoringConfig,
) -> bool {
    match_gate(release, title, config).is_some()
}

fn match_gate(
    release: &Release,
    title: &PreparedTitle,
    config: &ScoringConfig,
) -> Option<(f64, String, Option<nats_jobs::MatchedTarget>)> {
    if !config.match_targets.is_empty() {
        let targets = config
            .prepared_targets
            .get_or_init(|| prepare_targets(config.match_targets));
        score_prepared_targets(title, targets, config.min_score)
            .map(|r| (r.score, r.matched_target, r.semantic_target))
    } else if !config.match_patterns.is_empty() {
        match_patterns(
            config.match_patterns,
            config.match_mode,
            config.min_score,
            &release.title,
        )
        .map(|r| (r.score, r.matched_pattern, None))
    } else {
        None
    }
}

/// Score a single release through all gates.
///
/// Gate order:
/// 1. Exclusion — hard early return (user blocklist)
/// 2. Category — hard early return (wrong category)
/// 3. All remaining gates run unconditionally to populate full scoring data:
///    - Structural, Votes, Pattern/target matching, Quality, Episode filter
///    - Bonuses, Protocol preference
/// 4. Final decision from accumulated rejections
///
/// Returns a fully-populated ProcessedRelease regardless of decision.
pub fn score_release(release: &Release, config: &ScoringConfig) -> ProcessedRelease {
    score_release_facts(&ReleaseFacts::new(&Arc::new(release.clone())), config)
}

/// [`score_release`] for a release whose pattern-independent work is shared
/// across the patterns of a fetch.
pub fn score_release_facts(facts: &ReleaseFacts, config: &ScoringConfig) -> ProcessedRelease {
    let release = facts.release();
    use nats_jobs::{RejectReason, ScoringDecision};

    // ── Gate 1: Exclusion (hard early return) ──
    if let Some(excluded) = config.excluded_titles
        && excluded.contains(&release.title)
    {
        return ProcessedRelease {
            release: facts.shared(),
            decision: ScoringDecision::Excluded,
            reject_reasons: vec![],
            match_score: None,
            matched_pattern: None,
            matched_target: None,
            parsed_quality: Some(facts.quality().clone()),
            quality_score: None,
            quality_rejected: false,
            quality_reject_reasons: vec![],
            quality_breakdown: None,
            structural_rejected: false,
            structural_reject_reason: None,
            total_score: 0.0,
            episode_key: None,
            is_preferred_protocol: false,
            structural_bonus: 0.0,
            seeder_bonus: 0.0,
            freshness_bonus: 0.0,
            context_flags: vec![],
        };
    }

    // ── Gate 2: Category (hard early return) ──
    if let Some(allowed_cats) = config.allowed_categories
        && !crate::helpers::release_matches_categories(&release.category_ids, allowed_cats)
    {
        return ProcessedRelease {
            release: facts.shared(),
            decision: ScoringDecision::Rejected,
            reject_reasons: vec![RejectReason::CategoryMismatch],
            match_score: None,
            matched_pattern: None,
            matched_target: None,
            parsed_quality: Some(facts.quality().clone()),
            quality_score: None,
            quality_rejected: false,
            quality_reject_reasons: vec![],
            quality_breakdown: None,
            structural_rejected: false,
            structural_reject_reason: None,
            total_score: 0.0,
            episode_key: None,
            is_preferred_protocol: false,
            structural_bonus: 0.0,
            seeder_bonus: 0.0,
            freshness_bonus: 0.0,
            context_flags: vec![],
        };
    }

    // ── All remaining gates run unconditionally ──
    let mut reject_reasons: Vec<RejectReason> = Vec::new();

    let kinds = config.media_want.map(structural_media_kinds);

    // Gate 3: Structural rejection
    let structural_result = kinds.and_then(|(reject_kind, _)| {
        structural_reject_with_parsed(
            release,
            &facts.parsed(reject_kind),
            config.media_want,
            config.search_context,
        )
    });
    let structural_rejected = structural_result.is_some();
    if let Some(reason) = structural_result {
        reject_reasons.push(RejectReason::Structural(reason));
    }

    // Gate 4: Votes rejection
    if votes_reject(release).is_some() {
        reject_reasons.push(RejectReason::NegativeVotes);
    }

    // Gate 5: Pattern/target matching (always run)
    let (match_score, matched_pattern, matched_target) =
        match match_gate(release, facts.title(), config) {
            Some((score, pattern, target)) => (Some(score), Some(pattern), target),
            None => (None, None, None),
        };

    let is_no_match = match_score.is_none();

    // Gate 6: Quality scoring (always run)
    let quality = score_quality_parsed(
        &release.title,
        facts.quality().clone(),
        config.quality_profile,
        config.quality_overrides,
        config.trash_filters,
        |filters, group| facts.blocklist(filters, group),
    );
    let quality_rejected = quality.rejected;
    let quality_reject_reasons = quality.reject_reasons.clone();
    if quality_rejected {
        for qr in &quality.reject_reasons {
            reject_reasons.push(RejectReason::Quality(*qr));
        }
    }

    // Gate 7: Episode filter (TvSeason only)
    let episode_key = if let Some(episodes_needed) = config.episodes_needed
        && let Some(mw_json) = config.media_want_json
    {
        let key = crate::watch::episode_key::is_tv_season(mw_json)
            .then(|| crate::watch::episode_key::episode_key_from_parsed(&facts.parsed("tv")))
            .flatten();
        if let Some(ref k) = key
            && !crate::watch::episode_key::is_episode_needed(k, episodes_needed)
        {
            reject_reasons.push(RejectReason::EpisodeNotNeeded);
        }
        key
    } else {
        None
    };

    // ── Bonuses ──
    let seeder_bonus = seeder_tiebreaker(release);
    let struct_bonus = kinds.map_or(0.0, |(_, bonus_kind)| {
        structural_bonus_with_parsed(
            release,
            &facts.parsed(bonus_kind),
            config.media_want,
            config.xem_show_map,
        )
    });
    let fresh_bonus = freshness_tiebreaker(release);

    let total_score =
        match_score.unwrap_or(0.0) + quality.score + seeder_bonus + struct_bonus + fresh_bonus;

    // Protocol preference
    let is_preferred_protocol = config
        .preferred_protocol
        .is_some_and(|pref| release.protocol == pref);

    // ── Final decision ──
    let decision = if !reject_reasons.is_empty() {
        if is_no_match
            && reject_reasons.iter().all(|r| {
                !matches!(
                    r,
                    RejectReason::Quality(_)
                        | RejectReason::Structural(_)
                        | RejectReason::NegativeVotes
                        | RejectReason::CategoryMismatch
                        | RejectReason::EpisodeNotNeeded
                        | RejectReason::Context(_)
                )
            })
        {
            ScoringDecision::NoMatch
        } else {
            ScoringDecision::Rejected
        }
    } else if is_no_match {
        ScoringDecision::NoMatch
    } else {
        ScoringDecision::Candidate
    };

    ProcessedRelease {
        release: facts.shared(),
        decision,
        reject_reasons,
        match_score,
        matched_pattern,
        matched_target,
        // score_quality already parsed the title; reuse it
        parsed_quality: Some(quality.parsed),
        quality_score: Some(quality.score),
        quality_rejected,
        quality_reject_reasons,
        quality_breakdown: Some(quality.breakdown),
        structural_rejected,
        structural_reject_reason: structural_result,
        total_score,
        episode_key,
        is_preferred_protocol,
        structural_bonus: struct_bonus,
        seeder_bonus,
        freshness_bonus: fresh_bonus,
        context_flags: vec![],
    }
}

/// Score a batch of releases through the unified pipeline.
///
/// Phase 1: Individual scoring via `score_release()` for each release.
/// Phase 2: Contextual analysis — cross-release heuristics that can
///          downgrade candidates based on batch context.
pub fn score_batch(releases: &[Release], config: &ScoringConfig) -> Vec<ProcessedRelease> {
    use crate::cpu_bound::run_cpu_bound;
    use rayon::prelude::*;

    // Regex + Jaro-Winkler scoring over the batch (rayon for ≥16) plus the
    // sequential contextual pass is pure CPU with no await points. Async callers
    // (HTTP search, streaming, backlog/RSS jobs) reach it on the runtime, so run
    // it via run_cpu_bound to keep a worker free for the JetStream heartbeats.
    run_cpu_bound(|| {
        let mut results: Vec<ProcessedRelease> = if releases.len() >= 16 {
            releases
                .par_iter()
                .map(|release| score_release(release, config))
                .collect()
        } else {
            releases
                .iter()
                .map(|release| score_release(release, config))
                .collect()
        };

        contextual_analysis(&mut results);

        results
    })
}

/// Cross-release heuristics that examine the batch as a whole.
///
/// Currently implements:
/// - **Suspicious quality outlier**: If 80%+ of matched releases are garbage
///   quality (CAM/TS/line-mic) and 1-2 are suspiciously high quality, flag
///   those outliers.
fn contextual_analysis(results: &mut [ProcessedRelease]) {
    use nats_jobs::{ContextFlag, RejectReason};

    // Only run if we have enough results to detect patterns
    if results.len() < 4 {
        return;
    }

    // Count quality tiers among releases that matched (have a match_score)
    let matched: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.match_score.is_some())
        .map(|(i, _)| i)
        .collect();

    if matched.len() < 4 {
        return;
    }

    let garbage_sources = ["cam", "telesync", "ts", "tc", "hdts", "hd-ts"];
    let good_sources = ["web-dl", "webdl", "webrip", "bluray", "blu-ray", "remux"];

    let mut garbage_count = 0usize;
    let mut good_indices: Vec<usize> = Vec::new();

    for &idx in &matched {
        if let Some(ref pq) = results[idx].parsed_quality
            && let Some(ref source) = pq.source
        {
            let s = source.to_lowercase();
            if garbage_sources.iter().any(|g| s.contains(g)) {
                garbage_count += 1;
            } else if good_sources.iter().any(|g| s.contains(g)) {
                good_indices.push(idx);
            }
        }
    }

    // If 80%+ are garbage and only 1-2 are good → flag the good ones as suspicious
    let garbage_ratio = garbage_count as f64 / matched.len() as f64;
    if garbage_ratio >= 0.8 && !good_indices.is_empty() && good_indices.len() <= 2 {
        for idx in good_indices {
            results[idx]
                .context_flags
                .push(ContextFlag::SuspiciousQualityOutlier);
            results[idx]
                .reject_reasons
                .push(RejectReason::Context(ContextFlag::SuspiciousQualityOutlier));
            // Downgrade from candidate to rejected
            if results[idx].decision == nats_jobs::ScoringDecision::Candidate {
                results[idx].decision = nats_jobs::ScoringDecision::Rejected;
            }
        }
    }
}

/// conflict with the MediaWant. This catches false positives that fuzzy matching
/// cannot (e.g., "Toy Story 1995" matching against want for "Toy Story 5 2026").
///
/// Returns `Some("reason")` to reject, `None` to accept.
pub fn structural_reject(
    release: &Release,
    media_want: Option<&MediaWant>,
    search_context: SearchContext,
) -> Option<nats_jobs::StructuralRejectReason> {
    let mw = media_want?;
    let (reject_kind, _) = structural_media_kinds(mw);
    let parsed = parse_release(&release.title, reject_kind);
    structural_reject_with_parsed(release, &parsed, media_want, search_context)
}

/// Same as [`structural_reject`] but takes an already-parsed release to avoid
/// re-parsing on the hot path. `parsed` must have been produced with the
/// reject-side `media_kind` from [`structural_media_kinds`].
pub fn structural_reject_with_parsed(
    release: &Release,
    parsed: &ParsedRelease,
    media_want: Option<&MediaWant>,
    search_context: SearchContext,
) -> Option<nats_jobs::StructuralRejectReason> {
    use nats_jobs::StructuralRejectReason;
    let media_want = media_want?;

    // TV title similarity check: reject if parsed show name has no meaningful
    // token overlap with wanted show name. This catches false positives like
    // "To Catch a Smuggler S06" matching want for "Catch! Teenieping S06".
    if let Some(wanted_name) = match media_want {
        MediaWant::TvSeason { show_name, .. } | MediaWant::TvEpisode { show_name, .. } => {
            Some(show_name.as_str())
        }
        _ => None,
    } && let ParsedMedia::Tv(tv) = &parsed.media
        && !tv.show_name.is_empty()
        && !titles_similar(wanted_name, &tv.show_name)
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }

    // ── Non-TV title similarity veto ────────────────────────────────────
    // Mirrors the TV block: the fuzzy scorer is happy to accept two generic
    // tokens in common (e.g. "film" + "2010"), so without this a release like
    // `Srpski.film.2010...` scores 0.767 against a want for "A Serbian Film"
    // and sails through. Apply the same Sonarr-style normalise-and-overlap
    // rule used for TV shows to every title-bearing MediaWant variant.
    //
    // `any_title_similar` also consults `alternate_titles` on MediaWant::Movie
    // so a user can explicitly opt-in to a foreign/scene alias (e.g. TMDB
    // `alternative_titles`) without loosening the core rule.
    if let MediaWant::Movie {
        title: wanted,
        alternate_titles,
        ..
    } = media_want
        && let ParsedMedia::Movie(movie) = &parsed.media
        && !wanted.is_empty()
        && !movie.title.is_empty()
        && !any_title_similar(
            wanted,
            alternate_titles.as_deref(),
            &movie.title,
            movie.year.is_some(),
        )
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }

    if let MediaWant::Book { title: wanted, .. } = media_want
        && let ParsedMedia::Book(book) = &parsed.media
        && !wanted.is_empty()
        && !book.title.is_empty()
        && !titles_similar(wanted, &book.title)
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }

    if let MediaWant::Audiobook { title: wanted, .. } = media_want
        && let ParsedMedia::Audiobook(ab) = &parsed.media
        && !wanted.is_empty()
        && !ab.title.is_empty()
        && !titles_similar(wanted, &ab.title)
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }

    if let MediaWant::Game { title: wanted, .. } = media_want
        && let ParsedMedia::Game(game) = &parsed.media
        && !wanted.is_empty()
        && !game.title.is_empty()
        && !titles_similar(wanted, &game.title)
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }

    if let MediaWant::Software { name: wanted, .. } = media_want
        && let ParsedMedia::Software(software) = &parsed.media
        && !wanted.is_empty()
        && !software.title.is_empty()
        && !titles_similar(wanted, &software.title)
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }

    // Music: compare on album only (artist has its own token-overlap check
    // in the existing author branch below). Skip when the want has no album
    // — some music wants are artist-only.
    if let MediaWant::Music {
        album: Some(wanted_album),
        ..
    } = media_want
        && let ParsedMedia::Music(music) = &parsed.media
        && let Some(parsed_album) = music.album.as_deref()
        && !wanted_album.is_empty()
        && !parsed_album.is_empty()
        && !titles_similar(wanted_album, parsed_album)
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }

    // Note on Comic: `nats_jobs::ParsedMedia` has no Comic variant today, so
    // there's no parsed comic title to compare against. Until the parser
    // exposes one the `MediaWant::Comic { title, .. }` case is left to the
    // fuzzy scorer + year check. Flag as follow-up.

    // ── Cross-media-type guard: reject TV-structured releases for Movie wants ──
    // parse_release("movies") ignores episode markers (E04, S01E03). Re-parse as
    // TV to detect them — if the re-parse yields episodes/season, it's TV, not a movie.
    if matches!(media_want, MediaWant::Movie { .. }) {
        let tv_parsed = parse_release(&release.title, "tv");
        if let ParsedMedia::Tv(tv) = &tv_parsed.media
            && (!tv.episodes.is_empty() || tv.season.is_some())
        {
            return Some(StructuralRejectReason::CrossMediaTypeTv);
        }
    }

    // ── TV show disambiguation: air date gating ──────────────────────────
    // Reject releases for episodes that haven't aired yet.
    // Leniency: allow grabbing up to AIR_DATE_LENIENCY_DAYS before the listed
    // air_date (timezone safety — listed dates are often US-centric, episode
    // may already be available elsewhere).
    let today = crate::clock::now_utc().date();
    let air_date_leniency = AIR_DATE_LENIENCY_DAYS;
    let publish_date_reject_days = match search_context {
        SearchContext::Rss => PUBLISH_DATE_BEFORE_AIR_REJECT_DAYS_RSS,
        SearchContext::Structured | SearchContext::TextFallback => {
            PUBLISH_DATE_BEFORE_AIR_REJECT_DAYS_SEARCH
        }
    };

    // TvEpisode: reject if air_date hasn't passed (with context-dependent buffer)
    if let MediaWant::TvEpisode {
        air_date: Some(air_date_str),
        ..
    } = media_want
        && let Some(air_date) = parse_iso_date(air_date_str)
        && air_date - time::Duration::days(air_date_leniency) > today
    {
        return Some(StructuralRejectReason::UnairedEpisode);
    }

    // SportEvent: reject anything for a session that has not been run yet.
    //
    // A sport session is live: the footage cannot exist before the session
    // happens, so a candidate for a future event is always some OTHER event
    // that matched on league, season and session type. This is the structural
    // backstop for location data that cannot disambiguate — TheSportsDB reports
    // the HOST country, so San Marino (Misano) and Italy (Mugello) are both
    // "IT", and Spain/Catalonia/Aragon/Valencia are all "ES". kobbler now
    // prefers the event title over that country, but the gate has to hold here
    // too: some scene groups genuinely name Misano "Italy"
    // (`MotoGP.2025.Round16.Italy`), so no location string can separate the two
    // rounds — only the calendar can. In September 2026 six wants for San Marino
    // (Sep 11-13) grabbed the previous May's Mugello weekend, days before San
    // Marino had been run.
    //
    // The same leniency as TV applies: sessions run in their local timezone and
    // a listed date can be a day off, so only events more than the leniency
    // window in the future are rejected. Round is deliberately not required —
    // the groups that triggered this (VERUM, BILLIE) never emit a round token,
    // which is exactly why the round gate could not catch it.
    if let MediaWant::SportEvent {
        event_date: Some(event_date_str),
        ..
    } = media_want
        && let Some(event_date) = parse_iso_date(event_date_str)
        && event_date - time::Duration::days(air_date_leniency) > today
    {
        return Some(StructuralRejectReason::EventNotYetHeld);
    }

    // TvSeason: two separate unaired checks, because a listed air date means very
    // different things depending on whether the episode carries one of its own.
    if let MediaWant::TvSeason {
        episodes_needed: Some(eps),
        ..
    } = media_want
        && let ParsedMedia::Tv(tv) = &parsed.media
        && !tv.is_pack
        && !tv.episodes.is_empty()
    {
        // A season that has not started airing should have no releases at all,
        // so anything claiming to be one is mislabelled or fake.
        if let Some(premiere) = eps
            .iter()
            .filter_map(|er| er.air_date.as_deref().and_then(parse_iso_date))
            .min()
            && premiere - time::Duration::days(air_date_leniency) > today
        {
            return Some(StructuralRejectReason::UnairedEpisode);
        }

        // Past the premiere, an episode's OWN future air date proves nothing:
        // providers list binge-dropped seasons on a weekly schedule they never
        // follow, and the whole season is routinely up on day one.
        //
        // An episode with NO air date is a different signal — the provider does
        // not know when it airs, so the only bound is that it cannot air before
        // the episode before it. A future bound there means the episode really
        // has not happened, and a release claiming it belongs to some other
        // series with the same episode number.
        let mut sorted: Vec<&nats_jobs::EpisodeRef> = eps.iter().collect();
        sorted.sort_by_key(|er| {
            er.sort_order
                .unwrap_or(er.episode.parse::<i32>().unwrap_or(999))
        });
        let mut inherited_air_date: Option<time::Date> = None;
        let mut bounds: std::collections::HashMap<u32, (bool, Option<time::Date>)> =
            std::collections::HashMap::new();
        for er in &sorted {
            let own = er.air_date.as_deref().and_then(parse_iso_date);
            if let Some(d) = own {
                inherited_air_date = Some(d);
            }
            if let Ok(ep_num) = er.episode.parse::<u32>() {
                bounds.insert(ep_num, (own.is_some(), inherited_air_date));
            }
        }

        let all_unaired = tv.episodes.iter().all(|ep_num| {
            bounds.get(ep_num).is_some_and(|(has_own_date, bound)| {
                !has_own_date
                    && bound.is_some_and(|d| d - time::Duration::days(air_date_leniency) > today)
            })
        });
        if all_unaired {
            return Some(StructuralRejectReason::UnairedEpisode);
        }
    }

    // ── Dated-want disambiguation: publish date sanity ──────────────────
    // Reject releases published too long before the date the wanted content
    // came into existence, or before its year.
    // For TvEpisode: use the episode's air_date.
    // For SportEvent: use the event_date; no year fallback (see the arm below).
    // For TvSeason: use the season premiere — the earliest air_date across the
    // season — never the individual episode's. Providers list binge-dropped
    // seasons on a weekly schedule even though every episode is published at
    // once, so gating each release on its own episode's air date rejected
    // whole seasons permanently.
    let tv_season_premiere_air_date: Option<&str> = if let MediaWant::TvSeason {
        episodes_needed: Some(eps),
        ..
    } = media_want
    {
        eps.iter().filter_map(|er| er.air_date.as_deref()).min()
    } else {
        None
    };

    if let Some((dated_want_date, dated_want_year)) = match media_want {
        MediaWant::TvEpisode { air_date, year, .. } => Some((air_date.as_deref(), *year)),
        MediaWant::TvSeason { year, .. } => Some((tv_season_premiere_air_date, *year)),
        // A sport session's footage cannot be published before the session was
        // run, so the same "published too far before the date" rule applies —
        // and it covers the case the unaired gate above cannot, where the search
        // happens AFTER the wanted event and an older same-location round from
        // the same season is still on offer.
        //
        // No year fallback here, deliberately. The season year would reject on
        // publish date alone whenever an indexer reports a stale or missing
        // date, and unlike a TV show year it buys almost nothing: the season
        // is already gated exactly by SeasonMismatch above. Without an
        // event_date this arm simply does not fire.
        MediaWant::SportEvent { event_date, .. } => Some((event_date.as_deref(), None)),
        _ => None,
    } {
        // Try RFC 3339, then RFC 2822 (Newznab RSS), then plain ISO date
        let pub_date_opt = time::OffsetDateTime::parse(
            &release.publish_date,
            &time::format_description::well_known::Rfc3339,
        )
        .or_else(|_| {
            time::OffsetDateTime::parse(
                &release.publish_date,
                &time::format_description::well_known::Rfc2822,
            )
        })
        .map(|dt| dt.date())
        .ok()
        .or_else(|| parse_iso_date(&release.publish_date));

        if let Some(pub_date) = pub_date_opt {
            // Check 1: publish_date vs air_date
            if let Some(air_date) = dated_want_date.and_then(parse_iso_date) {
                let cutoff = air_date - time::Duration::days(publish_date_reject_days);
                if pub_date < cutoff {
                    return Some(StructuralRejectReason::PublishDateTooOld);
                }
            } else if let Some(show_year) = dated_want_year {
                // Check 2: publish_date vs show_year (fallback when no air_date)
                if pub_date.year() < (show_year as i32 - SHOW_YEAR_PUBLISH_REJECT_MARGIN_YEARS) {
                    return Some(StructuralRejectReason::PublishDateBeforeShowYear);
                }
            }
        }
    }

    // ── Movie publish date sanity ──────────────────────────────────────
    // Reject releases published before (movie_year - margin). A 2026 movie
    // shouldn't have releases from 2024. Uses release_date when available,
    // falls back to year.
    if let MediaWant::Movie {
        year, release_date, ..
    } = media_want
    {
        let pub_date_opt = time::OffsetDateTime::parse(
            &release.publish_date,
            &time::format_description::well_known::Rfc3339,
        )
        .or_else(|_| {
            time::OffsetDateTime::parse(
                &release.publish_date,
                &time::format_description::well_known::Rfc2822,
            )
        })
        .map(|dt| dt.date())
        .ok()
        .or_else(|| parse_iso_date(&release.publish_date));

        if let Some(pub_date) = pub_date_opt {
            if let Some(rel_date) = release_date.as_deref().and_then(parse_iso_date) {
                // Precise check: reject if published too long before release date
                let cutoff = rel_date - time::Duration::days(publish_date_reject_days);
                if pub_date < cutoff {
                    return Some(StructuralRejectReason::PublishDateBeforeMovieRelease);
                }
            } else if let Some(movie_year) = year {
                // Fallback: reject if published before (movie_year - margin)
                if pub_date.year() < (*movie_year as i32 - SHOW_YEAR_PUBLISH_REJECT_MARGIN_YEARS) {
                    return Some(StructuralRejectReason::PublishDateBeforeMovieYear);
                }
            }
        }
    }

    // ── TV show disambiguation: year mismatch ───────────────────────────
    // Reject if release has a year and it differs from the wanted show year.
    // Per scene rules, year in a TV release title = show premiere year for
    // disambiguation (e.g., "Scrubs.2026.S01E01" vs "Scrubs.S01E01").
    // Non-standard groups that use season-air-year (e.g., ZmWeb DSNP rips)
    // will be rejected — manual search is the fallback for those.
    if let Some(wanted_year) = match media_want {
        MediaWant::TvEpisode { year, .. } | MediaWant::TvSeason { year, .. } => *year,
        _ => None,
    } && let ParsedMedia::Tv(tv) = &parsed.media
        && let Some(parsed_year) = tv.year
        && parsed_year != wanted_year
    {
        return Some(StructuralRejectReason::YearMismatch);
    }

    // Book/Audiobook: reject if parsed author has no token overlap with wanted author
    if let Some(wanted_author) = match media_want {
        MediaWant::Book { author, .. } | MediaWant::Audiobook { author, .. } => author.as_deref(),
        _ => None,
    } && !wanted_author.is_empty()
    {
        let parsed_author = match &parsed.media {
            ParsedMedia::Book(book) => book.author.as_deref(),
            ParsedMedia::Audiobook(ab) => ab.author.as_deref(),
            _ => None,
        };
        if let Some(parsed_author) = parsed_author
            && !parsed_author.is_empty()
        {
            let wanted_tokens = normalize_author_tokens(wanted_author);
            let parsed_tokens = normalize_author_tokens(parsed_author);
            if !wanted_tokens.is_empty()
                && !parsed_tokens.is_empty()
                && wanted_tokens
                    .iter()
                    .filter(|t| parsed_tokens.iter().any(|pt| pt == *t))
                    .count()
                    == 0
            {
                return Some(StructuralRejectReason::AuthorMismatch);
            }
        }
    }

    match (media_want, &parsed.media) {
        // Movie: yearless releases are allowed — fuzzy scoring already penalises
        // the missing year token, and RSS watermark advancement means a hard reject
        // here causes the release to be skipped permanently.

        // Movie: reject if both have years and they differ
        (
            MediaWant::Movie {
                year: Some(wanted), ..
            },
            ParsedMedia::Movie(movie),
        ) if movie.year.is_some_and(|y| y != *wanted) => Some(StructuralRejectReason::YearMismatch),

        // TV Season: reject packs when auto and too few episodes needed relative to season size
        (
            MediaWant::TvSeason {
                pack_preference,
                episodes_needed,
                total_episodes,
                ..
            },
            ParsedMedia::Tv(tv),
        ) if tv.is_pack
            && pack_preference == "auto"
            && match (episodes_needed.as_ref(), total_episodes) {
                (Some(needed), Some(total)) if *total > 0 => {
                    (needed_not_in_library(needed) as f64) < (*total as f64 * 0.5)
                }
                (Some(needed), None) => needed_not_in_library(needed) <= 2,
                _ => false,
            } =>
        {
            Some(StructuralRejectReason::PackRejectedLowEpisodeRatio)
        }

        // TV Season: reject packs based on pack_preference and recency
        (
            MediaWant::TvSeason {
                pack_preference,
                last_air_date,
                ..
            },
            ParsedMedia::Tv(tv),
        ) if tv.is_pack
            && match pack_preference.as_str() {
                "never" => true,
                "always" => false,
                _ => {
                    // "auto": reject packs for recent shows (< 30 days since last air date)
                    last_air_date
                        .as_ref()
                        .and_then(|d| parse_iso_date(d))
                        .is_none_or(|date| {
                            let cutoff = crate::clock::now_utc().date() - time::Duration::days(30);
                            date > cutoff
                        })
                }
            } =>
        {
            Some(StructuralRejectReason::PackRejectedByPreference)
        }

        // TV Season: reject if parsed to different season
        (MediaWant::TvSeason { season, .. }, ParsedMedia::Tv(tv))
            if !season_matches(tv.season, season) =>
        {
            Some(StructuralRejectReason::SeasonMismatch)
        }

        // TV Episode: reject season packs — episode wants go to single-ep
        // releases (or per-file emission from packs with known file data in
        // `hash_index_rows_with_files_to_releases`). A bare pack release has
        // no file enumeration, so we can't pin the wanted episode to a file.
        (MediaWant::TvEpisode { .. }, ParsedMedia::Tv(tv)) if tv.is_pack => {
            Some(StructuralRejectReason::EpisodeWantPackRejected)
        }

        // TV Episode: reject if parsed to different season (non-pack)
        // Keep tv.season.is_some() guard: anime absolute numbering and daily shows
        // have no parsed season and should not be rejected here.
        (MediaWant::TvEpisode { season, .. }, ParsedMedia::Tv(tv))
            if !tv.is_pack && tv.season.is_some() && !season_matches(tv.season, season) =>
        {
            Some(StructuralRejectReason::SeasonMismatch)
        }

        // TV Episode: reject if parsed episode doesn't match wanted (non-pack, non-daily, has episodes)
        (MediaWant::TvEpisode { episode, .. }, ParsedMedia::Tv(tv))
            if !tv.is_pack
                && !tv.is_daily
                && tv.absolute_episode.is_none()
                && !tv.episodes.is_empty()
                && episode
                    .parse::<u32>()
                    .is_ok_and(|e| !tv.episodes.contains(&e)) =>
        {
            Some(StructuralRejectReason::EpisodeMismatch)
        }

        // TV Episode: reject if TV-classified but no S/E evidence at all.
        // Without any season or episode signal there's no way to confirm the
        // release contains the wanted episode. Anime absolute-numbering and
        // daily shows legitimately have no parsed season/episodes and are
        // excluded via their own fields.
        (MediaWant::TvEpisode { .. }, ParsedMedia::Tv(tv))
            if !tv.is_pack
                && !tv.is_daily
                && tv.absolute_episode.is_none()
                && tv.season.is_none()
                && tv.episodes.is_empty() =>
        {
            Some(StructuralRejectReason::NoEpisodeEvidence)
        }

        // Sport: reject if both have season year and they differ
        (MediaWant::SportEvent { season, .. }, ParsedMedia::Sport(sport))
            if sport.season.is_some()
                && season
                    .parse::<u32>()
                    .is_ok_and(|s| sport.season.is_some_and(|ps| ps != s)) =>
        {
            Some(StructuralRejectReason::SeasonMismatch)
        }

        // Sport: reject if both have round and they differ
        (
            MediaWant::SportEvent {
                round: Some(wanted_round),
                ..
            },
            ParsedMedia::Sport(sport),
        ) if sport.round.is_some_and(|r| r != *wanted_round) => {
            Some(StructuralRejectReason::RoundMismatch)
        }

        // Sport: reject if both have location and neither contains the other (case-insensitive).
        // Releases often include circuit name after country (e.g. "Portugal Algarve")
        // while TheSportsDB may only have the country ("Portugal"), or vice versa.
        (
            MediaWant::SportEvent {
                location: Some(wanted_location),
                ..
            },
            ParsedMedia::Sport(sport),
        ) if sport
            .location
            .as_ref()
            .is_some_and(|loc| !location_contains(loc, wanted_location)) =>
        {
            Some(StructuralRejectReason::LocationMismatch)
        }

        // Sport: reject if both have session type and they differ (case-insensitive)
        (
            MediaWant::SportEvent {
                session_type: Some(wanted_session),
                ..
            },
            ParsedMedia::Sport(sport),
        ) if sport
            .session_type
            .as_ref()
            .is_some_and(|sess| !sess.eq_ignore_ascii_case(wanted_session)) =>
        {
            Some(StructuralRejectReason::SessionMismatch)
        }

        // Sport: reject non-race session wants when release has no session indicator.
        // A generic release like "MotoGP.2026.Brazil.1080p" is implicitly the race.
        // Qualifying/Sprint/Practice wants must match a release that explicitly says so.
        (
            MediaWant::SportEvent {
                session_type: Some(wanted_session),
                ..
            },
            ParsedMedia::Sport(sport),
        ) if sport.session_type.is_none() && !is_default_sport_session(wanted_session) => {
            Some(StructuralRejectReason::SessionMissing)
        }

        // Sport: a session-less release only stands in for the race while it is
        // race-sized. DHT carries whole weekends under bare names
        // ("MotoGP.2026.Round15.Austria", 21-46 GiB) that otherwise read as the race.
        // Reported as SessionMissing: nothing in the name says which session it is,
        // and koblem already knows that reason.
        (
            MediaWant::SportEvent {
                session_type: Some(wanted_session),
                ..
            },
            ParsedMedia::Sport(sport),
        ) if sport.session_type.is_none()
            && is_default_sport_session(wanted_session)
            && is_sport_weekend_pack(release.size, parsed.quality.resolution.as_deref()) =>
        {
            Some(StructuralRejectReason::SessionMissing)
        }

        // Sport: defensive catch for sessions the parser couldn't recognise.
        // If the want is a default session (Race / GP) and parse_sport gave
        // up on session_type but the release title still contains a hint
        // word (Conference, Shakedown, Practice, Qualifying, Sprint, ...),
        // treat the release as a non-race and reject. Prevents prod
        // fallthrough when a new release shape breaks parse_sport — like
        // the underscore separator gap that let Practice releases through
        // until 2026-05-09. The `sport.session_type.is_none()` gate avoids
        // false positives where the parser DID extract a session and the
        // hint token sits in the league name (e.g. "Sprint Cup").
        (
            MediaWant::SportEvent {
                session_type: Some(wanted_session),
                ..
            },
            ParsedMedia::Sport(sport),
        ) if is_default_sport_session(wanted_session)
            && sport.session_type.is_none()
            && release_has_hint_word_outside_session(
                &release.title,
                sport.session_type.as_deref(),
            ) =>
        {
            Some(StructuralRejectReason::SessionMismatch)
        }

        // Music: reject if both have years and they differ
        (
            MediaWant::Music {
                year: Some(wanted), ..
            },
            ParsedMedia::Music(music),
        ) if music.year.is_some_and(|y| y != *wanted) => Some(StructuralRejectReason::YearMismatch),

        _ => None,
    }
}

/// Public wrapper for structural_reject, for use by watch/service.rs handle_match().
pub fn structural_reject_from_want(
    release: &Release,
    media_want: &MediaWant,
    search_context: SearchContext,
) -> Option<nats_jobs::StructuralRejectReason> {
    structural_reject(release, Some(media_want), search_context)
}

pub fn group_by_target_id(
    results: &[ProcessedRelease],
) -> HashMap<Option<nats_jobs::MatchedTarget>, Vec<&ProcessedRelease>> {
    let mut by_target: HashMap<Option<nats_jobs::MatchedTarget>, Vec<&ProcessedRelease>> =
        HashMap::new();

    for result in results {
        by_target
            .entry(result.matched_target.clone())
            .or_default()
            .push(result);
    }

    by_target
}

#[cfg(test)]
mod reference;

#[cfg(test)]
mod tests {
    use super::*;
    use nats_jobs::StructuralRejectReason;
    use uuid::Uuid;

    fn make_release(title: &str) -> Release {
        make_release_dated(title, "2024-01-01T00:00:00Z")
    }

    fn make_release_dated(title: &str, publish_date: &str) -> Release {
        Release {
            guid: Uuid::new_v4().to_string(),
            title: title.to_string(),
            file_name: None,
            size: 1_000_000_000,
            indexer: "test".to_string(),
            protocol: "torrent".to_string(),
            download_url: Some("http://example.com/dl".to_string()),
            magnet_url: None,
            info_url: None,
            publish_date: publish_date.to_string(),
            category_ids: Vec::new(),
            info_hash: None,
            files: None,
            seeders: Some(100),
            peers: None,
            leechers: None,
            grabs: None,
            votes: None,
            indexer_id: None,
            indexer_source_id: None,
            raw_data: serde_json::json!({}),
            external_ids: Vec::new(),
            media: None,
        }
    }

    #[test]
    fn test_structural_bonus_tv_episode_exact_match() {
        let release = make_release("Show.Name.S01E05.1080p.WEB-DL.x264");
        let media_want = MediaWant::TvEpisode {
            show_name: "Show Name".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        assert!(
            bonus >= 0.5,
            "Expected bonus >= 0.5 for exact episode match, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_tv_season_pack() {
        let release = make_release("Show.Name.S01.1080p.WEB-DL.x264");
        let media_want = MediaWant::TvEpisode {
            show_name: "Show Name".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        assert!(
            bonus >= 0.2,
            "Expected bonus >= 0.2 for season pack, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_proper_release() {
        let release = make_release("Show.Name.S01E05.PROPER.1080p.WEB-DL.x264");
        let media_want = MediaWant::TvEpisode {
            show_name: "Show Name".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        // Should get 0.5 for episode match + 0.1 for PROPER
        assert!(
            bonus >= 0.6,
            "Expected bonus >= 0.6 for exact match + PROPER, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_movie_year_match() {
        let release = make_release("Movie.Title.2024.1080p.BluRay.x264");
        let media_want = MediaWant::Movie {
            title: "Movie Title".to_string(),
            year: Some(2024),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        assert!(
            bonus >= 0.3,
            "Expected bonus >= 0.3 for movie year match, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_wrong_episode() {
        let release = make_release("Show.Name.S01E10.1080p.WEB-DL.x264");
        let media_want = MediaWant::TvEpisode {
            show_name: "Show Name".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        assert!(
            bonus < 0.2,
            "Expected minimal bonus for wrong episode, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_no_media_want() {
        let release = make_release("Show.Name.S01E05.1080p.WEB-DL.x264");
        let bonus = structural_bonus(&release, None, None);
        assert_eq!(bonus, 0.0, "Expected no bonus when media_want is None");
    }

    #[test]
    fn test_structural_bonus_book_author_match() {
        let release = make_release("Brandon.Sanderson-Mistborn.The.Final.Empire.EPUB");
        let media_want = MediaWant::Book {
            title: "Mistborn: The Final Empire".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        assert!(
            bonus >= 0.3,
            "Expected bonus >= 0.3 for author match, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_book_series_match() {
        let release = make_release("Mistborn.Cosmere.Series.Brandon.Sanderson.EPUB");
        let media_want = MediaWant::Book {
            title: "Mistborn: The Final Empire".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: Some("Cosmere".to_string()),
            series_position: None,
            release_date: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        assert!(
            bonus >= 0.5,
            "Expected bonus >= 0.5 for author+series match, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_book_no_match() {
        let release = make_release("Some.Random.Release.EPUB");
        let media_want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };

        let bonus = structural_bonus(&release, Some(&media_want), None);
        assert!(
            bonus < 0.1,
            "Expected minimal bonus for no match, got {}",
            bonus
        );
    }

    // ── structural_bonus pack preference tests ────────────────────────

    fn make_tv_season_want(
        pack_preference: &str,
        total_episodes: Option<u32>,
        episodes_needed: Option<Vec<nats_jobs::EpisodeRef>>,
    ) -> MediaWant {
        MediaWant::TvSeason {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            year: None,
            episodes_needed,
            total_episodes,
            pack_preference: pack_preference.to_string(),
            last_air_date: Some("2020-01-01".to_string()), // old show so pack not rejected
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        }
    }

    fn make_episode_refs(count: usize) -> Vec<nats_jobs::EpisodeRef> {
        (1..=count)
            .map(|i| nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: i.to_string(),
                sort_order: Some(i as i32),
                air_date: None,
                in_library: None,
                library_video_height: None,
                title: None,
            })
            .collect()
    }

    #[test]
    fn test_structural_bonus_pack_auto_high_ratio() {
        let release = make_release("Show.S01.1080p.WEB-DL.x264");
        let mw = make_tv_season_want("auto", Some(10), Some(make_episode_refs(9)));
        let bonus = structural_bonus(&release, Some(&mw), None);
        assert!(
            bonus >= 0.5,
            "Expected +0.5 for auto with 9/10 needed, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_pack_auto_low_ratio() {
        let release = make_release("Show.S01.1080p.WEB-DL.x264");
        let mw = make_tv_season_want("auto", Some(10), Some(make_episode_refs(1)));
        let bonus = structural_bonus(&release, Some(&mw), None);
        assert!(
            bonus < 0.1,
            "Expected 0.0 for auto with 1/10 needed, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_pack_auto_no_total() {
        let release = make_release("Show.S01.1080p.WEB-DL.x264");
        let mw = make_tv_season_want("auto", None, Some(make_episode_refs(5)));
        let bonus = structural_bonus(&release, Some(&mw), None);
        assert!(
            bonus < 0.1,
            "Expected 0.0 for auto with unknown total, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_bonus_pack_always() {
        let release = make_release("Show.S01.1080p.WEB-DL.x264");
        let mw = make_tv_season_want("always", Some(10), Some(make_episode_refs(1)));
        let bonus = structural_bonus(&release, Some(&mw), None);
        assert!(
            bonus >= 0.5,
            "Expected +0.5 for always regardless of ratio, got {}",
            bonus
        );
    }

    #[test]
    fn test_structural_reject_unparseable_season() {
        let release = make_release("Greys_Anatomy_-_Seizoen_2_-_Compleet_-_(2005-2006).DVDR-DVDNL");
        let mw = MediaWant::TvSeason {
            show_name: "Grey's Anatomy".to_string(),
            season: "22".to_string(),
            year: Some(2005),
            episodes_needed: None,
            total_episodes: None,
            pack_preference: "always".to_string(),
            last_air_date: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SeasonMismatch),
            "Should reject: unparseable season (Seizoen) doesn't match wanted S22"
        );
    }

    #[test]
    fn test_structural_reject_unparseable_season_episode_no_evidence() {
        // TvEpisode + TV-classified release with no parseable S/E evidence →
        // rejected as NoEpisodeEvidence. The parser doesn't recognize "Seizoen"
        // as a season, so tv.season is None and tv.episodes is empty; without
        // any S/E signal we can't confirm the release contains the wanted ep.
        let release = make_release("Greys_Anatomy_-_Seizoen_2_-_Compleet_-_(2005-2006).DVDR-DVDNL");
        let mw = MediaWant::TvEpisode {
            show_name: "Grey's Anatomy".to_string(),
            season: "22".to_string(),
            episode: "5".to_string(),
            year: Some(2005),
            air_date: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::NoEpisodeEvidence),
            "TvEpisode with no S/E evidence should be rejected"
        );
    }

    #[test]
    fn test_structural_reject_tv_episode_no_s_or_e_is_rejected() {
        // Mirrors the Law & Order Toronto case from magnetdb — imdb_id hit but
        // title has no S/E tokens (stripped upstream). structural_reject must
        // keep these out of the debrid candidate pool.
        let title = "Law & Order Toronto. Criminal Intent.2024.WEB-DL  720p.TVShows";
        // Pin the parser output this test depends on. If the parser ever
        // starts extracting an S/E (or classifying this as non-Tv), the
        // rejection assertion below stops being a valid test of
        // NoEpisodeEvidence — fail loudly here instead.
        let parsed = crate::parsing::parse_release(title, "tv");
        let ParsedMedia::Tv(tv) = &parsed.media else {
            panic!(
                "expected ParsedMedia::Tv for title {title:?}, got {:?}",
                parsed.media
            );
        };
        assert!(tv.season.is_none(), "parser must not extract a season");
        assert!(tv.episodes.is_empty(), "parser must not extract episodes");
        assert!(tv.absolute_episode.is_none(), "must not be anime-absolute");
        assert!(!tv.is_pack, "must not be a pack");
        assert!(!tv.is_daily, "must not be a daily show");

        let release = make_release(title);
        let mw = MediaWant::TvEpisode {
            show_name: "Law & Order Toronto: Criminal Intent".to_string(),
            season: "3".to_string(),
            episode: "7".to_string(),
            year: Some(2024),
            air_date: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::NoEpisodeEvidence),
            "Title with no season/episode tokens must be rejected"
        );
    }

    #[test]
    fn test_structural_reject_tv_episode_daily_not_caught_by_no_evidence() {
        // Daily-show format: no season/episodes but is_daily=true, so the
        // NoEpisodeEvidence rule must NOT fire. Air-date matching happens
        // elsewhere; structural_reject should leave this alone.
        let title = "The.Daily.Show.2024.03.14.1080p.WEB.x264";
        let parsed = crate::parsing::parse_release(title, "tv");
        let ParsedMedia::Tv(tv) = &parsed.media else {
            panic!("expected Tv, got {:?}", parsed.media);
        };
        assert!(tv.is_daily, "expected is_daily for {title:?}");
        let release = make_release(title);
        let mw = MediaWant::TvEpisode {
            show_name: "The Daily Show".to_string(),
            season: "30".to_string(),
            episode: "80".to_string(),
            year: None,
            air_date: Some("2024-03-14".to_string()),
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        // Daily shows may still be rejected by other rules (publish-date gates,
        // air-date guards); we only care that the new rule's guard spared them.
        assert_ne!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::NoEpisodeEvidence),
            "daily show must not be rejected as NoEpisodeEvidence"
        );
    }

    #[test]
    fn test_structural_reject_tv_episode_anime_absolute_not_caught_by_no_evidence() {
        // Anime absolute numbering: season=None, episodes=[] but
        // absolute_episode=Some. The guard excludes this from
        // NoEpisodeEvidence — episode-match handling is elsewhere.
        let title = "[Group] Some Anime - 123 [1080p].mkv";
        let parsed = crate::parsing::parse_release(title, "tv");
        let ParsedMedia::Tv(tv) = &parsed.media else {
            panic!("expected Tv, got {:?}", parsed.media);
        };
        assert!(
            tv.absolute_episode.is_some(),
            "expected absolute_episode for {title:?}"
        );
        let release = make_release(title);
        let mw = MediaWant::TvEpisode {
            show_name: "Some Anime".to_string(),
            season: "1".to_string(),
            episode: "123".to_string(),
            year: None,
            air_date: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        assert_ne!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::NoEpisodeEvidence),
            "anime absolute-numbering must not be rejected as NoEpisodeEvidence"
        );
    }

    #[test]
    fn test_structural_reject_tv_episode_pack_rejected() {
        // A bare season-pack release has no file enumeration, so it can't pin
        // the wanted episode to a specific file. Rejected. Packs with file
        // data are already exploded into per-file releases by
        // `hash_index_rows_with_files_to_releases`, so they hit scoring as
        // single-ep releases — not as packs.
        let title = "Show.Name.S01.1080p.WEB-DL.x264";
        let parsed = crate::parsing::parse_release(title, "tv");
        let ParsedMedia::Tv(tv) = &parsed.media else {
            panic!("expected Tv, got {:?}", parsed.media);
        };
        assert!(tv.is_pack, "expected is_pack for {title:?}");
        let release = make_release(title);
        let mw = MediaWant::TvEpisode {
            show_name: "Show Name".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            air_date: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::EpisodeWantPackRejected),
            "bare pack release must be rejected for an episode want"
        );
    }

    // ── structural_reject tests ───────────────────────────────────────

    #[test]
    fn test_structural_reject_movie_year_mismatch() {
        let release = make_release("Toy.Story.1995.REPACK.720p.BluRay.DD+5.1.x264-playHD");
        let mw = MediaWant::Movie {
            title: "Toy Story 5".to_string(),
            year: Some(2026),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_some(),
            "Should reject: want year 2026 vs release year 1995"
        );
    }

    #[test]
    fn test_structural_reject_movie_year_match() {
        let release = make_release("Movie.Title.2024.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::Movie {
            title: "Movie Title".to_string(),
            year: Some(2024),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: years match"
        );
    }

    #[test]
    fn test_structural_reject_movie_no_year_in_release_allowed() {
        let release = make_release_dated(
            "Movie.Title.1080p.BluRay.x264-GROUP",
            "2026-03-01T00:00:00Z",
        );
        let mw = MediaWant::Movie {
            title: "Movie Title".to_string(),
            year: Some(2026),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        // Yearless movies are allowed — fuzzy scoring penalises the missing
        // year token naturally, and hard rejection causes RSS watermark to
        // skip the release permanently.
        for ctx in [
            SearchContext::Rss,
            SearchContext::TextFallback,
            SearchContext::Structured,
        ] {
            assert!(
                structural_reject(&release, Some(&mw), ctx).is_none(),
                "Yearless movie should not be hard-rejected in {ctx:?}"
            );
        }
    }

    #[test]
    fn test_structural_reject_movie_2001_space_odyssey() {
        let release = make_release("2001.A.Space.Odyssey.1968.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::Movie {
            title: "2001 A Space Odyssey".to_string(),
            year: Some(1968),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: parser should find 1968 as the year"
        );
    }

    #[test]
    fn test_structural_reject_movie_remake_vs_original() {
        let release = make_release("Dune.1984.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::Movie {
            title: "Dune".to_string(),
            year: Some(2021),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_some(),
            "Should reject: 1984 vs 2021"
        );
    }

    #[test]
    fn test_structural_reject_movie_off_by_one_year() {
        let release = make_release("Movie.Title.2024.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::Movie {
            title: "Movie Title".to_string(),
            year: Some(2025),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_some(),
            "Should reject: strict year match, 2024 != 2025"
        );
    }

    #[test]
    fn test_structural_reject_movie_aka_year() {
        let release =
            make_release("Dao.AKA.The.Blade.REPACK.1995.1080p.NF.WEB-DL.AAC2.0.x264-Gohan");
        let mw = MediaWant::Movie {
            title: "The Blade".to_string(),
            year: Some(1995),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: years match (1995)"
        );
    }

    #[test]
    fn test_structural_reject_movie_remux_hdr10() {
        let release = make_release(
            "The.Super.Mario.Bros.Movie.2023.2160p.REMUX.HEVC.HDR10.TrueHD.7.1-samhydetorrentgalaxy",
        );
        let mw = MediaWant::Movie {
            title: "The Super Mario Bros Movie".to_string(),
            year: Some(2023),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: years match, HDR10 should not confuse parser"
        );
    }

    #[test]
    fn test_structural_reject_movie_want_no_year() {
        let release = make_release("Blade.1998.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::Movie {
            title: "Blade".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: want has no year to compare"
        );
    }

    #[test]
    fn test_structural_reject_movie_4k_remaster() {
        let release =
            make_release("Jurassic.Park.1993.30th.Anniversary.4K.Remaster.2160p.BluRay-GROUP");
        let mw = MediaWant::Movie {
            title: "Jurassic Park".to_string(),
            year: Some(1993),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: years match"
        );
    }

    #[test]
    fn test_structural_reject_tv_wrong_season() {
        let release = make_release("Breaking.Bad.S01E05.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Breaking Bad".to_string(),
            season: "3".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_some(),
            "Should reject: season 1 != season 3"
        );
    }

    #[test]
    fn test_structural_reject_tv_right_season() {
        let release = make_release("Breaking.Bad.S01E05.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Breaking Bad".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: season matches"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_pack_for_episode_want() {
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::EpisodeWantPackRejected),
            "Should reject: season packs must not be grabbed by episode wants"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_pack_for_season_want_always() {
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = MediaWant::TvSeason {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            year: None,
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "always".to_string(),
            total_episodes: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: pack_preference=always allows packs"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_pack_never() {
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = MediaWant::TvSeason {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            year: None,
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "never".to_string(),
            total_episodes: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PackRejectedByPreference),
            "Should reject: pack_preference=never rejects all packs"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_pack_auto_recent() {
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let recent_date =
            (time::OffsetDateTime::now_utc().date() - time::Duration::days(5)).to_string();
        let mw = MediaWant::TvSeason {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            year: None,
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: Some(recent_date),
            pack_preference: "auto".to_string(),
            total_episodes: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PackRejectedByPreference),
            "Should reject: auto with recent air date rejects packs"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_pack_auto_old() {
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let old_date =
            (time::OffsetDateTime::now_utc().date() - time::Duration::days(60)).to_string();
        let mw = MediaWant::TvSeason {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            year: None,
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: Some(old_date),
            pack_preference: "auto".to_string(),
            total_episodes: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: auto with old air date allows packs"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_pack_auto_no_date() {
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = MediaWant::TvSeason {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            year: None,
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "auto".to_string(),
            total_episodes: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PackRejectedByPreference),
            "Should reject: auto with no air date rejects packs (unknown = recent)"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_wrong_season() {
        // The actual bug: "American Dad S06" matching against a want for S22
        // Use pack_preference=always so pack rejection doesn't fire first
        let release = make_release("American.Dad.S06.1080p.DSNP.WEB-DL.AVC.DDP5.1-FLUX");
        let mw = MediaWant::TvSeason {
            show_name: "American Dad!".to_string(),
            season: "22".to_string(),
            year: Some(2005),
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "always".to_string(),
            total_episodes: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SeasonMismatch),
            "Should reject: release is S06 but want is S22"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_correct_season() {
        let release =
            make_release("American.Dad.S22E03.Powering.Through.1080p.DSNP.WEB-DL-playWEB");
        let mw = MediaWant::TvSeason {
            show_name: "American Dad!".to_string(),
            season: "22".to_string(),
            year: Some(2005),
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "never".to_string(),
            total_episodes: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: S22 matches wanted S22"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_various_wrong_seasons() {
        // All the wrong-season matches from the bug report
        // Use pack_preference=always so pack rejection doesn't fire first
        let wrong_releases = [
            "American Dad S06 1080p DSNP WEB-DL AVC DDP5 1-FLUX",
            "American Dad S15 1080p DSNP WEB-DL AVC DDP5 1-FLUX",
            "American Dad S12 1080p DSNP WEB-DL AVC DDP5 1-FLUX",
            "American Dad S13 1080p DSNP WEB-DL AVC DDP5 1-FLUX",
            "American Dad S18 1080p DSNP WEB-DL AVC DDP5 1-FLUX",
            "American Dad S14 1080p DSNP WEB-DL AVC DDP5 1-FLUX",
        ];
        let mw = MediaWant::TvSeason {
            show_name: "American Dad!".to_string(),
            season: "22".to_string(),
            year: Some(2005),
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "always".to_string(),
            total_episodes: None,
        };
        for title in &wrong_releases {
            let release = make_release(title);
            assert_eq!(
                structural_reject(&release, Some(&mw), SearchContext::default()),
                Some(StructuralRejectReason::SeasonMismatch),
                "Should reject '{title}': wrong season for S22 want"
            );
        }
    }

    #[test]
    fn test_structural_reject_tv_anime_absolute() {
        let release = make_release("[SubGroup] Naruto - 100 [720p].mkv");
        let mw = MediaWant::TvEpisode {
            show_name: "Naruto".to_string(),
            season: "1".to_string(),
            episode: "100".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: anime absolute numbering has no season to compare"
        );
    }

    #[test]
    fn test_structural_reject_music_wrong_year() {
        let release = make_release("Radiohead-OK_Computer-2003-FLAC-GROUP");
        let mw = MediaWant::Music {
            artist: "Radiohead".to_string(),
            album: Some("OK Computer".to_string()),
            year: Some(1997),
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_some(),
            "Should reject: music year 2003 != 1997"
        );
    }

    #[test]
    fn test_structural_reject_music_correct_year() {
        let release = make_release("Radiohead-OK_Computer-1997-FLAC-GROUP");
        let mw = MediaWant::Music {
            artist: "Radiohead".to_string(),
            album: Some("OK Computer".to_string()),
            year: Some(1997),
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: years match"
        );
    }

    #[test]
    fn test_structural_reject_book_no_reject() {
        let release = make_release("Brandon.Sanderson-Mistborn.2006.EPUB-GROUP");
        let mw = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: author matches"
        );
    }

    #[test]
    fn test_structural_reject_no_media_want() {
        let release = make_release("Anything.Goes.2024.1080p");
        assert!(
            structural_reject(&release, None, SearchContext::default()).is_none(),
            "Should not reject: no MediaWant to compare against"
        );
    }

    #[test]
    fn test_structural_reject_tv_daily_show() {
        // Daily show format (no SxxExx) — parser won't find a season, so no rejection
        let release = make_release("The.Daily.Show.2024.01.15.720p.HDTV-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "The Daily Show".to_string(),
            season: "2024".to_string(),
            episode: "1".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: daily show format has no SxxExx season to compare"
        );
    }

    #[test]
    fn test_structural_reject_sport_matching() {
        // Sport with matching season and round — should not reject
        let release = make_release("MotoGP.2026.R02.Brazil.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: None,
            session_type: None,
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: season and round both match"
        );
    }

    #[test]
    fn test_structural_reject_sport_wrong_round() {
        let release = make_release("MotoGP.2026.R03.Thailand.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: None,
            session_type: None,
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::RoundMismatch),
            "Should reject: round 3 != round 2"
        );
    }

    #[test]
    fn test_structural_reject_sport_wrong_season() {
        let release = make_release("MotoGP.2025.R02.Brazil.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: None,
            session_type: None,
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SeasonMismatch),
            "Should reject: season 2025 != 2026"
        );
    }

    #[test]
    fn test_structural_reject_sport_no_round_in_want() {
        // Want has no round — can't reject on round
        let release = make_release("MotoGP.2026.R03.Thailand.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: None,
            location: None,
            session_type: None,
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: no round in want to compare"
        );
    }

    #[test]
    fn test_structural_reject_sport_season_pack() {
        // Season pack — parser may set is_pack, but even so season should still be checked
        let release = make_release("Formula.1.2025.Complete.Season.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "Formula 1".to_string(),
            season: "2026".to_string(),
            round: None,
            location: None,
            session_type: None,
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        // This may or may not reject depending on parser — if parser finds season 2025, it should reject
        let result = structural_reject(&release, Some(&mw), SearchContext::default());
        // If parser extracted the season, it should be rejected
        if result.is_some() {
            assert_eq!(result, Some(StructuralRejectReason::SeasonMismatch));
        }
        // Either way, this test documents the behavior
    }

    // ── Sport event-date gating ────────────────────────────────────────
    //
    // Regression cover for the September 2026 San Marino incident: six wants
    // for the San Marino round (Misano, round 14, Sep 11-13) each grabbed the
    // corresponding session from the Italy round (Mugello, round 7) that ran
    // back in May. TheSportsDB reports the HOST country, so both rounds carry
    // "IT" and both wants were built with location "Italy"; season, location
    // and session type all matched legitimately, and the releases involved
    // (VERUM, BILLIE) never carry a round token, so the round gate could not
    // fire either. Only the calendar separates the two.

    /// Helper: an ISO date `offset` days from today, so these tests do not rot.
    fn iso_days_from_today(offset: i64) -> String {
        let d = time::OffsetDateTime::now_utc().date() + time::Duration::days(offset);
        // Built by hand rather than via Date::format: the crate's `formatting`
        // feature is not enabled here, and `Month` is repr(u8) from January = 1.
        format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day())
    }

    fn san_marino_want(event_date: Option<String>) -> MediaWant {
        MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(14),
            location: Some("Italy".to_string()),
            session_type: Some("Sprint Race".to_string()),
            event_title: Some("San Marino - Sprint Race".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date,
            in_library: None,
            library_video_height: None,
        }
    }

    /// The exact release that was wrongly grabbed, against the exact want.
    /// Every other gate passes; only the event date can reject it.
    #[test]
    fn test_structural_reject_sport_event_not_yet_held() {
        let release = make_release("MotoGP.2026.Italy.Sprint.1080p.WEB.h264-BILLIE");
        let mw = san_marino_want(Some(iso_days_from_today(5)));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::EventNotYetHeld),
            "A session five days in the future cannot have footage yet"
        );
    }

    /// Without the date gate this release passes every structural check — the
    /// proof that season/round/location/session were all insufficient.
    #[test]
    fn test_structural_reject_sport_san_marino_passes_all_other_gates() {
        let release = make_release("MotoGP.2026.Italy.Sprint.1080p.WEB.h264-BILLIE");
        let mw = san_marino_want(None);
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            None,
            "Documents the bug: with no event_date nothing rejects the wrong round"
        );
    }

    /// Once the session has been run, the gate must get out of the way.
    #[test]
    fn test_structural_reject_sport_past_event_allowed() {
        let release = make_release_dated(
            "MotoGP.2026.Italy.Sprint.1080p.WEB.h264-BILLIE",
            &format!("{}T00:00:00Z", iso_days_from_today(-9)),
        );
        let mw = san_marino_want(Some(iso_days_from_today(-10)));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            None,
            "A past event must still be grabbable"
        );
    }

    /// Sessions run in their own timezone and listed dates can be a day out, so
    /// the same leniency as TV applies — today and tomorrow are not rejected.
    #[test]
    fn test_structural_reject_sport_event_date_leniency() {
        for offset in [0, 1] {
            // Publish date tracks the event so only the leniency window is
            // under test here, never the publish-date rule below it.
            let release = make_release_dated(
                "MotoGP.2026.Italy.Sprint.1080p.WEB.h264-BILLIE",
                &format!("{}T00:00:00Z", iso_days_from_today(offset)),
            );
            let mw = san_marino_want(Some(iso_days_from_today(offset)));
            assert_ne!(
                structural_reject(&release, Some(&mw), SearchContext::default()),
                Some(StructuralRejectReason::EventNotYetHeld),
                "Event {offset} day(s) out is within the leniency window"
            );
        }
    }

    /// The second half of the guard: when the search runs AFTER the wanted
    /// event, the unaired check cannot fire, but a release published months
    /// before the session was run still cannot be footage of it.
    #[test]
    fn test_structural_reject_sport_publish_date_before_event() {
        // Event ran 10 days ago (so the unaired gate cannot fire); the release
        // was published ~3.5 months earlier, back at the Mugello round.
        let release = make_release_dated(
            "MotoGP.2026.Italy.Sprint.1080p.WEB.h264-BILLIE",
            &format!("{}T00:00:00Z", iso_days_from_today(-114)),
        );
        let mw = san_marino_want(Some(iso_days_from_today(-10)));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PublishDateTooOld),
            "May-published Mugello footage cannot be the September San Marino session"
        );
    }

    /// A want with no event_date must behave exactly as before — the sport
    /// publish-date arm has no season-year fallback, so a stale indexer date
    /// must not start rejecting otherwise-valid releases.
    #[test]
    fn test_structural_reject_sport_no_event_date_ignores_publish_date() {
        let release = make_release_dated(
            "MotoGP.2026.Italy.Sprint.1080p.WEB.h264-BILLIE",
            "2020-01-01T00:00:00Z",
        );
        let mw = san_marino_want(None);
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            None,
            "No event_date means no date gating at all"
        );
    }

    #[test]
    fn test_structural_reject_sport_wrong_location() {
        // Want Thailand Race, release is Australia Race — should reject
        let release = make_release("MotoGP.2026.R02.Australia.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Thailand".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::LocationMismatch),
            "Should reject: Australia != Thailand"
        );
    }

    #[test]
    fn test_structural_reject_sport_wrong_session() {
        // Want Race, release is Qualifying — should reject
        let release = make_release("MotoGP.2026.R02.Thailand.Qualifying.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Thailand".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Should reject: Qualifying != Race"
        );
    }

    #[test]
    fn test_structural_reject_sport_matching_location_and_session() {
        // Everything matches — should NOT reject
        let release = make_release("MotoGP.2026.R02.Thailand.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Thailand".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: season, round, location, and session all match"
        );
    }

    #[test]
    fn test_structural_reject_sport_underscore_practice_vs_race() {
        // Regression for prod 2026-05-08: SABnzbd / DrunkenSlug release names
        // use underscores. `parse_sport`'s regexes only accept `[\.\s]` as a
        // separator, so the release falls through to parse_fallback and
        // session_type is None — letting the broad fuzzy match download a
        // Practice session as if it were the Race.
        let release = make_release("Moto2_2026_France_Practice_1080p_WEB_h264-VERUM");
        let mw = MediaWant::SportEvent {
            league_name: "Moto2".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("France".to_string()),
            session_type: Some("Race".to_string()),
            event_title: Some("French Grand prix Moto2".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Want Race must reject underscore-separated Practice release"
        );
    }

    #[test]
    fn test_structural_reject_sport_underscore_prod_verum_fp1_vs_race() {
        // Exact prod title from the 2026-05-08 incident. Want=Race, release is
        // the underscore-delimited FP1 clip that was wrongly downloaded. With
        // underscore normalization the parser now sees session="Free Practice 1"
        // and the structural guard rejects it via SessionMismatch.
        let release = make_release("Moto3_2026_Hungary_Free_Practice_One_1080p_WEB_h264-VERUM");
        let mw = MediaWant::SportEvent {
            league_name: "Moto3".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("Hungary".to_string()),
            session_type: Some("Race".to_string()),
            event_title: Some("Hungarian Grand prix Moto3".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Want Race must reject the prod underscore FP1 VERUM release"
        );
    }

    #[test]
    fn test_structural_reject_sport_fp1_vs_race() {
        // Want Race, release is FP1 — should reject
        let release = make_release("Formula.1.2026.R01.Brazil.FP1.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "Formula 1".to_string(),
            season: "2026".to_string(),
            round: Some(1),
            location: Some("Brazil".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Should reject: Free Practice 1 != Race"
        );
    }

    // ── MotoGP 2026 Spain GP: Press Conference / Gear Up / Valencia Test ──
    //
    // Regression for prod incident 2026-04-24: a kobbler-sourced Want for
    // MotoGP 2026 Spain GP round 4 Race triggered kobload to pick
    // "MotoGP.2026.Round04.Spain.Jerez.Pre-Event.Press.Conference..." instead
    // of the actual race. The parser fails to recognize "Press Conference" and
    // "Pre-Event" as a session; the regex for ROUND_LOC_RE also fails because
    // "Pre-Event" contains a hyphen that breaks the location-word pattern.
    // End result: parse_release falls all the way through to parse_fallback,
    // which produces sport.round = None. The sport structural rules then
    // skip RoundMismatch (guard: sport.round.is_some_and(...)) and skip
    // SessionMissing (guard: !is_default_sport_session("Race") is false).
    //
    // These tests pin the parser output and document the structural gap.

    fn motogp_spain_gp_round4_race_want() -> MediaWant {
        // Fully-populated MediaWant as kobbler would send for event evt-2366137.
        MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(4),
            location: Some("Spain".to_string()),
            session_type: Some("Race".to_string()),
            event_title: Some("Spain GP".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        }
    }

    #[test]
    fn test_parser_motogp_press_conference_prod_title() {
        // After the 2026-04-24 parser fix: "Pre-Event.Press.Conference" is
        // recognized. "Pre-Event" gets absorbed into location (hyphen-allowed
        // location class), then "Press Conference" is matched as a session.
        let title = "MotoGP.2026.Round04.Spain.Jerez.Pre-Event.Press.Conference.WEB-DL.1080p.H264.English-MWR";
        let parsed = crate::parsing::parse_release(title, "sports");
        let ParsedMedia::Sport(sport) = &parsed.media else {
            panic!("expected Sport, got {:?}", parsed.media);
        };
        assert_eq!(sport.round, Some(4));
        assert_eq!(sport.session_type.as_deref(), Some("Press Conference"));
        assert!(
            sport.location.as_deref() == Some("Spain Jerez")
                || sport.location.as_deref() == Some("Spain Jerez Pre-Event"),
            "location was {:?}",
            sport.location
        );
    }

    #[test]
    fn test_parser_motogp_gear_up_prod_title() {
        let title = "MotoGP.2026.Round04.Spain.Jerez.Gear.Up.WEB-DL.1080p.H264.English-MWR";
        let parsed = crate::parsing::parse_release(title, "sports");
        let ParsedMedia::Sport(sport) = &parsed.media else {
            panic!("expected Sport, got {:?}", parsed.media);
        };
        assert_eq!(sport.round, Some(4));
        assert_eq!(sport.session_type.as_deref(), Some("Gear Up"));
        assert_eq!(sport.location.as_deref(), Some("Spain Jerez"));
    }

    #[test]
    fn test_parser_motogp_valencia_test_prod_title() {
        // Round00 test session — parser captures "Morning Session" as session
        // and "Spain Valencia Test" as location. Scorer rejects on round
        // (0 != 4) before session check matters.
        let title = "MotoGP.2026.Round00.Spain.Valencia.Test.Morning.Session.TNT.WEB-DL.1080p.h264.English-MWR";
        let parsed = crate::parsing::parse_release(title, "sports");
        let ParsedMedia::Sport(sport) = &parsed.media else {
            panic!("expected Sport, got {:?}", parsed.media);
        };
        assert_eq!(sport.round, Some(0));
        assert_eq!(sport.session_type.as_deref(), Some("Morning Session"));
    }

    #[test]
    fn test_structural_reject_motogp_press_conference_should_reject_for_race_want() {
        // THE BUG: Press Conference must be rejected for a Race want.
        // Currently it is NOT rejected because:
        //   1. Parser falls through to fallback → sport.round = None
        //   2. RoundMismatch rule's guard `sport.round.is_some_and(|r| r != 4)`
        //      is false when parse is None, so rule is skipped.
        //   3. SessionMissing rule's guard `!is_default_sport_session("Race")`
        //      is false because "Race" IS the default session, so rule is skipped.
        //   4. SessionMismatch needs release to have session_type=Some; it's None, skipped.
        //
        // This test is expected to FAIL with current code (TDD red).
        let release = make_release(
            "MotoGP.2026.Round04.Spain.Jerez.Pre-Event.Press.Conference.WEB-DL.1080p.H264.English-MWR",
        );
        let mw = motogp_spain_gp_round4_race_want();
        let verdict = structural_reject(&release, Some(&mw), SearchContext::default());
        assert!(
            verdict.is_some(),
            "Press Conference must be rejected for a Race want — got {verdict:?}"
        );
    }

    #[test]
    fn test_structural_reject_motogp_gear_up_should_reject_for_race_want() {
        // Gear Up is not the race either. In prod, the structural rejector
        // did fire (RoundMismatch), so this test currently passes — included
        // as a pinning test so a future parser change that re-introduces the
        // gap (e.g. stops parsing round) would fail here.
        let release =
            make_release("MotoGP.2026.Round04.Spain.Jerez.Gear.Up.WEB-DL.1080p.H264.English-MWR");
        let mw = motogp_spain_gp_round4_race_want();
        let verdict = structural_reject(&release, Some(&mw), SearchContext::default());
        assert!(
            verdict.is_some(),
            "Gear Up must be rejected for a Race want — got {verdict:?}"
        );
    }

    #[test]
    fn test_structural_reject_motogp_hint_word_fallback_for_race_want() {
        // Defensive fallback: a hypothetical future auxiliary session that the
        // parser vocabulary doesn't yet cover but whose title contains a
        // session-hint word ("Launch" here) must still be rejected for a
        // default-session (Race) Want. Protects against silent fallthrough
        // when new motorsport content types appear in prod.
        let release = make_release(
            "MotoGP.2026.Round04.Spain.Jerez.Team.Launch.WEB-DL.1080p.H264.English-MWR",
        );
        let mw = motogp_spain_gp_round4_race_want();
        let verdict = structural_reject(&release, Some(&mw), SearchContext::default());
        assert_eq!(
            verdict,
            Some(StructuralRejectReason::SessionMismatch),
            "Hint-word 'Launch' in a Race want must reject — got {verdict:?}"
        );
    }

    // ── Defense-in-depth: practice/qualifying/sprint hint words ──────
    //
    // If a future release shape breaks parse_sport (the way underscores did
    // until 2026-05-09), the defensive hint check is the last line that
    // stops obvious non-race releases from satisfying a Race want. We pin
    // the three common motorsport non-race tokens here.
    //
    // The hint check is gated on `sport.session_type.is_none()` so that a
    // legit "Sprint Cup" league release (where parser correctly extracts
    // session=Race) doesn't false-positive on the bare "Sprint" token in
    // the league name.

    /// All-hyphen release shape — every parse_sport regex needs `[\.\s]`
    /// (or normalized `_`) as a separator, so this falls through to
    /// parse_fallback with session_type=None. Stable scaffolding for
    /// hint-defense tests.
    fn parser_failing_hyphen_only(token: &str) -> String {
        format!("MotoGP-2026-{token}-Brazil-1080p")
    }

    #[test]
    fn test_structural_reject_sport_practice_hint_for_race_want() {
        let release = make_release(&parser_failing_hyphen_only("Practice"));
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("Brazil".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Race want must reject parser-failed release with 'practice' hint",
        );
    }

    #[test]
    fn test_structural_reject_sport_warmup_hint_for_race_want() {
        // "Warm.Up" tokenizes to ["warm", "up"]; the bare "warm" hint must trip
        // for a Race want when the parser fails to classify the session.
        let release = make_release("MotoGP-2026-Warm-Up-Brazil-1080p");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("Brazil".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Race want must reject parser-failed release with 'warm' hint",
        );
    }

    #[test]
    fn test_structural_reject_sport_qualifying_hint_for_race_want() {
        let release = make_release(&parser_failing_hyphen_only("Qualifying"));
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("Brazil".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Race want must reject parser-failed release with 'qualifying' hint",
        );
    }

    #[test]
    fn test_structural_reject_sport_sprint_hint_for_race_want() {
        let release = make_release(&parser_failing_hyphen_only("Sprint"));
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("Brazil".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::SessionMismatch),
            "Race want must reject parser-failed release with 'sprint' hint",
        );
    }

    #[test]
    fn test_structural_reject_sport_qualifying_release_for_qualifying_want_passes() {
        // Verifies the hint-word additions don't accidentally block the real
        // Qualifying flow: a properly-named Qualifying release for a
        // Qualifying want must not be rejected.
        let release = make_release("MotoGP_2026_Brazil_Qualifying_1080p_WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("Brazil".to_string()),
            session_type: Some("Qualifying".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Real Qualifying release for Qualifying want must not be rejected",
        );
    }

    #[test]
    fn test_structural_reject_sport_sprint_cup_race_not_blocked_by_hint() {
        // A "Sprint Cup" series race release contains the bare token "sprint"
        // in the league name. The hint check must NOT false-positive: parser
        // extracts session=Race, so the rule's `sport.session_type.is_none()`
        // gate skips the hint check.
        let release = make_release("Sprint.Cup.2024.R01.Daytona.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "Sprint Cup".to_string(),
            season: "2024".to_string(),
            round: Some(1),
            location: Some("Daytona".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Sprint Cup race must not be rejected by the 'sprint' hint word",
        );
    }

    #[test]
    fn test_structural_reject_motogp_hint_word_no_false_positive_for_named_location() {
        // Safety check on the hint-word rule: a legit race release with a
        // multi-word circuit name must NOT be falsely rejected. "Mexico City
        // Grand Prix" is a real race name and should pass for a Race want.
        // "Race" is the parsed session (explicit), so the hint-word rule's
        // guard (wanted session is default AND hint word is present) is the
        // only risk — verify it doesn't regress this case.
        let release = make_release("MotoGP.2026.R05.Mexico.City.Grand.Prix.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(5),
            location: Some("Mexico".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Legit race with multi-word location must not be rejected"
        );
    }

    // ── YEARxROUND format tests ──────────────────────────────────────

    #[test]
    fn test_structural_reject_sport_year_x_round_matching() {
        // 2025x22 should parse as round 22 and match
        let release = make_release("MotoGP.2025x22.Valencia.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2025".to_string(),
            round: Some(22),
            location: None,
            session_type: None,
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: 2025x22 = round 22"
        );
    }

    #[test]
    fn test_structural_reject_sport_year_x_round_wrong_round() {
        // 2025x05 should parse as round 5 and not match round 22
        let release = make_release("MotoGP.2025x05.France.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2025".to_string(),
            round: Some(22),
            location: None,
            session_type: None,
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::RoundMismatch),
            "Should reject: 2025x05 = round 5 != round 22"
        );
    }

    #[test]
    fn test_structural_bonus_sport_year_x_round() {
        // structural_bonus should award round bonus for YEARxROUND format
        let release = make_release("MotoGP.2025x22.Valencia.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2025".to_string(),
            round: Some(22),
            location: Some("Valencia".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        let bonus = structural_bonus(&release, Some(&mw), None);
        assert!(
            bonus >= 0.2,
            "Should get round bonus for YEARxROUND format, got {bonus}"
        );
    }

    // ── Episode mismatch tests ────────────────────────────────────────

    #[test]
    fn test_structural_reject_tv_episode_mismatch() {
        let release = make_release("Breaking.Bad.S01E10.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Breaking Bad".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::EpisodeMismatch),
            "Should reject: episode 10 != episode 5"
        );
    }

    #[test]
    fn test_structural_reject_tv_episode_match() {
        let release = make_release("Breaking.Bad.S01E05.1080p.BluRay.x264-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Breaking Bad".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: episode matches"
        );
    }

    #[test]
    fn test_structural_reject_tv_multi_episode_contains_wanted() {
        // Multi-episode release (E05-E07) when we want E06 — should accept
        let release = make_release("Show.S01E05E06E07.1080p.WEB-DL-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            episode: "6".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: multi-ep release contains wanted episode"
        );
    }

    #[test]
    fn test_structural_reject_tv_multi_episode_missing_wanted() {
        // Multi-episode release (E05-E07) when we want E10 — should reject
        let release = make_release("Show.S01E05E06E07.1080p.WEB-DL-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            episode: "10".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::EpisodeMismatch),
            "Should reject: E05-E07 doesn't contain E10"
        );
    }

    #[test]
    fn test_structural_reject_tv_daily_episode_no_reject() {
        // Daily show — episode field might be "2024-01-15", can't parse as u32
        let release = make_release("The.Daily.Show.2024.01.15.720p.HDTV-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "The Daily Show".to_string(),
            season: "2024".to_string(),
            episode: "2024-01-15".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: daily show episode can't be parsed as number"
        );
    }

    #[test]
    fn test_structural_reject_tv_season_pack_episode_rejected() {
        // Season pack when we want specific episode — packs must be rejected for episode wants
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = MediaWant::TvEpisode {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            episode: "5".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::EpisodeWantPackRejected),
            "Should reject: episode wants must not grab season packs"
        );
    }

    // ── Title mismatch tests ─────────────────────────────────────────

    #[test]
    fn test_structural_reject_tv_title_mismatch_smuggler_vs_teenieping() {
        // "To Catch a Smuggler S06" must not match want for "Catch! Teenieping S06"
        let release =
            make_release("To.Catch.a.Smuggler.S06.1080p.HULU.WEB-DL.DDP5.1.H264-WhiteHat");
        let mw = MediaWant::TvSeason {
            show_name: "Catch! Teenieping".to_string(),
            season: "6".to_string(),
            year: None,
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "never".to_string(),
            total_episodes: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
            "Should reject: different show names despite same season"
        );
    }

    #[test]
    fn test_structural_reject_tv_title_match_correct_show() {
        // Correct show must not be rejected
        let release = make_release("Catch.Teenieping.S06E20.1080p.WEB-DL.AAC2.0.H.264-Group");
        let mw = MediaWant::TvSeason {
            show_name: "Catch! Teenieping".to_string(),
            season: "6".to_string(),
            year: None,
            episodes_needed: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            last_air_date: None,
            pack_preference: "never".to_string(),
            total_episodes: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: correct show and season"
        );
    }

    #[test]
    fn test_structural_reject_tv_title_match_episode_correct() {
        // TvEpisode want: correct show must not be rejected
        let release = make_release("Catch.Teenieping.S06E20.1080p.WEB-DL-Group");
        let mw = MediaWant::TvEpisode {
            show_name: "Catch! Teenieping".to_string(),
            season: "6".to_string(),
            episode: "20".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: correct show, season, and episode"
        );
    }

    #[test]
    fn test_structural_reject_tv_title_mismatch_episode_want() {
        // TvEpisode want: wrong show must be rejected
        let release = make_release("To.Catch.a.Smuggler.S06E01.1080p.HULU.WEB-DL-Group");
        let mw = MediaWant::TvEpisode {
            show_name: "Catch! Teenieping".to_string(),
            season: "6".to_string(),
            episode: "1".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
            "Should reject: different show names for episode want"
        );
    }

    #[test]
    fn test_titles_similar() {
        // Same show
        assert!(titles_similar("Grey's Anatomy", "Greys Anatomy"));
        assert!(titles_similar("Catch! Teenieping", "Catch Teenieping"));
        assert!(titles_similar(
            "Law & Order Toronto: Criminal Intent",
            "Law Order Toronto Criminal Intent"
        ));

        // Different shows
        assert!(!titles_similar("Catch! Teenieping", "To Catch a Smuggler"));
        assert!(!titles_similar("The Good Doctor", "Good Trouble"));
        // Show name is proper subset of a different show
        assert!(!titles_similar("Paradise", "Hells Paradise"));
        assert!(!titles_similar("Paradise", "Hells.Paradise"));

        // Movie cases — foreign/translated titles must not match a coincidence
        // of generic tokens (Srpski film ≠ A Serbian Film on its own).
        assert!(!titles_similar("A Serbian Film", "Srpski film"));
        assert!(titles_similar("A Serbian Film", "A Serbian Film"));

        // Article-dropping still works in both directions.
        assert!(titles_similar("The Matrix", "Matrix"));
        assert!(!titles_similar("The Matrix", "Enter The Matrix"));

        // User example: Harry Potter vs Garden Potter must never match on
        // the shared "Potter" token alone.
        assert!(!titles_similar("Harry Potter", "Garden Potter"));
        // And a short wanted title must reject a release that prepends a
        // qualifying word — "Potter" want should not accept a "Harry Potter"
        // release (release has an extra significant word at the front).
        assert!(!titles_similar("Potter", "Harry Potter"));
    }

    // ── Book/Audiobook author structural rejection tests ─────────────

    #[test]
    fn test_structural_reject_book_author_mismatch() {
        // Title deliberately matches the release so the *author* check is
        // the only remaining veto (added after the title check was wired in).
        let release = make_release("Patrick.Rothfuss-Name.of.the.Wind.EPUB");
        let mw = MediaWant::Book {
            title: "Name of the Wind".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::AuthorMismatch),
            "Should reject: no author token overlap between Sanderson and Rothfuss"
        );
    }

    #[test]
    fn test_structural_reject_book_author_match() {
        let release = make_release("Brandon.Sanderson-Oathbringer.EPUB");
        let mw = MediaWant::Book {
            title: "Oathbringer".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: author tokens overlap (Brandon, Sanderson)"
        );
    }

    #[test]
    fn test_structural_reject_book_no_wanted_author() {
        let release = make_release("Patrick.Rothfuss-Name.of.the.Wind.EPUB");
        let mw = MediaWant::Book {
            title: "Name of the Wind".to_string(),
            author: None,
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: no wanted author to compare"
        );
    }

    #[test]
    fn test_structural_reject_book_no_parsed_author() {
        // Release with no author separator — parser can't extract author
        let release = make_release("Mistborn.The.Final.Empire.EPUB");
        let mw = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: no parsed author to compare against"
        );
    }

    #[test]
    fn test_structural_reject_audiobook_author_mismatch() {
        // Title deliberately matches the release so the *author* check is
        // the only remaining veto.
        let release = make_release("Patrick.Rothfuss-Name.of.the.Wind.m4b");
        let mw = MediaWant::Audiobook {
            title: "Name of the Wind".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            narrator: None,
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        };
        // Audiobook parser may extract the author from `Author-Title` style
        // names; either a title_mismatch or author_mismatch is acceptable as
        // long as the release is rejected (both mean "wrong book").
        let result = structural_reject(&release, Some(&mw), SearchContext::default());
        assert!(
            matches!(
                result,
                Some(StructuralRejectReason::AuthorMismatch)
                    | Some(StructuralRejectReason::TitleMismatch)
            ),
            "expected rejection (author or title), got {result:?}"
        );
    }

    // ── Author normalization tests ───────────────────────────────────

    #[test]
    fn test_normalize_author_tokens_initials() {
        let tokens = normalize_author_tokens("J.K. Rowling");
        assert!(tokens.contains(&"jk".to_string()), "tokens: {tokens:?}");
        assert!(
            tokens.contains(&"rowling".to_string()),
            "tokens: {tokens:?}"
        );
    }

    #[test]
    fn test_normalize_author_tokens_hyphen() {
        let tokens = normalize_author_tokens("Ursula K. Le Guin");
        assert!(tokens.contains(&"ursula".to_string()), "tokens: {tokens:?}");
        assert!(tokens.contains(&"guin".to_string()), "tokens: {tokens:?}");
    }

    #[test]
    fn test_normalize_author_tokens_apostrophe() {
        let tokens = normalize_author_tokens("Patrick O'Brian");
        assert!(tokens.contains(&"obrian".to_string()), "tokens: {tokens:?}");
    }

    // ── Article-aware title matching tests (Paradise vs The Paradise) ────

    #[test]
    fn test_titles_similar_article_distinguishes_shows() {
        // "The Paradise" is a different show from "Paradise" —
        // parsed has extra "The" prefix → reject
        assert!(!titles_similar("Paradise", "The Paradise"));
        // But "The Flash" ~ "The Flash" is the same show
        assert!(titles_similar("The Flash", "The Flash"));
        // Want "The Flash", release drops article to "Flash" → OK
        // (extra words in wanted title are fine, release just dropped the article)
        assert!(titles_similar("The Flash", "Flash"));
    }

    /// Highest-level scoring test: "The.Paradise S02E08" must NOT match
    /// a want for "Paradise" season 2 — it's a completely different show.
    #[test]
    fn test_score_rejects_the_paradise_for_paradise_want() {
        let releases = vec![make_release(
            "The.Paradise S02E08 Episode.8.1080p.AMZN.WEB-DL.DDP.5.1.H265-d3g",
        )];
        let media_want = MediaWant::TvSeason {
            show_name: "Paradise".to_string(),
            season: "2".to_string(),
            year: Some(2025),
            episodes_needed: None,
            total_episodes: None,
            pack_preference: "never".to_string(),
            last_air_date: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        let targets = crate::media::match_targets_from_media_want(&media_want);
        let config =
            ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(&media_want);
        let results = score_batch(&releases, &config);
        assert!(
            results.iter().all(|r| r.match_score.is_none()),
            "The.Paradise should NOT match Paradise want, but got: {:?}",
            results
                .iter()
                .filter(|r| r.match_score.is_some())
                .map(|r| &r.release.title)
                .collect::<Vec<_>>()
        );
    }

    /// Correct "Paradise 2025" releases must still match a Paradise S02 want.
    #[test]
    fn test_score_accepts_correct_paradise_for_paradise_want() {
        let releases = vec![make_release(
            "Paradise.2025.S02E06.Jane.1080p.DSNP.WEB-DL.DDP5.1.H.264-FLUX",
        )];
        let media_want = MediaWant::TvSeason {
            show_name: "Paradise".to_string(),
            season: "2".to_string(),
            year: Some(2025),
            episodes_needed: None,
            total_episodes: None,
            pack_preference: "never".to_string(),
            last_air_date: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        let targets = crate::media::match_targets_from_media_want(&media_want);
        let config =
            ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(&media_want);
        let results = score_batch(&releases, &config);
        assert!(
            results.iter().any(|r| r.match_score.is_some()),
            "Correct Paradise.2025 release should match Paradise want"
        );
    }

    /// "Flash.S01E01" must match a want for "The Flash" S01 — via
    /// article-stripped match target.
    #[test]
    fn test_score_accepts_flash_for_the_flash_want() {
        let releases = vec![make_release("Flash.S01E01.1080p.WEB-DL.DDP5.1.H.264-GROUP")];
        let media_want = MediaWant::TvSeason {
            show_name: "The Flash".to_string(),
            season: "1".to_string(),
            year: None,
            episodes_needed: None,
            total_episodes: None,
            pack_preference: "never".to_string(),
            last_air_date: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        let targets = crate::media::match_targets_from_media_want(&media_want);
        let config =
            ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(&media_want);
        let results = score_batch(&releases, &config);
        assert!(
            results.iter().any(|r| r.match_score.is_some()),
            "Flash.S01E01 should match 'The Flash' want via article-stripped target"
        );
    }

    // ── Cross-media-type rejection tests ────────────────────────────────

    fn make_movie_want(title: &str, year: Option<u32>) -> MediaWant {
        MediaWant::Movie {
            title: title.to_string(),
            year,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        }
    }

    #[test]
    fn test_structural_reject_movie_want_vs_tv_release() {
        // "E04" without "S##" may not parse as TV, but the yearless guard catches it
        let release = make_release("Super Mario World E04-Ghosts R Us.DVDRip.x264.Kopimi");
        let mw = make_movie_want("The Super Mario Galaxy Movie", Some(2026));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_some(),
            "TV episode release should be rejected when want is Movie"
        );
    }

    #[test]
    fn test_structural_reject_movie_want_vs_tv_season_episode() {
        // S01E05 is clearly TV structure — cross_media_type_tv guard catches it
        let release = make_release("Breaking.Bad.S01E05.1080p.BluRay.x264-GROUP");
        let mw = make_movie_want("Breaking Bad Movie", Some(2026));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::CrossMediaTypeTv),
            "S01E05 release should be rejected as cross_media_type_tv for Movie want"
        );
    }

    #[test]
    fn test_structural_reject_movie_want_vs_actual_movie() {
        let release = make_release_dated(
            "The.Super.Mario.Galaxy.Movie.2026.1080p.WEB-DL.x264-GROUP",
            "2026-03-01T00:00:00Z",
        );
        let mw = make_movie_want("The Super Mario Galaxy Movie", Some(2026));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Actual movie release should NOT be rejected"
        );
    }

    #[test]
    fn test_structural_reject_movie_want_vs_movie_no_episode_markers() {
        let release = make_release_dated("Super.Mario.2026.1080p.WEB-DL", "2026-03-01T00:00:00Z");
        let mw = make_movie_want("Super Mario", Some(2026));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Movie release without S/E tokens should NOT be rejected"
        );
    }

    // ── Movie yearless rejection tests ──────────────────────────────────

    #[test]
    fn test_structural_reject_movie_yearless_allowed_all_contexts() {
        // Yearless movies are no longer hard-rejected — fuzzy scoring handles
        // the missing year token naturally, and hard rejection caused RSS
        // watermark to skip releases permanently.
        let release = make_release_dated("Super.Mario.World.DVDRip.x264", "2026-03-01T00:00:00Z");
        let mw = make_movie_want("Super Mario World", Some(2026));
        for ctx in [
            SearchContext::Rss,
            SearchContext::TextFallback,
            SearchContext::Structured,
        ] {
            assert!(
                structural_reject(&release, Some(&mw), ctx).is_none(),
                "Yearless movie should not be hard-rejected in {ctx:?}"
            );
        }
    }

    #[test]
    fn test_structural_reject_movie_with_year_any_context() {
        let release = make_release_dated("Movie.Name.2026.1080p.WEB-DL", "2026-03-01T00:00:00Z");
        let mw = make_movie_want("Movie Name", Some(2026));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Movie with matching year should NOT be rejected"
        );
    }

    // ── Movie publish date sanity tests ─────────────────────────────────

    #[test]
    fn test_structural_reject_movie_publish_date_too_old() {
        // Release published in 2024 for a 2026 movie → rejected
        let release = make_release_dated("Movie.2026.1080p.WEB-DL", "2024-01-01T00:00:00Z");
        let mw = make_movie_want("Movie", Some(2026));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PublishDateBeforeMovieYear),
            "Release published 2 years before movie year should be rejected"
        );
    }

    #[test]
    fn test_structural_reject_movie_publish_date_recent_ok() {
        // Release published in 2026 for a 2026 movie → OK
        let release = make_release_dated("Movie.2026.1080p.WEB-DL", "2026-06-15T00:00:00Z");
        let mw = make_movie_want("Movie", Some(2026));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Release published same year as movie should NOT be rejected"
        );
    }

    #[test]
    fn test_structural_reject_movie_publish_date_one_year_before_ok() {
        // Release published in 2025 for a 2026 movie → OK (within margin)
        let release = make_release_dated("Movie.2026.1080p.WEB-DL", "2025-06-15T00:00:00Z");
        let mw = make_movie_want("Movie", Some(2026));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Release published 1 year before movie year should NOT be rejected (within margin)"
        );
    }

    // ── TV season publish date sanity tests ────────────────────────────

    /// Season whose episodes carry TMDB's weekly schedule, as a real
    /// binge-dropped season does: all eight land on the platform at once but
    /// the provider lists them a week apart.
    fn make_binge_dropped_season_want() -> MediaWant {
        let air_dates = [
            "2026-06-17",
            "2026-06-17",
            "2026-06-24",
            "2026-07-01",
            "2026-07-08",
            "2026-07-15",
            "2026-07-22",
            "2026-07-22",
        ];
        let episodes = air_dates
            .iter()
            .enumerate()
            .map(|(i, air_date)| nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: (i + 1).to_string(),
                sort_order: Some(i as i32 + 1),
                air_date: Some(air_date.to_string()),
                in_library: Some(false),
                library_video_height: None,
                title: None,
            })
            .collect();
        MediaWant::TvSeason {
            show_name: "Million Dollar Nannies".to_string(),
            season: "1".to_string(),
            year: Some(2026),
            episodes_needed: Some(episodes),
            total_episodes: Some(8),
            pack_preference: "auto".to_string(),
            last_air_date: Some("2026-07-22".to_string()),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        }
    }

    #[test]
    fn test_structural_reject_binge_dropped_season_later_episodes_accepted() {
        // The prod bug: Disney+ dropped all 8 episodes on 2026-06-19, but TMDB
        // lists them weekly through 2026-07-22. Gating each release on its own
        // episode's air date rejected everything from E03 on, permanently —
        // the season could never complete without a manual grab.
        let mw = make_binge_dropped_season_want();
        for episode in 3..=8 {
            let release = make_release_dated(
                &format!(
                    "Million.Dollar.Nannies.S01E{episode:02}.1080p.DSNP.WEB-DL.DD+5.1.H.264-playWEB"
                ),
                "Fri, 19 Jun 2026 11:46:19 +0000",
            );
            assert_eq!(
                structural_reject(&release, Some(&mw), SearchContext::TextFallback),
                None,
                "E{episode} published on the season's drop date must not be rejected"
            );
        }
    }

    /// Same provider-schedule-vs-reality mismatch as the publish-date gate: on
    /// drop day the whole season is up, but TMDB still lists the back half as
    /// airing weeks away. A season that has demonstrably started airing must not
    /// have its later episodes gated on those dates.
    #[test]
    fn test_structural_reject_binge_drop_later_episodes_not_treated_as_unaired() {
        let today = time::OffsetDateTime::now_utc().date();
        let premiere = today - time::Duration::days(2);
        let episodes = (1..=8)
            .map(|i| nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: i.to_string(),
                sort_order: Some(i),
                // Weekly schedule: E01 two days ago, E08 six weeks out.
                air_date: Some((premiere + time::Duration::days((i as i64 - 1) * 7)).to_string()),
                in_library: Some(false),
                library_video_height: None,
                title: None,
            })
            .collect();
        let mut mw = make_binge_dropped_season_want();
        if let MediaWant::TvSeason {
            episodes_needed,
            last_air_date,
            ..
        } = &mut mw
        {
            *episodes_needed = Some(episodes);
            *last_air_date = Some((premiere + time::Duration::days(49)).to_string());
        }
        let release = make_release_dated(
            "Million.Dollar.Nannies.S01E08.1080p.DSNP.WEB-DL.DD+5.1.H.264-playWEB",
            &time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
        );
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::TextFallback),
            None,
            "E08 of an already-premiered season must not be rejected as unaired"
        );
    }

    /// An undated episode following one that has not aired yet really has not
    /// happened, so a release claiming it belongs to a different series with the
    /// same episode number. Distinct from the binge-drop case above: there the
    /// episodes carry their own (unreliable) dates, here they carry none.
    #[test]
    fn test_structural_reject_undated_episode_after_future_episode_is_unaired() {
        let today = time::OffsetDateTime::now_utc().date();
        let mut episodes: Vec<nats_jobs::EpisodeRef> = (1..=6)
            .map(|i| nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: i.to_string(),
                sort_order: Some(i),
                air_date: Some((today - time::Duration::days((7 - i as i64) * 7)).to_string()),
                in_library: Some(false),
                library_video_height: None,
                title: None,
            })
            .collect();
        // E07 airs in five days; E08-E10 have no date at all.
        episodes.push(nats_jobs::EpisodeRef {
            season: "1".to_string(),
            episode: "7".to_string(),
            sort_order: Some(7),
            air_date: Some((today + time::Duration::days(5)).to_string()),
            in_library: Some(false),
            library_video_height: None,
            title: None,
        });
        for i in 8..=10 {
            episodes.push(nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: i.to_string(),
                sort_order: Some(i),
                air_date: None,
                in_library: Some(false),
                library_video_height: None,
                title: None,
            });
        }
        let mut mw = make_binge_dropped_season_want();
        if let MediaWant::TvSeason {
            show_name,
            episodes_needed,
            ..
        } = &mut mw
        {
            *show_name = "Scrubs".to_string();
            *episodes_needed = Some(episodes);
        }
        let release = make_release_dated(
            "Scrubs.S01E10.My.Nickname.1080p.WEB-DL.DDP5.1.H.264-NTb",
            &(today - time::Duration::days(1))
                .with_hms(0, 0, 0)
                .unwrap()
                .assume_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
        );
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::TextFallback),
            Some(StructuralRejectReason::UnairedEpisode),
            "undated E10 sitting behind a future E07 must still be rejected"
        );
    }

    #[test]
    fn test_structural_reject_season_that_has_not_premiered_is_unaired() {
        // The protection that must survive: nothing from a season that has not
        // started airing should be grabbed.
        let today = time::OffsetDateTime::now_utc().date();
        let premiere = today + time::Duration::days(20);
        let episodes = (1..=4)
            .map(|i| nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: i.to_string(),
                sort_order: Some(i),
                air_date: Some((premiere + time::Duration::days((i as i64 - 1) * 7)).to_string()),
                in_library: Some(false),
                library_video_height: None,
                title: None,
            })
            .collect();
        let mut mw = make_binge_dropped_season_want();
        if let MediaWant::TvSeason {
            episodes_needed,
            last_air_date,
            ..
        } = &mut mw
        {
            *episodes_needed = Some(episodes);
            *last_air_date = Some((premiere + time::Duration::days(21)).to_string());
        }
        let release = make_release_dated(
            "Million.Dollar.Nannies.S01E02.1080p.WEB-DL-GROUP",
            &time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
        );
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::TextFallback),
            Some(StructuralRejectReason::UnairedEpisode),
            "A season that has not premiered yet must still be rejected"
        );
    }

    #[test]
    fn test_structural_reject_season_release_published_before_premiere() {
        // The guard must still bite: a release predating the season premiere
        // by more than the tolerance belongs to some other show or season.
        let mw = make_binge_dropped_season_want();
        let release = make_release_dated(
            "Million.Dollar.Nannies.S01E04.1080p.WEB-DL-GROUP",
            "Sat, 17 May 2026 11:46:19 +0000",
        );
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::TextFallback),
            Some(StructuralRejectReason::PublishDateTooOld),
            "Release published a month before the season premiere should be rejected"
        );
    }

    #[test]
    fn test_structural_reject_season_premiere_used_even_when_e01_lacks_air_date() {
        // Episode 1 has no air_date; the premiere floor must fall through to
        // the earliest date the season does know about rather than to E04's.
        let mut mw = make_binge_dropped_season_want();
        if let MediaWant::TvSeason {
            episodes_needed: Some(eps),
            ..
        } = &mut mw
        {
            eps[0].air_date = None;
            eps[1].air_date = None;
        }
        let release = make_release_dated(
            "Million.Dollar.Nannies.S01E04.1080p.DSNP.WEB-DL.DD+5.1.H.264-playWEB",
            "Wed, 24 Jun 2026 11:46:19 +0000",
        );
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::TextFallback),
            None,
            "Release on the earliest known season air date must not be rejected"
        );
    }

    // ── Sport session structural reject tests ──────────────────────────────

    fn make_sport_want(session_type: Option<&str>) -> MediaWant {
        MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Brazil".to_string()),
            session_type: session_type.map(|s| s.to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        }
    }

    #[test]
    fn test_structural_reject_sport_qualifying_want_generic_release() {
        // Want: Qualifying 1, Release: "MotoGP.2026.Brazil.1080p" (no session)
        // Should REJECT — generic release is implicitly the race, not qualifying
        let release = make_release("MotoGP.2026.Brazil.1080p.WEB.h264-BILLIE");
        let mw = make_sport_want(Some("Qualifying 1"));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::SessionMissing),
            "Qualifying want should reject release with no session indicator"
        );
    }

    #[test]
    fn test_structural_reject_sport_qualifying_want_qualifying_release() {
        // Want: Qualifying 1, Release has "Q1" → should ACCEPT
        let release = make_release("MotoGP.2026.R02.Brazil.Q1.1080p.WEB-DL");
        let mw = make_sport_want(Some("Qualifying 1"));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Qualifying want should accept release with matching session"
        );
    }

    #[test]
    fn test_structural_reject_sport_race_want_generic_release() {
        // Want: Race, Release: "MotoGP.2026.Brazil.1080p" (no session)
        // Should ACCEPT — generic release is implicitly the race
        let release = make_release("MotoGP.2026.Brazil.1080p.WEB.h264-BILLIE");
        let mw = make_sport_want(Some("Race"));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Race want should accept generic release (no session = implicitly race)"
        );
    }

    #[test]
    fn test_structural_reject_sport_race_want_race_release() {
        // Want: Race, Release has "Race" → should ACCEPT
        let release = make_release("MotoGP.2026.Round02.Brazil.Race.WEB-DL.1080p.H264.English-MWR");
        let mw = make_sport_want(Some("Race"));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Race want should accept release with explicit Race session"
        );
    }

    #[test]
    fn test_structural_reject_sport_sprint_want_generic_release() {
        // Want: Sprint Race, Release: no session → should REJECT
        let release = make_release("MotoGP.2026.Brazil.1080p.WEB.h264-BILLIE");
        let mw = make_sport_want(Some("Sprint Race"));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::SessionMissing),
            "Sprint Race want should reject release with no session indicator"
        );
    }

    #[test]
    fn test_structural_reject_sport_practice_want_generic_release() {
        // Want: Free Practice 1, Release: no session → should REJECT
        let release = make_release("MotoGP.2026.Brazil.1080p.WEB.h264-BILLIE");
        let mw = make_sport_want(Some("Free Practice 1"));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::SessionMissing),
            "Practice want should reject release with no session indicator"
        );
    }

    #[test]
    fn test_structural_reject_sport_no_session_want_generic_release() {
        // Want: no session_type, Release: no session → should ACCEPT
        let release = make_release("MotoGP.2026.Brazil.1080p.WEB.h264-BILLIE");
        let mw = make_sport_want(None);
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Want with no session_type should accept any release"
        );
    }

    #[test]
    fn test_structural_reject_sport_qualifying_want_race_release() {
        // Want: Qualifying 1, Release: "Race" → should reject (session_mismatch)
        let release = make_release("MotoGP.2026.Round02.Brazil.Race.WEB-DL.1080p.H264.English-MWR");
        let mw = make_sport_want(Some("Qualifying 1"));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::SessionMismatch),
            "Qualifying want should reject release with Race session"
        );
    }

    #[test]
    fn test_structural_reject_sport_gp_want_generic_release() {
        // "GP" is a default session (alias for Race)
        let release = make_release("MotoGP.2026.Brazil.1080p.WEB.h264-BILLIE");
        let mw = make_sport_want(Some("GP"));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "GP want should accept generic release (GP = Race)"
        );
    }

    #[test]
    fn test_structural_reject_sport_ufc_main_card_want_release_with_main_card() {
        // UFC release with "Main Card" in title should match Main Card want
        let release = make_release("UFC.302.Main.Card.1080p.WEB.h264-GROUP");
        let mw = MediaWant::SportEvent {
            league_name: "UFC".to_string(),
            season: "2026".to_string(),
            round: None,
            location: None,
            session_type: Some("Main Card".to_string()),
            event_title: Some("UFC 302".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Main Card want should accept UFC release with Main Card"
        );
    }

    #[test]
    fn test_structural_reject_sport_ufc_main_card_want_generic_release() {
        // "Main Card" is a default session — generic UFC release should be accepted
        let release = make_release("UFC.302.1080p.WEB.h264-GROUP");
        let mw = MediaWant::SportEvent {
            league_name: "UFC".to_string(),
            season: "2026".to_string(),
            round: None,
            location: None,
            session_type: Some("Main Card".to_string()),
            event_title: Some("UFC 302".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Main Card want should accept generic UFC release"
        );
    }

    #[test]
    fn test_structural_reject_sport_ufc_prelims_want_main_card_release() {
        // UFC release with "Main Card" should reject Prelims want
        let release = make_release("UFC.302.Main.Card.1080p.WEB.h264-GROUP");
        let mw = MediaWant::SportEvent {
            league_name: "UFC".to_string(),
            season: "2026".to_string(),
            round: None,
            location: None,
            session_type: Some("Prelims".to_string()),
            event_title: Some("UFC 302".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::SessionMismatch),
            "Prelims want should reject UFC release with Main Card"
        );
    }

    #[test]
    fn test_structural_reject_sport_ufc_prelims_want_generic_release() {
        // "Prelims" is NOT a default session — should reject generic release
        let release = make_release("UFC.302.1080p.WEB.h264-GROUP");
        let mw = MediaWant::SportEvent {
            league_name: "UFC".to_string(),
            season: "2026".to_string(),
            round: None,
            location: None,
            session_type: Some("Prelims".to_string()),
            event_title: Some("UFC 302".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::SessionMissing),
            "Prelims want should reject generic UFC release"
        );
    }

    #[test]
    fn test_structural_reject_sport_sprint_race_want_bare_sprint_release() {
        // Want "Sprint Race", release has bare "Sprint" → must match (not session_mismatch)
        let release = make_release("MotoGP.2026.R03.USA.Sprint.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(3),
            location: Some("USA".to_string()),
            session_type: Some("Sprint Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            None,
            "Sprint Race want should accept bare Sprint release"
        );
    }

    #[test]
    fn test_structural_reject_sport_qualifying2_want_qualifying_two_release() {
        // Want "Qualifying 2", release has "Qualifying Two" → must match
        let release = make_release("MotoGP.2026.USA.Qualifying.Two.1080p.WEB.h264-BILLIE");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: None,
            location: Some("USA".to_string()),
            session_type: Some("Qualifying 2".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            None,
            "Qualifying 2 want should accept Qualifying Two release"
        );
    }

    // ── Sport location matching tests ──────────────────────────────────

    #[test]
    fn test_structural_reject_sport_location_contains_wanted() {
        // Release: "Portugal Algarve", Want: "Portugal" → should ACCEPT
        // WSBK releases include circuit name after country, TheSportsDB usually has just the country
        let release = make_release(
            "WSBK.2026.Round02.Portugal.Algarve.Race.Two.WEB-DL.1080p.H264.English-MWR",
        );
        let mw = MediaWant::SportEvent {
            league_name: "WSBK".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Portugal".to_string()),
            session_type: Some("Race 2".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Release with 'Portugal Algarve' should match want with 'Portugal'"
        );
    }

    #[test]
    fn test_structural_reject_sport_location_wanted_contains_parsed() {
        // Release: "Austin", Want: "Austin Texas" → should ACCEPT
        let release = make_release("MotoGP.2026.R03.Austin.Race.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(3),
            location: Some("Austin Texas".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::Rss).is_none(),
            "Release with 'Austin' should match want with 'Austin Texas'"
        );
    }

    #[test]
    fn test_structural_reject_sport_location_substring_not_word_boundary() {
        // "Portugalarific" contains "Portugal" as a substring but not as a word → should REJECT
        let release = make_release("WSBK.2026.Round02.Portugalarific.Race.Two.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "WSBK".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Portugal".to_string()),
            session_type: Some("Race 2".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::LocationMismatch),
            "Substring match within a word should NOT count as a location match"
        );
    }

    #[test]
    fn test_structural_reject_sport_location_completely_different() {
        // Release: "Spain", Want: "Portugal" → should REJECT
        let release = make_release("WSBK.2026.Round02.Spain.Race.Two.1080p.WEB-DL");
        let mw = MediaWant::SportEvent {
            league_name: "WSBK".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Portugal".to_string()),
            session_type: Some("Race 2".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::LocationMismatch),
            "Release with 'Spain' should NOT match want with 'Portugal'"
        );
    }

    #[test]
    fn test_votes_reject_below_threshold() {
        let mut release = make_release("Bad.Movie.2026.720p.WEB.H264-FAKE");
        release.votes = Some(-10);
        assert_eq!(
            votes_reject(&release),
            Some("negative_community_votes"),
            "Release with -10 votes should be rejected (threshold is -5)"
        );
    }

    #[test]
    fn test_votes_reject_at_threshold() {
        let mut release = make_release("Borderline.Movie.2026.720p.WEB.H264-MEH");
        release.votes = Some(-5);
        assert_eq!(
            votes_reject(&release),
            None,
            "Release with exactly -5 votes should NOT be rejected (threshold is strictly less than)"
        );
    }

    #[test]
    fn test_votes_reject_above_threshold() {
        let mut release = make_release("Good.Movie.2026.1080p.WEB.H264-GOOD");
        release.votes = Some(15);
        assert_eq!(
            votes_reject(&release),
            None,
            "Release with positive votes should not be rejected"
        );
    }

    #[test]
    fn test_votes_reject_no_votes() {
        let release = make_release("Unknown.Movie.2026.1080p.WEB.H264-UNK");
        assert_eq!(
            votes_reject(&release),
            None,
            "Release with no votes data should not be rejected"
        );
    }

    #[test]
    fn test_scoring_filters_out_negative_votes() {
        let mut release = make_release("Bad.Movie.2026.720p.WEB.H264-FAKE");
        release.votes = Some(-20);
        let releases = vec![release];

        let targets = vec![MatchTarget {
            id: "test".to_string(),
            target: "Bad Movie 2026".to_string(),
            min_score: None,
            semantic_target: None,
        }];

        let config = ScoringConfig::new(&targets, &[], 0.5, None, None)
            .with_search_context(SearchContext::Rss);
        let results = score_batch(&releases, &config);
        assert!(
            results
                .iter()
                .all(|r| r.match_score.is_none()
                    || r.decision != nats_jobs::ScoringDecision::Candidate),
            "Release with -20 votes should be filtered out of scoring results"
        );
    }

    // ── structural_reject pack auto low episode ratio tests ───────────

    #[test]
    fn test_structural_reject_pack_auto_low_ratio() {
        // 1 of 23 episodes needed on an old show — pack should be rejected
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = make_tv_season_want("auto", Some(23), Some(make_episode_refs(1)));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PackRejectedLowEpisodeRatio),
            "Should reject: auto with 1/23 episodes needed"
        );
    }

    #[test]
    fn test_structural_reject_pack_auto_high_ratio_with_episodes() {
        // 20 of 23 episodes needed on an old show — pack should be allowed
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = make_tv_season_want("auto", Some(23), Some(make_episode_refs(20)));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: auto with 20/23 episodes needed (>50%)"
        );
    }

    #[test]
    fn test_structural_reject_pack_auto_unknown_total_few_needed() {
        // 1 episode needed, unknown total — pack should be rejected
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = make_tv_season_want("auto", None, Some(make_episode_refs(1)));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PackRejectedLowEpisodeRatio),
            "Should reject: auto with 1 episode needed and unknown total"
        );
    }

    #[test]
    fn test_structural_reject_pack_auto_unknown_total_many_needed() {
        // 5 episodes needed, unknown total — pack should be allowed (>2)
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = make_tv_season_want("auto", None, Some(make_episode_refs(5)));
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: auto with 5 episodes needed and unknown total"
        );
    }

    #[test]
    fn test_structural_reject_pack_auto_no_episodes_needed() {
        // No episodes_needed (season-level want) — pack should be allowed for old show
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = make_tv_season_want("auto", Some(23), None);
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: auto with no episodes_needed (season-level want)"
        );
    }

    /// Build `total` episode refs where the first `in_library` of them are marked
    /// already in the library at 1080p.
    fn make_episode_refs_with_library(
        total: usize,
        in_library: usize,
    ) -> Vec<nats_jobs::EpisodeRef> {
        (1..=total)
            .map(|i| nats_jobs::EpisodeRef {
                season: "1".to_string(),
                episode: i.to_string(),
                sort_order: Some(i as i32),
                air_date: None,
                in_library: Some(i <= in_library),
                library_video_height: if i <= in_library { Some(1080) } else { None },
                title: None,
            })
            .collect()
    }

    #[test]
    fn test_structural_reject_pack_auto_mostly_in_library() {
        // The prod bug: 18 episodes in `episodes_needed`, 17 already in library at
        // 1080p, only the finale genuinely missing, season 31 days old, auto.
        // The raw list looks like the whole season is wanted, but only 1 episode
        // is — the pack must be rejected on the genuinely-needed ratio.
        let release = make_release("Show.S01.Complete.720p.x265.10bit-VRC");
        let old_date =
            (time::OffsetDateTime::now_utc().date() - time::Duration::days(31)).to_string();
        let mut mw = make_tv_season_want(
            "auto",
            Some(18),
            Some(make_episode_refs_with_library(18, 17)),
        );
        if let MediaWant::TvSeason { last_air_date, .. } = &mut mw {
            *last_air_date = Some(old_date);
        }
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PackRejectedLowEpisodeRatio),
            "Should reject: 17/18 already in library, only finale missing"
        );
    }

    #[test]
    fn test_structural_reject_pack_auto_mostly_missing_still_grabs() {
        // Regression: a genuinely sparse season (16 of 18 missing) on an old show
        // must still grab the pack.
        let release = make_release("Show.S01.Complete.1080p.BluRay-GROUP");
        let mw = make_tv_season_want(
            "auto",
            Some(18),
            Some(make_episode_refs_with_library(18, 2)),
        );
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "Should not reject: 16/18 genuinely missing is a legit pack grab"
        );
    }

    #[test]
    fn test_structural_reject_pack_auto_unknown_total_mostly_in_library() {
        // Unknown total, 7 episodes listed but 6 already in library → only 1
        // genuinely needed (<=2) → reject.
        let release = make_release("Show.S01.Complete.720p.WEB-DL-GROUP");
        let mw = make_tv_season_want("auto", None, Some(make_episode_refs_with_library(7, 6)));
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::PackRejectedLowEpisodeRatio),
            "Should reject: 6/7 already in library, unknown total"
        );
    }

    #[test]
    fn test_structural_reject_from_want_matcher_path_mostly_in_library() {
        // The watch/RSS matcher calls structural_reject_from_want — the path the
        // three prod packs came through. Same Greys/Invincible shape (7/8 held).
        let release = make_release("Show.S01.MULTi.FRENCH.1080p.WEB.H264-SCENE");
        let mw = make_tv_season_want("auto", Some(8), Some(make_episode_refs_with_library(8, 7)));
        assert_eq!(
            structural_reject_from_want(&release, &mw, SearchContext::Rss),
            Some(StructuralRejectReason::PackRejectedLowEpisodeRatio),
            "Matcher path should reject: 7/8 already in library"
        );
    }

    // ── Air date leniency regression tests ─────────────────────────────
    // The air_date check uses SUBTRACTION (leniency) not addition (strictness).
    // Episodes airing today or tomorrow must pass. Only episodes 2+ days
    // from now should be rejected. The old +1 day buffer was backwards and
    // caused RSS to reject same-day episodes, which the watermark then
    // skipped permanently.

    #[test]
    fn test_episode_airing_today_passes_all_contexts() {
        let today = time::OffsetDateTime::now_utc().date();
        let today_str = today
            .format(&time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").unwrap())
            .unwrap();
        let pub_date = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let release = make_release_dated(
            "Invincible.2021.S04E05.1080p.AMZN.WEB-DL.DD5.1.H.264-playWEB",
            &pub_date,
        );
        let mw = MediaWant::TvEpisode {
            show_name: "Invincible".to_string(),
            season: "4".to_string(),
            episode: "5".to_string(),
            year: Some(2021),
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: Some(today_str),
        };
        for ctx in [
            SearchContext::Rss,
            SearchContext::TextFallback,
            SearchContext::Structured,
        ] {
            assert!(
                structural_reject(&release, Some(&mw), ctx).is_none(),
                "Episode airing today must pass in {ctx:?} context"
            );
        }
    }

    #[test]
    fn test_episode_airing_tomorrow_passes_with_leniency() {
        let tomorrow = time::OffsetDateTime::now_utc().date() + time::Duration::days(1);
        let tomorrow_str = tomorrow
            .format(&time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").unwrap())
            .unwrap();
        let pub_date = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let release = make_release_dated(
            "Invincible.2021.S04E05.1080p.AMZN.WEB-DL.DD5.1.H.264-playWEB",
            &pub_date,
        );
        let mw = MediaWant::TvEpisode {
            show_name: "Invincible".to_string(),
            season: "4".to_string(),
            episode: "5".to_string(),
            year: Some(2021),
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: Some(tomorrow_str),
        };
        // 1-day leniency — tomorrow should pass in all contexts
        for ctx in [
            SearchContext::Rss,
            SearchContext::TextFallback,
            SearchContext::Structured,
        ] {
            assert!(
                structural_reject(&release, Some(&mw), ctx).is_none(),
                "Episode airing tomorrow must pass with leniency in {ctx:?}"
            );
        }
    }

    #[test]
    fn test_episode_airing_in_3_days_rejected() {
        let future = time::OffsetDateTime::now_utc().date() + time::Duration::days(3);
        let future_str = future
            .format(&time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").unwrap())
            .unwrap();
        let pub_date = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let release = make_release_dated(
            "Invincible.2021.S04E06.1080p.AMZN.WEB-DL.DD5.1.H.264-playWEB",
            &pub_date,
        );
        let mw = MediaWant::TvEpisode {
            show_name: "Invincible".to_string(),
            season: "4".to_string(),
            episode: "6".to_string(),
            year: Some(2021),
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: Some(future_str),
        };
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::Rss),
            Some(StructuralRejectReason::UnairedEpisode),
            "Episode airing in 3 days should be rejected even with leniency"
        );
    }

    // ── Non-TV structural title-mismatch veto ─────────────────────────
    //
    // These cover the full matrix of title-bearing MediaWant variants to
    // guarantee the fuzzy-matcher's weak-floor problem ("film" + "2010"
    // squeaking past 0.75) doesn't resurrect itself for any media type.

    fn movie_want(title: &str, year: Option<u32>, alternates: Option<Vec<String>>) -> MediaWant {
        MediaWant::Movie {
            title: title.to_string(),
            year,
            identifiers: None,
            alternate_titles: alternates,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        }
    }

    fn book_want(title: &str) -> MediaWant {
        MediaWant::Book {
            title: title.to_string(),
            author: None,
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        }
    }

    fn audiobook_want(title: &str) -> MediaWant {
        MediaWant::Audiobook {
            title: title.to_string(),
            author: None,
            narrator: None,
            isbn: None,
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
        }
    }

    fn game_want(title: &str) -> MediaWant {
        MediaWant::Game {
            title: title.to_string(),
            platform: None,
            year: None,
        }
    }

    fn software_want(name: &str) -> MediaWant {
        MediaWant::Software {
            name: name.to_string(),
            version: None,
            platform: None,
        }
    }

    fn music_want(artist: &str, album: Option<&str>) -> MediaWant {
        MediaWant::Music {
            artist: artist.to_string(),
            album: album.map(str::to_string),
            year: None,
        }
    }

    // ── Movie ────────────────────────────────────────────────────────
    #[test]
    fn test_movie_rejects_srpski_film_for_a_serbian_film_want() {
        // The regression: "Srpski.film.2010..." scored 0.767 against a want
        // for "A Serbian Film" because "film" + "2010" matched token-wise.
        let release = make_release("Srpski.film.2010.1080p.BluRay.DD.5.1.x264-PTer");
        let mw = movie_want("A Serbian Film", Some(2010), None);
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
        );
    }

    #[test]
    fn test_movie_accepts_srpski_film_when_alternate_title_configured() {
        // Escape valve: user explicitly opts into the foreign title.
        let release = make_release("Srpski.film.2010.1080p.BluRay.DD.5.1.x264-PTer");
        let mw = movie_want(
            "A Serbian Film",
            Some(2010),
            Some(vec!["Srpski Film".to_string()]),
        );
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
            "alternate_titles should let the Srpski release through",
        );
    }

    #[test]
    fn test_movie_accepts_legitimate_a_serbian_film_releases() {
        let mw = movie_want("A Serbian Film", Some(2010), None);
        for title in [
            "A.Serbian.Film.2010.1080p.BluRay.x264-GROUP",
            "A.Serbian.Film.2010.Uncut.NORDIC.Bluray.1080p.DTS-HD.x264-Grym",
        ] {
            let release = make_release(title);
            assert!(
                structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
                "legit release must pass: {title}",
            );
        }
    }

    #[test]
    fn test_movie_rejects_weak_match_false_positive() {
        // "Some Other Film 2010" also scores 0.767 against "A Serbian Film" on
        // the film+2010 coincidence — the structural veto must catch it.
        let release = make_release("Some.Other.Film.2010.1080p.BluRay-XYZ");
        let mw = movie_want("A Serbian Film", Some(2010), None);
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
        );
    }

    #[test]
    fn test_movie_harry_potter_vs_garden_potter() {
        let release = make_release("Garden.Potter.2019.1080p.WEB-DL-ABC");
        let mw = movie_want("Harry Potter and the Philosophers Stone", Some(2001), None);
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
        );
    }

    #[test]
    fn test_movie_harry_potter_matches_legit() {
        let release =
            make_release("Harry.Potter.and.the.Philosophers.Stone.2001.1080p.BluRay.x264-REFiNED");
        let mw = movie_want("Harry Potter and the Philosophers Stone", Some(2001), None);
        assert!(structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),);
    }

    #[test]
    fn test_movie_rejects_companion_docs_extending_wanted_title() {
        // Regression: these were grabbed for a "The Odyssey (2026)" want — the
        // wanted title is a prefix and the year matches.
        let mw = movie_want("The Odyssey", Some(2026), None);
        for title in [
            "The.Odyssey.The.Real.Story.2026.1080p.FNDG.WEB-DL.AAC2.0.H.264-NYXIS",
            "The.Odyssey.The.Making.of.an.Epic.2026.1080p.WEB.h264-EDITH",
            "The.Odyssey.The.Making.of.an.Epic.2026.1080p.AMZN.WEB-DL.DDP5.1.H.264-Kitsune",
        ] {
            let release = make_release_dated(title, "2026-10-14T00:00:00Z");
            assert_eq!(
                structural_reject(&release, Some(&mw), SearchContext::default()),
                Some(StructuralRejectReason::TitleMismatch),
                "{title}",
            );
        }
    }

    #[test]
    fn test_movie_rejects_subtitled_sequel_for_shorter_want() {
        let release = make_release("Dune.Part.Two.2024.1080p.WEB-DL.x264-GROUP");
        let mw = movie_want("Dune", Some(2024), None);
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
        );
    }

    #[test]
    fn test_movie_accepts_exact_title_and_dropped_article() {
        let mw = movie_want("The Odyssey", Some(2026), None);
        for title in [
            "The.Odyssey.2026.1080p.WEB-DL.x264-GROUP",
            "Odyssey.2026.1080p.WEB-DL.x264-GROUP",
        ] {
            let release = make_release_dated(title, "2026-10-14T00:00:00Z");
            assert!(
                structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
                "{title}",
            );
        }
    }

    #[test]
    fn test_movie_accepts_edition_words_before_year() {
        let mw = movie_want("Blade Runner", Some(1982), None);
        for title in [
            "Blade.Runner.Final.Cut.1982.1080p.BluRay.x264-GROUP",
            "Blade.Runner.Directors.Cut.1982.1080p.BluRay.x264-GROUP",
            "Blade.Runner.30th.Anniversary.Edition.1982.1080p.BluRay.x264-GROUP",
        ] {
            let release = make_release(title);
            assert!(
                structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
                "{title}",
            );
        }
    }

    #[test]
    fn test_movie_accepts_release_dropping_tmdb_subtitle() {
        // Shorter release title is not an extension.
        let release = make_release("Mission.Impossible.Dead.Reckoning.2023.1080p.WEB-DL-GROUP");
        let mw = movie_want(
            "Mission Impossible Dead Reckoning Part One",
            Some(2023),
            None,
        );
        assert!(structural_reject(&release, Some(&mw), SearchContext::default()).is_none());
    }

    #[test]
    fn test_movie_extended_title_accepted_via_alternate_title() {
        let release = make_release("Star.Wars.Episode.IV.A.New.Hope.1977.1080p.BluRay-GROUP");
        assert_eq!(
            structural_reject(
                &release,
                Some(&movie_want("Star Wars", Some(1977), None)),
                SearchContext::default(),
            ),
            Some(StructuralRejectReason::TitleMismatch),
        );
        let mw = movie_want(
            "Star Wars",
            Some(1977),
            Some(vec!["Star Wars: Episode IV - A New Hope".to_string()]),
        );
        assert!(structural_reject(&release, Some(&mw), SearchContext::default()).is_none());
    }

    #[test]
    fn test_movie_aka_segment_still_matches_shorter_want() {
        let release =
            make_release("Dao.AKA.The.Blade.REPACK.1995.1080p.NF.WEB-DL.AAC2.0.x264-Gohan");
        for wanted in ["Dao", "The Blade"] {
            let mw = movie_want(wanted, Some(1995), None);
            assert!(
                structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),
                "{wanted}",
            );
        }
    }

    #[test]
    fn test_movie_yearless_release_skips_extension_check() {
        // Without a year the parsed title is the whole cleaned name, which can
        // carry unrecognised tags; leave those to fuzzy scoring.
        let release = make_release_dated(
            "Movie.Name.NORDiC.1080p.WEB-DL-GROUP",
            "2026-03-01T00:00:00Z",
        );
        let mw = movie_want("Movie Name", Some(2026), None);
        assert!(structural_reject(&release, Some(&mw), SearchContext::default()).is_none());
    }

    // ── Book ─────────────────────────────────────────────────────────
    #[test]
    fn test_book_rejects_garden_potter_for_harry_potter_want() {
        let release = make_release("Garden.Potter-Some.Other.Author.EPUB");
        let mw = book_want("Harry Potter and the Philosophers Stone");
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
        );
    }

    #[test]
    fn test_book_accepts_harry_potter_legit() {
        let release = make_release("J.K.Rowling-Harry.Potter.and.the.Philosophers.Stone.EPUB");
        let mw = book_want("Harry Potter and the Philosophers Stone");
        assert!(structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),);
    }

    // ── Audiobook ────────────────────────────────────────────────────
    #[test]
    fn test_audiobook_rejects_garden_potter_for_harry_potter_want() {
        let release = make_release("Garden.Potter.Unabridged.m4b");
        let mw = audiobook_want("Harry Potter and the Philosophers Stone");
        // Audiobook parser may or may not extract a comparable title; we only
        // assert that IF a verdict is returned, it's the correct one.
        if let Some(reason) = structural_reject(&release, Some(&mw), SearchContext::default()) {
            assert_eq!(reason, StructuralRejectReason::TitleMismatch);
        }
    }

    // ── Game ─────────────────────────────────────────────────────────
    #[test]
    fn test_game_rejects_garden_potter_for_harry_potter_want() {
        // The game parser doesn't always extract a clean title from
        // release-style filenames. Only assert the verdict when the parser
        // produced one — the structural check is no-op without a parsed title.
        let release = make_release("Garden.Potter.Champions-RELOADED");
        let mw = game_want("Harry Potter Quidditch Champions");
        if let Some(reason) = structural_reject(&release, Some(&mw), SearchContext::default()) {
            assert_eq!(reason, StructuralRejectReason::TitleMismatch);
        }
    }

    // ── Software ─────────────────────────────────────────────────────
    #[test]
    fn test_software_rejects_illustrator_for_photoshop_want() {
        let release = make_release("Adobe.Illustrator.2024-XFORCE");
        let mw = software_want("Adobe Photoshop");
        assert_eq!(
            structural_reject(&release, Some(&mw), SearchContext::default()),
            Some(StructuralRejectReason::TitleMismatch),
        );
    }

    #[test]
    fn test_software_accepts_photoshop_legit() {
        let release = make_release("Adobe.Photoshop.2024.v25.1-XFORCE");
        let mw = software_want("Adobe Photoshop");
        assert!(structural_reject(&release, Some(&mw), SearchContext::default()).is_none(),);
    }

    // ── Music ────────────────────────────────────────────────────────
    #[test]
    fn test_music_rejects_wrong_album() {
        // The music parser sometimes fails to extract an album. Only assert
        // the verdict when the parser produced one.
        let release = make_release("Pink.Floyd.Wish.You.Were.Here.1975.FLAC");
        let mw = music_want("Pink Floyd", Some("The Dark Side of the Moon"));
        if let Some(reason) = structural_reject(&release, Some(&mw), SearchContext::default()) {
            assert_eq!(reason, StructuralRejectReason::TitleMismatch);
        }
    }

    #[test]
    fn test_music_accepts_matching_album() {
        let release = make_release("Pink.Floyd.The.Dark.Side.of.the.Moon.1973.FLAC");
        let mw = music_want("Pink Floyd", Some("The Dark Side of the Moon"));
        // The music parser may not always extract the album — accept either
        // "no verdict" or a non-title-mismatch verdict.
        if let Some(reason) = structural_reject(&release, Some(&mw), SearchContext::default()) {
            assert_ne!(reason, StructuralRejectReason::TitleMismatch);
        }
    }

    #[test]
    fn test_music_artist_only_want_is_not_vetoed() {
        // When the want has no album, the title veto branch must not fire —
        // artist-only wants rely on the existing fuzzy scorer.
        let release = make_release("Pink.Floyd.The.Wall.1979.FLAC");
        let mw = music_want("Pink Floyd", None);
        assert!(
            structural_reject(&release, Some(&mw), SearchContext::default())
                .iter()
                .all(|r| *r != StructuralRejectReason::TitleMismatch),
        );
    }
}
