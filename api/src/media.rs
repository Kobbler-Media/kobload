//! Media-aware helpers for search term generation.

use std::collections::HashSet;

use nats_jobs::{MatchedTarget, MediaWant};
use unicode_normalization::UnicodeNormalization;

use crate::watch::MatchTarget;

/// Strip a leading article ("The", "A", "An") from a title.
/// Returns the remainder if an article was found, None otherwise.
fn strip_leading_article(title: &str) -> Option<&str> {
    for article in &["the ", "a ", "an "] {
        if title.len() > article.len() && title[..article.len()].eq_ignore_ascii_case(article) {
            return Some(&title[article.len()..]);
        }
    }
    None
}

/// Strip diacritics from a string using Unicode NFD decomposition.
/// "España" → "Espana", "São Paulo" → "Sao Paulo"
fn strip_diacritics(s: &str) -> String {
    s.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect()
}

/// Noise words to strip from match targets for better fuzzy matching.
const NOISE_WORDS: &[&str] = &["grand", "prix", "championship", "world", "series"];

/// Strip noise words from a string for match target generation.
fn strip_noise_words(s: &str) -> String {
    s.split_whitespace()
        .filter(|w| !NOISE_WORDS.contains(&w.to_lowercase().as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Shorten a session type for search queries. Releases use shorter forms
/// than our canonical names, and shorter queries match more results.
/// Default sessions (Race, GP) return empty — most releases omit them.
fn search_session(session: &str) -> &str {
    match session {
        "Race" | "GP" | "Main Event" => "",
        "Sprint Race" | "Sprint Qualifying" => "Sprint",
        "Qualifying 1" | "Qualifying 2" | "Qualifying" => "Qualifying",
        "Free Practice 1" | "Free Practice 2" | "Free Practice 3" | "Free Practice 4"
        | "Free Practice" => "Practice",
        other => other,
    }
}

/// Map a MediaWant to its media_type_config key for DB lookups.
pub fn media_type_key_from_media_want(media_want: &MediaWant) -> &'static str {
    match media_want {
        MediaWant::Book { .. } => "ebooks",
        MediaWant::Audiobook { .. } => "audiobooks",
        MediaWant::TvEpisode { .. }
        | MediaWant::TvSeason { .. }
        | MediaWant::SportEvent { .. }
        | MediaWant::SportSeason { .. } => "tv",
        MediaWant::Movie { .. } => "movies",
        MediaWant::Comic { .. } => "comics",
        MediaWant::Music { .. } => "music",
        MediaWant::Game { .. } => "games",
        MediaWant::Software { .. } => "software",
    }
}

pub fn search_terms_from_media_want(media_want: &MediaWant) -> Vec<String> {
    let mut terms = Vec::new();

    match media_want {
        MediaWant::Book {
            title,
            author,
            series,
            ..
        }
        | MediaWant::Audiobook {
            title,
            author,
            series,
            ..
        } => {
            // Primary: "Author Title" — matches how Usenet releases are named.
            // ISBN search returns 0 results (releases aren't tagged with ISBNs).
            // Category filtering (cat=7000,7020,3000,3030) prevents TV/movie junk.
            if let Some(author) = author.as_ref().filter(|v| !v.trim().is_empty()) {
                terms.push(format!("{} {}", author.trim(), title.trim()));
            }
            // Series + title: catches releases organized by series name
            if let Some(series_name) = series.as_ref().filter(|v| !v.trim().is_empty()) {
                terms.push(format!("{} {}", series_name.trim(), title.trim()));
            }
            // Fallback: title only (usually skipped by early-exit scoring)
            if !title.trim().is_empty() {
                terms.push(title.trim().to_string());
            }
        }
        MediaWant::TvEpisode {
            show_name,
            season,
            episode,
            ..
        } => {
            // Structured search (season/ep params + IDs) is tried first.
            // These free-text terms are the fallback cascade.
            terms.extend(episode_search_terms(show_name, season, episode));
            // Article-stripped search terms (Sonarr strips leading articles from queries)
            if let Some(stripped) = strip_leading_article(show_name.trim()) {
                terms.extend(episode_search_terms(stripped, season, episode));
            }
        }
        MediaWant::TvSeason {
            show_name, season, ..
        } => {
            if let Ok(s_num) = season.parse::<u32>() {
                terms.push(format!("{} S{:02}", show_name.trim(), s_num));
            } else {
                terms.push(format!("{} {}", show_name.trim(), season));
            }
            terms.push(format!("{} Season {}", show_name.trim(), season));
            // Article-stripped search terms (Sonarr strips leading articles from queries)
            if let Some(stripped) = strip_leading_article(show_name.trim()) {
                if let Ok(s_num) = season.parse::<u32>() {
                    terms.push(format!("{stripped} S{s_num:02}"));
                }
                terms.push(format!("{stripped} Season {season}"));
            }
            // Note: year-only term (e.g. "The Rookie 2018") intentionally omitted —
            // too broad for season searches, matches all seasons and resembles movie queries.
        }
        MediaWant::Movie { title, year, .. } => {
            if let Some(year) = year.filter(|y| *y > 0) {
                terms.push(format!("{} {}", title.trim(), year));
            }
            if !title.trim().is_empty() {
                terms.push(title.trim().to_string());
            }
        }
        MediaWant::Comic {
            title,
            issue,
            series,
            year,
            ..
        } => {
            if let Some(issue) = issue {
                terms.push(format!("{} #{}", title.trim(), issue));
            }
            if let Some(series) = series.as_ref().filter(|v| !v.trim().is_empty()) {
                terms.push(format!("{} {}", series.trim(), title.trim()));
            }
            if let Some(year) = year {
                terms.push(format!("{} {}", title.trim(), year));
            }
            if !title.trim().is_empty() {
                terms.push(title.trim().to_string());
            }
        }
        MediaWant::Music {
            artist,
            album,
            year,
            ..
        } => {
            if let Some(album) = album.as_ref().filter(|v| !v.trim().is_empty()) {
                terms.push(format!("{} {}", artist.trim(), album.trim()));
                if let Some(year) = year {
                    terms.push(format!("{} {} {}", artist.trim(), album.trim(), year));
                }
            }
            if !artist.trim().is_empty() {
                terms.push(artist.trim().to_string());
            }
        }
        MediaWant::Game {
            title,
            platform,
            year,
            ..
        } => {
            if let Some(platform) = platform.as_ref().filter(|v| !v.trim().is_empty()) {
                terms.push(format!("{} {}", title.trim(), platform.trim()));
            }
            if let Some(year) = year {
                terms.push(format!("{} {}", title.trim(), year));
            }
            if !title.trim().is_empty() {
                terms.push(title.trim().to_string());
            }
        }
        MediaWant::Software {
            name,
            version,
            platform,
            ..
        } => {
            if let Some(version) = version.as_ref().filter(|v| !v.trim().is_empty()) {
                terms.push(format!("{} {}", name.trim(), version.trim()));
            }
            if let Some(platform) = platform.as_ref().filter(|v| !v.trim().is_empty()) {
                terms.push(format!("{} {}", name.trim(), platform.trim()));
            }
            if !name.trim().is_empty() {
                terms.push(name.trim().to_string());
            }
        }
        MediaWant::SportEvent {
            league_name,
            season,
            round,
            location,
            session_type,
            event_title,
            ..
        } => {
            let league = strip_diacritics(league_name.trim());
            let szn = season.trim();

            // Motorsport path: location + session_type are set.
            // Single broad query — structural matching filters results.
            // Round is omitted (R03 vs Round03 varies, text search catches both).
            if let (Some(_round), Some(location), Some(session_type)) =
                (round, location.as_deref(), session_type.as_deref())
            {
                let loc = strip_diacritics(location.trim());
                let sess = search_session(session_type.trim());
                if sess.is_empty() {
                    // Default session (Race/GP) — omit, most releases don't include it
                    terms.push(format!("{league} {szn} {loc}"));
                } else {
                    terms.push(format!("{league} {szn} {loc} {sess}"));
                }
            }

            // Event title search terms.
            // For motorsport (location+session_type set), the round-based terms
            // above are sufficient — bare event titles like "Brazil GP" waste
            // searches by returning every sport's Brazil race.
            // For team/combat sports, event_title is the primary identifier.
            let is_motorsport = location.is_some() && session_type.is_some();
            if !is_motorsport && let Some(event_title) = event_title.as_deref() {
                let title = strip_diacritics(event_title.trim());
                if !title.is_empty() {
                    // "UFC 302 Main Card 2025" / "Arsenal vs Chelsea 2024"
                    terms.push(format!("{title} {szn}"));
                    // League-qualified, but skip if title already starts with
                    // league name to avoid "UFC 2025 UFC 302 Main Card"
                    if !title.to_lowercase().starts_with(&league.to_lowercase()) {
                        terms.push(format!("{league} {szn} {title}"));
                    }
                }
            }
        }
        MediaWant::SportSeason {
            league_name,
            season,
            ..
        } => {
            terms.push(format!("{} {}", league_name.trim(), season.trim()));
        }
    }

    dedupe_terms(terms)
}

pub fn match_targets_from_media_want(media_want: &MediaWant) -> Vec<MatchTarget> {
    let (target, fallback_semantic) = match media_want {
        MediaWant::Book {
            title,
            author,
            series,
            alternate_titles,
            ..
        }
        | MediaWant::Audiobook {
            title,
            author,
            series,
            alternate_titles,
            ..
        } => {
            let st = Some(MatchedTarget::Book);
            let author = author.as_deref().map(str::trim).filter(|v| !v.is_empty());
            let series = series.as_deref().map(str::trim).filter(|v| !v.is_empty());
            let titles = std::iter::once((title.trim(), "media".to_string())).chain(
                alternate_titles
                    .iter()
                    .flatten()
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty())
                    .map(|t| (t, format!("alt_{t}"))),
            );
            let mut targets = Vec::new();
            for (title, id) in titles {
                if title.is_empty() {
                    continue;
                }
                let Some(author) = author else {
                    targets.push(MatchTarget {
                        id,
                        target: title.to_string(),
                        min_score: None,
                        semantic_target: st.clone(),
                    });
                    continue;
                };
                targets.extend([
                    MatchTarget {
                        id: id.clone(),
                        target: format!("{title} {author}"),
                        min_score: None,
                        semantic_target: st.clone(),
                    },
                    MatchTarget {
                        id: format!("{id}_reversed"),
                        target: format!("{author} {title}"),
                        min_score: None,
                        semantic_target: st.clone(),
                    },
                    MatchTarget {
                        id: format!("{id}_title_only"),
                        target: title.to_string(),
                        min_score: Some(0.9),
                        semantic_target: st.clone(),
                    },
                ]);
                if let Some(series_name) = series {
                    targets.push(MatchTarget {
                        id: format!("{id}_series"),
                        target: format!("{series_name} {title}"),
                        min_score: Some(0.80),
                        semantic_target: st.clone(),
                    });
                }
            }
            return targets;
        }
        MediaWant::TvEpisode {
            show_name,
            season,
            episode,
            ..
        } => {
            let primary = format_episode_target(show_name, season, episode);
            let st = Some(MatchedTarget::Episode {
                season: season.clone(),
                episode: episode.clone(),
            });
            let mut targets = vec![MatchTarget {
                id: "media".to_string(),
                target: primary,
                min_score: None,
                semantic_target: st.clone(),
            }];
            // Article-stripped variant (e.g. "The Flash" → "Flash S01E05")
            if let Some(stripped) = strip_leading_article(show_name.trim()) {
                targets.push(MatchTarget {
                    id: "media_no_article".to_string(),
                    target: format_episode_target(stripped, season, episode),
                    min_score: Some(0.85),
                    semantic_target: st.clone(),
                });
            }
            return targets;
        }
        MediaWant::TvSeason {
            show_name, season, ..
        } => {
            // Return two targets: "Show Season 1" and compact "Show S01"
            let target1 = format!("{} Season {}", show_name.trim(), season);
            let target2 = if let Ok(s_num) = season.parse::<u32>() {
                format!("{} S{:02}", show_name.trim(), s_num)
            } else {
                format!("{} {}", show_name.trim(), season)
            };
            if target1.trim().is_empty() {
                return Vec::new();
            }
            let st = Some(MatchedTarget::Season {
                season: season.clone(),
            });
            let mut targets = vec![
                MatchTarget {
                    id: "media".to_string(),
                    target: target1,
                    min_score: None,
                    semantic_target: st.clone(),
                },
                MatchTarget {
                    id: "media_compact".to_string(),
                    target: target2,
                    min_score: None,
                    semantic_target: st.clone(),
                },
            ];
            // Article-stripped variants (e.g. "The Flash" → "Flash Season 1" + "Flash S01")
            if let Some(stripped) = strip_leading_article(show_name.trim()) {
                targets.push(MatchTarget {
                    id: "media_no_article".to_string(),
                    target: format!("{stripped} Season {season}"),
                    min_score: Some(0.85),
                    semantic_target: st.clone(),
                });
                let compact_stripped = if let Ok(s_num) = season.parse::<u32>() {
                    format!("{stripped} S{s_num:02}")
                } else {
                    format!("{stripped} {season}")
                };
                targets.push(MatchTarget {
                    id: "media_compact_no_article".to_string(),
                    target: compact_stripped,
                    min_score: Some(0.85),
                    semantic_target: st.clone(),
                });
            }
            return targets;
        }
        MediaWant::Movie { title, year, .. } => {
            let primary = match year {
                Some(year) if *year > 0 => format!("{} {}", title.trim(), year),
                _ => title.trim().to_string(),
            };
            let st = Some(MatchedTarget::Movie);
            let mut targets = vec![MatchTarget {
                id: "media".to_string(),
                target: primary,
                min_score: None,
                semantic_target: st.clone(),
            }];
            // Article-stripped variant
            if let Some(stripped) = strip_leading_article(title.trim()) {
                let alt = match year {
                    Some(year) if *year > 0 => format!("{} {}", stripped, year),
                    _ => stripped.to_string(),
                };
                targets.push(MatchTarget {
                    id: "media_no_article".to_string(),
                    target: alt,
                    min_score: Some(0.85),
                    semantic_target: st.clone(),
                });
            }
            return targets;
        }
        MediaWant::Comic {
            title,
            issue,
            series,
            ..
        } => {
            if let Some(issue) = issue {
                // Generate both "Title #100" and "Title 100" — releases rarely use #
                let with_hash = format!("{} #{}", title.trim(), issue);
                let without_hash = format!("{} {}", title.trim(), issue);
                let st = Some(MatchedTarget::Media);
                return vec![
                    MatchTarget {
                        id: "media".to_string(),
                        target: with_hash,
                        min_score: None,
                        semantic_target: st.clone(),
                    },
                    MatchTarget {
                        id: "media_no_hash".to_string(),
                        target: without_hash,
                        min_score: None,
                        semantic_target: st,
                    },
                ];
            } else if let Some(series) = series.as_ref().filter(|v| !v.trim().is_empty()) {
                (
                    format!("{} {}", series.trim(), title.trim()),
                    Some(MatchedTarget::Media),
                )
            } else {
                (title.trim().to_string(), Some(MatchedTarget::Media))
            }
        }
        MediaWant::Music { artist, album, .. } => {
            if let Some(album) = album.as_ref().filter(|v| !v.trim().is_empty()) {
                (
                    format!("{} {}", artist.trim(), album.trim()),
                    Some(MatchedTarget::Media),
                )
            } else {
                (artist.trim().to_string(), Some(MatchedTarget::Media))
            }
        }
        MediaWant::Game {
            title, platform, ..
        } => {
            if let Some(platform) = platform.as_ref().filter(|v| !v.trim().is_empty()) {
                (
                    format!("{} {}", title.trim(), platform.trim()),
                    Some(MatchedTarget::Media),
                )
            } else {
                (title.trim().to_string(), Some(MatchedTarget::Media))
            }
        }
        MediaWant::Software { name, version, .. } => {
            if let Some(version) = version.as_ref().filter(|v| !v.trim().is_empty()) {
                (
                    format!("{} {}", name.trim(), version.trim()),
                    Some(MatchedTarget::Media),
                )
            } else {
                (name.trim().to_string(), Some(MatchedTarget::Media))
            }
        }
        MediaWant::SportEvent {
            league_name,
            season,
            round,
            location,
            session_type,
            event_title,
            meta,
            ..
        } => {
            let league = strip_diacritics(league_name.trim());
            let szn = season.trim();
            let st = Some(MatchedTarget::SportEvent);
            let mut targets = Vec::new();

            // Motorsport: location + session_type are set
            if let (Some(round), Some(location), Some(session_type)) =
                (round, location.as_deref(), session_type.as_deref())
            {
                let loc = strip_diacritics(location.trim());
                let sess = session_type.trim();
                targets.push(MatchTarget {
                    id: "media".to_string(),
                    target: format!("{league} {szn} R{round:02} {loc} {sess}"),
                    min_score: Some(0.75),
                    semantic_target: st.clone(),
                });
                targets.push(MatchTarget {
                    id: "media_broad".to_string(),
                    target: strip_noise_words(&format!("{league} {szn} R{round:02} {loc}")),
                    min_score: Some(0.85),
                    semantic_target: st.clone(),
                });
            }

            // Broad target without round/session for releases that omit them
            if let Some(location) = location.as_ref().filter(|v| !v.trim().is_empty()) {
                targets.push(MatchTarget {
                    id: "media_broad".to_string(),
                    target: format!("{league} {szn} {}", strip_diacritics(location.trim())),
                    min_score: Some(0.85),
                    semantic_target: st.clone(),
                });
            }

            // Event title targets only for team/combat sports (no location+session_type).
            // For motorsport, the league+round+location targets above are sufficient
            // and event_title-based targets cause cross-sport false positives
            // (e.g., "Brazil GP" or "MotoGP 2026 Brazil GP" matching F1 releases).
            let team_sport = targets.is_empty();
            if team_sport && let Some(event_title) = event_title.as_deref() {
                let title = strip_diacritics(event_title.trim());
                if !title.is_empty() {
                    // Title with year: "UFC 302 Main Card 2025"
                    targets.push(MatchTarget {
                        id: "media_title_year".to_string(),
                        target: format!("{title} {szn}"),
                        min_score: None,
                        semantic_target: st.clone(),
                    });
                    // League-qualified, skip if title already contains league
                    // to avoid "UFC 2025 UFC 302 Main Card"
                    if !title.to_lowercase().starts_with(&league.to_lowercase()) {
                        targets.push(MatchTarget {
                            id: "media".to_string(),
                            target: format!("{league} {szn} {title}"),
                            min_score: None,
                            semantic_target: st.clone(),
                        });
                    }
                }
            }

            if team_sport {
                targets.extend(team_targets(meta.teams.as_deref(), szn));
            }

            if !targets.is_empty() {
                return targets;
            }

            // Fallback
            (format!("{league} {szn}"), Some(MatchedTarget::SportEvent))
        }
        MediaWant::SportSeason {
            league_name,
            season,
            ..
        } => (
            format!("{} {}", league_name.trim(), season.trim()),
            Some(MatchedTarget::Media),
        ),
    };

    if target.trim().is_empty() {
        Vec::new()
    } else {
        vec![MatchTarget {
            id: "media".to_string(),
            target,
            min_score: None,
            semantic_target: fallback_semantic,
        }]
    }
}

/// Every pairing of the two teams' names, so "Man Utd vs Arsenal" finds a
/// want for Manchester United. Scored like the other alias targets:
/// near-exact or nothing.
fn team_targets(teams: Option<&[nats_jobs::SportTeam]>, season: &str) -> Vec<MatchTarget> {
    let [home, away] = teams.unwrap_or_default() else {
        return Vec::new();
    };
    let names = |t: &nats_jobs::SportTeam| {
        let mut out = vec![t.name.trim().to_string()];
        for n in t.short.iter().chain(&t.aliases) {
            let n = n.trim().to_string();
            if !n.is_empty() && !out.contains(&n) {
                out.push(n);
            }
        }
        out
    };
    let (homes, aways) = (names(home), names(away));
    homes
        .iter()
        .flat_map(|h| aways.iter().map(move |a| (h, a)))
        .filter(|(h, a)| !h.is_empty() && !a.is_empty())
        .map(|(h, a)| MatchTarget {
            id: format!("teams_{h}_{a}"),
            target: format!("{h} vs {a} {season}"),
            min_score: Some(0.80),
            semantic_target: Some(MatchedTarget::SportEvent),
        })
        .collect()
}

pub fn media_quality_profile_id(media_want: &MediaWant) -> Option<uuid::Uuid> {
    match media_want {
        MediaWant::TvEpisode {
            quality_profile_id, ..
        }
        | MediaWant::TvSeason {
            quality_profile_id, ..
        }
        | MediaWant::Movie {
            quality_profile_id, ..
        }
        | MediaWant::SportEvent {
            quality_profile_id, ..
        }
        | MediaWant::SportSeason {
            quality_profile_id, ..
        } => *quality_profile_id,
        _ => None,
    }
}

pub fn media_quality_overrides(media_want: &MediaWant) -> Option<&nats_jobs::QualityOverrides> {
    match media_want {
        MediaWant::TvEpisode {
            quality_overrides, ..
        }
        | MediaWant::TvSeason {
            quality_overrides, ..
        }
        | MediaWant::Movie {
            quality_overrides, ..
        }
        | MediaWant::SportEvent {
            quality_overrides, ..
        }
        | MediaWant::SportSeason {
            quality_overrides, ..
        } => quality_overrides.as_ref(),
        _ => None,
    }
}

/// Map a MediaWant to the `media_kind` string consumed by [`crate::parsing::parse_release`].
///
/// This is the kind that drives release-name parsing (and is stored on
/// `downloads.media_kind`), so the interpretation kobload sends to kobbler keeps
/// sport location/round/session, TV season/episode, etc. Sports must map to
/// `"sports"` — mapping it to `"tv"` (as quality-profile lookup does) would lose
/// the sport structure. Comics have no dedicated parser, so they fall to `"misc"`.
pub fn parse_media_kind(media_want: &MediaWant) -> &'static str {
    match media_want {
        MediaWant::TvEpisode { .. } | MediaWant::TvSeason { .. } => "tv",
        MediaWant::SportEvent { .. } | MediaWant::SportSeason { .. } => "sports",
        MediaWant::Movie { .. } => "movies",
        MediaWant::Book { .. } => "ebooks",
        MediaWant::Audiobook { .. } => "audiobooks",
        MediaWant::Music { .. } => "music",
        MediaWant::Game { .. } => "games",
        MediaWant::Software { .. } => "software",
        MediaWant::Comic { .. } => "misc",
    }
}

/// Map MediaWant to quality_profiles.media_kind value for default profile lookup.
pub fn quality_media_kind(media_want: &MediaWant) -> Option<&'static str> {
    match media_want {
        MediaWant::TvEpisode { .. }
        | MediaWant::TvSeason { .. }
        | MediaWant::SportEvent { .. }
        | MediaWant::SportSeason { .. } => Some("tv"),
        MediaWant::Movie { .. } => Some("movie"),
        _ => None,
    }
}

/// Map a media_types.key (e.g. "movies", "sports") to the quality_profiles.media_kind
/// value used for default profile lookup.  media_type keys are plural ("movies", "sports")
/// while quality profiles use singular kind values ("movie", "tv").
pub fn media_type_key_to_quality_kind(key: &str) -> &str {
    match key {
        "movies" => "movie",
        "tv" | "sports" => "tv",
        other => other,
    }
}

/// Extract external IDs from a MediaWant for identifier-based search.
pub fn external_ids_from_media_want(media_want: &MediaWant) -> Vec<crate::watch::ExternalId> {
    use crate::watch::{ExternalId, ExternalIdSource};
    let mut ids = Vec::new();

    match media_want {
        MediaWant::Book {
            identifiers, isbn, ..
        }
        | MediaWant::Audiobook {
            identifiers, isbn, ..
        } => {
            if let Some(isbn) = isbn.as_ref().filter(|v| !v.trim().is_empty()) {
                ids.push(ExternalId {
                    source: ExternalIdSource::Isbn,
                    id: isbn.trim().to_string(),
                });
            }
            if let Some(idents) = identifiers {
                if let Some(isbn13) = idents.isbn13.as_deref().filter(|v| !v.is_empty()) {
                    ids.push(ExternalId {
                        source: ExternalIdSource::Isbn,
                        id: isbn13.to_string(),
                    });
                }
                if let Some(isbn10) = idents.isbn10.as_deref().filter(|v| !v.is_empty()) {
                    ids.push(ExternalId {
                        source: ExternalIdSource::Isbn,
                        id: isbn10.to_string(),
                    });
                }
            }
            // Deduplicate ISBN entries by value
            let mut seen = std::collections::HashSet::new();
            ids.retain(|id| {
                if id.source == ExternalIdSource::Isbn {
                    seen.insert(id.id.clone())
                } else {
                    true
                }
            });
        }
        MediaWant::TvEpisode { identifiers, .. } => {
            extract_tv_ids(identifiers.as_ref(), &mut ids);
        }
        MediaWant::TvSeason { identifiers, .. } => {
            extract_tv_ids(identifiers.as_ref(), &mut ids);
        }
        MediaWant::Movie {
            identifiers: Some(idents),
            ..
        } => {
            if let Some(imdb) = idents.imdb_id.as_deref().filter(|v| !v.is_empty()) {
                ids.push(ExternalId {
                    source: ExternalIdSource::Imdb,
                    id: imdb.to_string(),
                });
            }
            if let Some(ref tmdb) = idents.tmdb_id {
                ids.push(ExternalId {
                    source: ExternalIdSource::Tmdb,
                    id: tmdb.clone(),
                });
            }
        }
        MediaWant::Movie {
            identifiers: None, ..
        }
        | MediaWant::Comic { .. }
        | MediaWant::Music { .. }
        | MediaWant::Game { .. }
        | MediaWant::Software { .. }
        | MediaWant::SportEvent { .. }
        | MediaWant::SportSeason { .. } => {}
    }

    ids
}

fn extract_tv_ids(
    identifiers: Option<&nats_jobs::TvIdentifiers>,
    ids: &mut Vec<crate::watch::ExternalId>,
) {
    use crate::watch::{ExternalId, ExternalIdSource};
    if let Some(idents) = identifiers {
        if let Some(imdb) = idents.imdb_id.as_deref().filter(|v| !v.is_empty()) {
            ids.push(ExternalId {
                source: ExternalIdSource::Imdb,
                id: imdb.to_string(),
            });
        }
        if let Some(ref tmdb) = idents.tmdb_id {
            ids.push(ExternalId {
                source: ExternalIdSource::Tmdb,
                id: tmdb.clone(),
            });
        }
        if let Some(ref tvdb) = idents.tvdb_id {
            ids.push(ExternalId {
                source: ExternalIdSource::Tvdb,
                id: tvdb.clone(),
            });
        }
    }
}

/// Generate search terms for a specific episode.
///
/// For numeric episodes: "Show S01E05", "Show 1x05"
/// For date-based episodes: "Show 2024-01-15", "Show 2024.01.15"
/// Extract season and episode numbers from a MediaWant for structured Newznab search params.
pub fn season_episode_from_media_want(media_want: &MediaWant) -> (Option<String>, Option<String>) {
    match media_want {
        MediaWant::TvEpisode {
            season, episode, ..
        } => (Some(season.clone()), Some(episode.clone())),
        MediaWant::TvSeason { season, .. } => (Some(season.clone()), None),
        _ => (None, None),
    }
}

pub fn episode_search_terms(show_name: &str, season: &str, episode: &str) -> Vec<String> {
    let show = show_name.trim();
    let mut terms = Vec::new();

    if let (Some(s_num), Some(e_num)) = (season.parse::<u32>().ok(), episode.parse::<u32>().ok()) {
        // Numeric: standard TV format
        terms.push(format!("{} S{:02}E{:02}", show, s_num, e_num));
        terms.push(format!("{} {}x{:02}", show, s_num, e_num));
    } else if episode.contains('-') && episode.len() == 10 {
        // Date-based episode (YYYY-MM-DD)
        terms.push(format!("{} {}", show, episode));
        terms.push(format!("{} {}", show, episode.replace('-', ".")));
    } else {
        // Fallback: use as-is
        terms.push(format!("{} {}", show, episode));
    }

    terms
}

/// Format a match target string for an episode.
pub fn format_episode_target(show_name: &str, season: &str, episode: &str) -> String {
    let show = show_name.trim();
    if let (Some(s_num), Some(e_num)) = (season.parse::<u32>().ok(), episode.parse::<u32>().ok()) {
        format!("{} S{:02}E{:02}", show, s_num, e_num)
    } else {
        format!("{} {}", show, episode)
    }
}

/// Generate a MatchTarget for a specific episode (used in fallback logic).
pub fn episode_match_target(show_name: &str, season: &str, episode: &str) -> MatchTarget {
    MatchTarget {
        id: format!("episode_{}", episode),
        target: format_episode_target(show_name, season, episode),
        min_score: None,
        semantic_target: Some(MatchedTarget::Episode {
            season: season.to_string(),
            episode: episode.to_string(),
        }),
    }
}

/// Generate additional match targets for XEM scene numbering.
/// Used when a show has scene numbering that differs from TMDB/TVDB.
pub fn xem_match_targets(
    show_name: &str,
    scene_season: u32,
    scene_episode: u32,
    tvdb_season: u32,
    tvdb_episode: u32,
) -> Vec<MatchTarget> {
    vec![MatchTarget {
        id: "media_xem".to_string(),
        target: format!(
            "{} S{:02}E{:02}",
            show_name.trim(),
            scene_season,
            scene_episode
        ),
        min_score: None,
        semantic_target: Some(MatchedTarget::SceneEpisode {
            scene_season,
            scene_episode,
            tvdb_season,
            tvdb_episode,
        }),
    }]
}

/// Generate additional match targets for XEM scene numbering at the season level.
/// Used when a show has scene numbering that differs from TMDB/TVDB.
pub fn xem_season_match_targets(show_name: &str, scene_season: u32) -> Vec<MatchTarget> {
    let st = Some(MatchedTarget::Season {
        season: scene_season.to_string(),
    });
    vec![
        MatchTarget {
            id: "media_xem".to_string(),
            target: format!("{} Season {}", show_name.trim(), scene_season),
            min_score: None,
            semantic_target: st.clone(),
        },
        MatchTarget {
            id: "media_xem_compact".to_string(),
            target: format!("{} S{:02}", show_name.trim(), scene_season),
            min_score: None,
            semantic_target: st,
        },
    ]
}

fn dedupe_terms(terms: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();

    for term in terms {
        let trimmed = term.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.to_lowercase();
        if seen.insert(normalized) {
            output.push(trimmed.to_string());
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_book_terms() {
        let want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Sanderson".to_string()),
            isbn: Some("9780765365432".to_string()),
            format_preferences: Vec::new(),
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };

        let terms = search_terms_from_media_want(&want);
        // Primary: "Author Title", fallback: "Title"
        assert_eq!(terms.len(), 2, "Expected 2 terms, got: {terms:?}");
        assert_eq!(terms[0], "Sanderson Mistborn");
        assert_eq!(terms[1], "Mistborn");
        // ISBNs no longer in search cascade (return 0 results on indexers)
        assert!(!terms.iter().any(|t| t.contains("9780765365432")));
    }

    #[test]
    fn test_tv_episode_terms() {
        let want = MediaWant::TvEpisode {
            show_name: "Show".to_string(),
            season: "1".to_string(),
            episode: "2".to_string(),
            year: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };

        let terms = search_terms_from_media_want(&want);
        assert!(terms.iter().any(|t| t.contains("S01E02")));
    }

    #[test]
    fn test_tv_episode_daily_terms() {
        let want = MediaWant::TvEpisode {
            show_name: "Daily Show".to_string(),
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

        let terms = search_terms_from_media_want(&want);
        assert!(
            terms.iter().any(|t| t.contains("2024-01-15")),
            "terms: {terms:?}"
        );
        assert!(
            terms.iter().any(|t| t.contains("2024.01.15")),
            "terms: {terms:?}"
        );
    }

    #[test]
    fn test_tv_season_dual_targets() {
        let want = MediaWant::TvSeason {
            show_name: "Breaking Bad".to_string(),
            season: "3".to_string(),
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

        let targets = match_targets_from_media_want(&want);
        let target_strings: Vec<&str> = targets.iter().map(|t| t.target.as_str()).collect();
        assert!(
            target_strings.contains(&"Breaking Bad Season 3"),
            "missing long format: {target_strings:?}"
        );
        assert!(
            target_strings.contains(&"Breaking Bad S03"),
            "missing compact format: {target_strings:?}"
        );
    }

    #[test]
    fn test_media_type_key_mapping() {
        let cases: Vec<(MediaWant, &str)> = vec![
            (
                MediaWant::Book {
                    title: "t".into(),
                    author: None,
                    isbn: None,
                    format_preferences: vec![],
                    identifiers: None,
                    series: None,
                    series_position: None,
                    release_date: None,
                    alternate_titles: None,
                },
                "ebooks",
            ),
            (
                MediaWant::Audiobook {
                    title: "t".into(),
                    author: None,
                    narrator: None,
                    isbn: None,
                    format_preferences: vec![],
                    identifiers: None,
                    series: None,
                    series_position: None,
                    release_date: None,
                    alternate_titles: None,
                },
                "audiobooks",
            ),
            (
                MediaWant::Comic {
                    title: "t".into(),
                    issue: None,
                    series: None,
                    year: None,
                },
                "comics",
            ),
            (
                MediaWant::Music {
                    artist: "a".into(),
                    album: None,
                    year: None,
                },
                "music",
            ),
            (
                MediaWant::Game {
                    title: "t".into(),
                    platform: None,
                    year: None,
                },
                "games",
            ),
            (
                MediaWant::Software {
                    name: "n".into(),
                    version: None,
                    platform: None,
                },
                "software",
            ),
        ];
        for (want, expected) in cases {
            assert_eq!(media_type_key_from_media_want(&want), expected);
        }
    }

    #[test]
    fn test_media_type_key_to_quality_kind() {
        // These are the keys that have quality profiles — must map correctly
        assert_eq!(media_type_key_to_quality_kind("movies"), "movie");
        assert_eq!(media_type_key_to_quality_kind("tv"), "tv");
        assert_eq!(media_type_key_to_quality_kind("sports"), "tv");
        // Keys without quality profiles pass through unchanged
        assert_eq!(media_type_key_to_quality_kind("ebooks"), "ebooks");
        assert_eq!(media_type_key_to_quality_kind("audiobooks"), "audiobooks");
    }

    #[test]
    fn test_parse_media_kind_sports_maps_to_sports() {
        // The crux of the manual-grab fix: a sport want must drive parse_release
        // with "sports" (NOT "tv"), or the interpretation kobbler ingests loses
        // location/round/session and the event gets mis-placed.
        let sport = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(3),
            location: Some("Czechia".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        assert_eq!(parse_media_kind(&sport), "sports");

        let movie = MediaWant::Movie {
            title: "Inception".to_string(),
            year: Some(2010),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
            release_date: None,
            in_library: None,
            library_video_height: None,
        };
        assert_eq!(parse_media_kind(&movie), "movies");
    }

    #[test]
    fn test_comic_terms() {
        let want = MediaWant::Comic {
            title: "Saga".to_string(),
            issue: Some(54),
            series: Some("Image".to_string()),
            year: Some(2023),
        };
        let terms = search_terms_from_media_want(&want);
        assert!(terms.iter().any(|t| t == "Saga #54"), "terms: {terms:?}");
        assert!(terms.iter().any(|t| t == "Image Saga"), "terms: {terms:?}");
    }

    #[test]
    fn test_music_terms() {
        let want = MediaWant::Music {
            artist: "Radiohead".to_string(),
            album: Some("OK Computer".to_string()),
            year: Some(1997),
        };
        let terms = search_terms_from_media_want(&want);
        assert!(
            terms.iter().any(|t| t == "Radiohead OK Computer"),
            "terms: {terms:?}"
        );
        assert!(terms.iter().any(|t| t == "Radiohead"), "terms: {terms:?}");

        let targets = match_targets_from_media_want(&want);
        assert_eq!(targets[0].target, "Radiohead OK Computer");
    }

    #[test]
    fn test_game_terms() {
        let want = MediaWant::Game {
            title: "Elden Ring".to_string(),
            platform: Some("PC".to_string()),
            year: None,
        };
        let terms = search_terms_from_media_want(&want);
        assert!(
            terms.iter().any(|t| t == "Elden Ring PC"),
            "terms: {terms:?}"
        );
        assert!(terms.iter().any(|t| t == "Elden Ring"), "terms: {terms:?}");
    }

    #[test]
    fn test_software_terms() {
        let want = MediaWant::Software {
            name: "Photoshop".to_string(),
            version: Some("2024".to_string()),
            platform: Some("Windows".to_string()),
        };
        let terms = search_terms_from_media_want(&want);
        assert!(
            terms.iter().any(|t| t == "Photoshop 2024"),
            "terms: {terms:?}"
        );
        assert!(
            terms.iter().any(|t| t == "Photoshop Windows"),
            "terms: {terms:?}"
        );
    }

    #[test]
    fn test_match_targets_book_multiple() {
        let want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Sanderson".to_string()),
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };
        let targets = match_targets_from_media_want(&want);
        assert_eq!(targets.len(), 3, "Expected 3 targets, got: {targets:?}");
        assert_eq!(targets[0].target, "Mistborn Sanderson");
        assert_eq!(targets[1].target, "Sanderson Mistborn");
        assert_eq!(targets[2].target, "Mistborn");
    }

    #[test]
    fn test_match_targets_book_no_author() {
        let want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: None,
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };
        let targets = match_targets_from_media_want(&want);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target, "Mistborn");
    }

    #[test]
    fn test_search_terms_book_with_series() {
        let want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Sanderson".to_string()),
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: Some("Cosmere".to_string()),
            series_position: Some("1".to_string()),
            release_date: None,
            alternate_titles: None,
        };
        let terms = search_terms_from_media_want(&want);
        assert_eq!(terms.len(), 3, "Expected 3 terms, got: {terms:?}");
        assert_eq!(terms[0], "Sanderson Mistborn");
        assert_eq!(terms[1], "Cosmere Mistborn");
        assert_eq!(terms[2], "Mistborn");
    }

    #[test]
    fn test_external_ids_isbn10() {
        use crate::watch::ExternalIdSource;

        let want = MediaWant::Book {
            title: "Test".to_string(),
            author: None,
            isbn: None,
            format_preferences: vec![],
            identifiers: Some(nats_jobs::BookIdentifiers {
                isbn10: Some("0765311780".to_string()),
                isbn13: None,
                hardcover_id: None,
                goodreads_id: None,
                openlibrary_id: None,
            }),
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };

        let ids = external_ids_from_media_want(&want);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].source, ExternalIdSource::Isbn);
        assert_eq!(ids[0].id, "0765311780");
    }

    #[test]
    fn test_external_ids_no_duplicates() {
        use crate::watch::ExternalIdSource;

        // isbn field and isbn13 have the same value
        let want = MediaWant::Book {
            title: "Test".to_string(),
            author: None,
            isbn: Some("9780765311788".to_string()),
            format_preferences: vec![],
            identifiers: Some(nats_jobs::BookIdentifiers {
                isbn10: Some("0765311780".to_string()),
                isbn13: Some("9780765311788".to_string()),
                hardcover_id: None,
                goodreads_id: None,
                openlibrary_id: None,
            }),
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };

        let ids = external_ids_from_media_want(&want);
        // Should have 2: isbn13/isbn deduped to one, plus isbn10
        let isbn_ids: Vec<_> = ids
            .iter()
            .filter(|id| id.source == ExternalIdSource::Isbn)
            .collect();
        assert_eq!(
            isbn_ids.len(),
            2,
            "Expected 2 unique ISBNs, got: {isbn_ids:?}"
        );
        let values: Vec<&str> = isbn_ids.iter().map(|id| id.id.as_str()).collect();
        assert!(values.contains(&"9780765311788"));
        assert!(values.contains(&"0765311780"));
    }

    #[test]
    fn test_external_ids_from_tv_want() {
        use crate::watch::ExternalIdSource;

        let want = MediaWant::TvEpisode {
            show_name: "Test".to_string(),
            season: "1".to_string(),
            episode: "1".to_string(),
            year: None,
            identifiers: Some(nats_jobs::TvIdentifiers {
                tmdb_id: Some("12345".to_string()),
                tvdb_id: Some("67890".to_string()),
                imdb_id: Some("tt1234567".to_string()),
            }),
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            air_date: None,
        };

        let ids = external_ids_from_media_want(&want);
        assert_eq!(ids.len(), 3);
        assert!(
            ids.iter()
                .any(|id| id.source == ExternalIdSource::Imdb && id.id == "tt1234567")
        );
        assert!(
            ids.iter()
                .any(|id| id.source == ExternalIdSource::Tmdb && id.id == "12345")
        );
        assert!(
            ids.iter()
                .any(|id| id.source == ExternalIdSource::Tvdb && id.id == "67890")
        );
    }

    #[test]
    fn test_sport_event_terms_motorsport() {
        let want = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2025".to_string(),
            round: Some(1),
            location: Some("Thailand".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let terms = search_terms_from_media_want(&want);
        assert_eq!(
            terms,
            vec!["MotoGP 2025 Thailand"],
            "Race session should be omitted from search: {terms:?}"
        );
    }

    #[test]
    fn test_sport_event_terms_diacritics() {
        let want = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2025".to_string(),
            round: Some(7),
            location: Some("España".to_string()),
            session_type: Some("Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let terms = search_terms_from_media_want(&want);
        assert!(
            terms.iter().any(|t| t == "MotoGP 2025 Espana"),
            "diacritics not stripped: {terms:?}"
        );
        assert!(
            !terms.iter().any(|t| t.contains("España")),
            "original diacritics should not appear: {terms:?}"
        );
    }

    #[test]
    fn test_sport_event_targets_motorsport() {
        let want = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2025".to_string(),
            round: Some(1),
            location: Some("Thailand".to_string()),
            session_type: Some("Sprint Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let targets = match_targets_from_media_want(&want);
        assert!(
            targets.len() >= 2,
            "Expected >= 2 targets, got: {targets:?}"
        );
        assert_eq!(targets[0].target, "MotoGP 2025 R01 Thailand Sprint Race");
    }

    #[test]
    fn test_sport_event_terms_team_sport() {
        let want = MediaWant::SportEvent {
            league_name: "English Premier League".to_string(),
            season: "2024".to_string(),
            round: Some(1),
            location: None,
            session_type: None,
            event_title: Some("Arsenal vs Chelsea".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let terms = search_terms_from_media_want(&want);
        assert!(
            terms
                .iter()
                .any(|t| t == "English Premier League 2024 Arsenal vs Chelsea"),
            "terms: {terms:?}"
        );
        assert!(
            terms.iter().any(|t| t == "Arsenal vs Chelsea 2024"),
            "terms: {terms:?}"
        );
        assert!(
            !terms.iter().any(|t| t == "Arsenal vs Chelsea"),
            "bare title without year should not be a search term: {terms:?}"
        );
    }

    #[test]
    fn test_sport_event_ufc_no_league_duplication() {
        // UFC event_title already starts with "UFC" — should not produce
        // "UFC 2025 UFC 302 Main Card"
        let want = MediaWant::SportEvent {
            league_name: "UFC".to_string(),
            season: "2025".to_string(),
            round: None,
            location: None,
            session_type: None,
            event_title: Some("UFC 302 Main Card".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };

        let terms = search_terms_from_media_want(&want);
        assert!(
            terms.iter().any(|t| t == "UFC 302 Main Card 2025"),
            "Should have title+year term: {terms:?}"
        );
        assert!(
            !terms.iter().any(|t| t.contains("UFC 2025 UFC")),
            "Should not duplicate league name in search: {terms:?}"
        );

        let targets = match_targets_from_media_want(&want);
        assert!(
            targets.iter().any(|t| t.target == "UFC 302 Main Card 2025"),
            "Should have title+year target: {targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t.target.contains("UFC 2025 UFC")),
            "Should not duplicate league name in target: {targets:?}"
        );
    }

    #[test]
    fn test_sport_event_targets_team_sport() {
        let want = MediaWant::SportEvent {
            league_name: "English Premier League".to_string(),
            season: "2024".to_string(),
            round: None,
            location: None,
            session_type: None,
            event_title: Some("Arsenal vs Chelsea".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let targets = match_targets_from_media_want(&want);
        assert!(!targets.is_empty(), "Expected targets, got none");
        assert!(
            targets
                .iter()
                .any(|t| t.target == "Arsenal vs Chelsea 2024"),
            "Should have title+year target: {targets:?}"
        );
        assert!(
            targets
                .iter()
                .any(|t| t.target == "English Premier League 2024 Arsenal vs Chelsea"),
            "Should have league-qualified target: {targets:?}"
        );
    }

    #[test]
    fn test_sport_event_targets_motorsport_with_event_title_no_event_targets() {
        // Motorsport events with location+session_type should NOT get any
        // event_title-based targets — they cause cross-sport false positives
        // (e.g., "Brazil GP" or "MotoGP 2026 Brazil GP" matching F1 releases).
        let want = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Brazil".to_string()),
            session_type: Some("Race".to_string()),
            event_title: Some("Brazil GP".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let targets = match_targets_from_media_want(&want);
        assert!(
            !targets.iter().any(|t| t.id == "media_title_only"),
            "Motorsport events should not have bare title target: {targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t.id == "media_event"),
            "Motorsport events should not have event_title target: {targets:?}"
        );
        // Should still have the motorsport-specific targets
        assert!(
            targets
                .iter()
                .any(|t| t.id == "media" && t.target == "MotoGP 2026 R02 Brazil Race"),
            "Should have motorsport primary target: {targets:?}"
        );
        assert!(
            targets.iter().any(|t| t.id == "media_broad"),
            "Should have motorsport broad target: {targets:?}"
        );
    }

    #[test]
    fn test_sport_event_targets_team_sport_has_title_with_year() {
        // Team sports (no location+session_type) get title+year target, not bare title
        let want = MediaWant::SportEvent {
            league_name: "English Premier League".to_string(),
            season: "2024".to_string(),
            round: None,
            location: None,
            session_type: None,
            event_title: Some("Arsenal vs Chelsea".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let targets = match_targets_from_media_want(&want);
        assert!(
            targets
                .iter()
                .any(|t| t.id == "media_title_year" && t.target == "Arsenal vs Chelsea 2024"),
            "Team sport should have title+year target: {targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t.id == "media_title_only"),
            "Should not have bare title without year: {targets:?}"
        );
    }

    #[test]
    fn test_sport_event_terms_sprint_race_uses_short_session() {
        // "Sprint Race" want should search with just "Sprint" — releases use bare "Sprint"
        let want = MediaWant::SportEvent {
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
            meta: Box::default(),
        };
        let terms = search_terms_from_media_want(&want);
        assert_eq!(
            terms,
            vec!["MotoGP 2026 USA Sprint"],
            "Sprint Race should shorten to Sprint, single query: {terms:?}"
        );
    }

    #[test]
    fn test_sport_event_terms_superpole_race_kept() {
        // Superpole Race is a distinct session from Superpole — keep full name
        let want = MediaWant::SportEvent {
            league_name: "SBK".to_string(),
            season: "2026".to_string(),
            round: Some(2),
            location: Some("Portugal".to_string()),
            session_type: Some("Superpole Race".to_string()),
            event_title: None,
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let terms = search_terms_from_media_want(&want);
        assert_eq!(
            terms,
            vec!["SBK 2026 Portugal Superpole Race"],
            "Superpole Race should stay in search query: {terms:?}"
        );
    }

    #[test]
    fn test_sport_event_terms_race_2_included() {
        // "Race 2" is a distinct SBK session — should be included in search
        let want = MediaWant::SportEvent {
            league_name: "SBK".to_string(),
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
            meta: Box::default(),
        };
        let terms = search_terms_from_media_want(&want);
        assert_eq!(
            terms,
            vec!["SBK 2026 Portugal Race 2"],
            "Race 2 should stay in search query: {terms:?}"
        );
    }

    #[test]
    fn test_strip_diacritics() {
        assert_eq!(strip_diacritics("España"), "Espana");
        assert_eq!(strip_diacritics("São Paulo"), "Sao Paulo");
        assert_eq!(strip_diacritics("Brünn"), "Brunn");
        assert_eq!(strip_diacritics("Thailand"), "Thailand");
    }

    #[test]
    fn test_strip_noise_words() {
        assert_eq!(
            strip_noise_words("MotoGP 2025 R01 Grand Prix Thailand"),
            "MotoGP 2025 R01 Thailand"
        );
        assert_eq!(
            strip_noise_words("F1 2025 R01 World Championship"),
            "F1 2025 R01"
        );
        assert_eq!(
            strip_noise_words("MotoGP 2025 R01 Thailand"),
            "MotoGP 2025 R01 Thailand"
        );
    }

    // ── Book series match target and search term tests ───────────────

    #[test]
    fn test_match_targets_book_with_series() {
        let want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: Some("Cosmere".to_string()),
            series_position: Some("1".to_string()),
            release_date: None,
            alternate_titles: None,
        };
        let targets = match_targets_from_media_want(&want);
        assert_eq!(targets.len(), 4, "Expected 4 targets, got: {targets:?}");
        assert_eq!(targets[0].id, "media");
        assert_eq!(targets[1].id, "media_reversed");
        assert_eq!(targets[2].id, "media_title_only");
        assert_eq!(targets[3].id, "media_series");
        assert_eq!(targets[3].target, "Cosmere Mistborn");
        assert_eq!(targets[3].min_score, Some(0.80));
    }

    #[test]
    fn test_match_targets_book_no_series() {
        let want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };
        let targets = match_targets_from_media_want(&want);
        assert_eq!(
            targets.len(),
            3,
            "Expected 3 targets (no series), got: {targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t.id == "media_series"),
            "Should not have series target without series name"
        );
    }

    #[test]
    fn test_search_terms_book_no_series() {
        let want = MediaWant::Book {
            title: "Mistborn".to_string(),
            author: Some("Sanderson".to_string()),
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };
        let terms = search_terms_from_media_want(&want);
        assert_eq!(
            terms.len(),
            2,
            "Expected 2 terms (no series), got: {terms:?}"
        );
        assert_eq!(terms[0], "Sanderson Mistborn");
        assert_eq!(terms[1], "Mistborn");
    }

    #[test]
    fn test_match_targets_audiobook_with_series() {
        let want = MediaWant::Audiobook {
            title: "Mistborn".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            narrator: Some("Michael Kramer".to_string()),
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: Some("Cosmere".to_string()),
            series_position: Some("1".to_string()),
            release_date: None,
            alternate_titles: None,
        };
        let targets = match_targets_from_media_want(&want);
        assert!(
            targets
                .iter()
                .any(|t| t.id == "media_series" && t.target == "Cosmere Mistborn"),
            "Expected series target for audiobook, got: {targets:?}"
        );
    }

    // ── Article-stripped target/search term tests ─────────────────────

    #[test]
    fn test_tv_season_article_stripped_match_targets() {
        let want = MediaWant::TvSeason {
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
        let targets = match_targets_from_media_want(&want);
        assert!(
            targets
                .iter()
                .any(|t| t.id == "media_no_article" && t.target == "Flash Season 1"),
            "Expected article-stripped season target, got: {targets:?}"
        );
        assert!(
            targets
                .iter()
                .any(|t| t.id == "media_compact_no_article" && t.target == "Flash S01"),
            "Expected article-stripped compact target, got: {targets:?}"
        );
    }

    #[test]
    fn test_tv_episode_article_stripped_match_target() {
        let want = MediaWant::TvEpisode {
            show_name: "The Flash".to_string(),
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
        let targets = match_targets_from_media_want(&want);
        assert!(
            targets
                .iter()
                .any(|t| t.id == "media_no_article" && t.target.starts_with("Flash ")),
            "Expected article-stripped episode target, got: {targets:?}"
        );
    }

    #[test]
    fn test_tv_season_no_article_no_stripped_target() {
        // Shows without a leading article should NOT have stripped targets
        let want = MediaWant::TvSeason {
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
        let targets = match_targets_from_media_want(&want);
        assert!(
            !targets.iter().any(|t| t.id.contains("no_article")),
            "Paradise should NOT have article-stripped targets, got: {targets:?}"
        );
    }

    #[test]
    fn test_tv_season_article_stripped_search_terms() {
        let want = MediaWant::TvSeason {
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
        let terms = search_terms_from_media_want(&want);
        assert!(
            terms.iter().any(|t| t == "Flash S01"),
            "Expected article-stripped search term 'Flash S01', got: {terms:?}"
        );
        assert!(
            terms.iter().any(|t| t == "Flash Season 1"),
            "Expected article-stripped search term 'Flash Season 1', got: {terms:?}"
        );
    }

    #[test]
    fn test_tv_episode_article_stripped_search_terms() {
        let want = MediaWant::TvEpisode {
            show_name: "The Flash".to_string(),
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
        let terms = search_terms_from_media_want(&want);
        assert!(
            terms
                .iter()
                .any(|t| t.contains("Flash") && t.contains("S01E05") && !t.contains("The")),
            "Expected article-stripped search term with 'Flash S01E05', got: {terms:?}"
        );
    }
}
