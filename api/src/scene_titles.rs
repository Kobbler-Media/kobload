//! Alternate show titles for TV matching.
//!
//! Scene releases frequently name a show differently to its metadata provider —
//! regional suffixes ("The Block" → "The Block AU"), anime romanisations, or
//! plain scene convention. Three sources cover this: Sonarr's curated scene
//! mapping list, XEM's title aliases, and the `alternate_titles` a caller
//! already carries on its `MediaWant`.
//!
//! Every search path that matches TV releases needs all three. Resolving them
//! lives here so the want, backlog, and RSS paths share one implementation.

use reqwest::Client;
use sqlx::PgPool;
use tracing::debug;

use crate::scene_mappings::{SceneMappingCache, SceneTitle};
use crate::watch::MatchTarget;
use crate::xem::XemCache;

/// Match targets built from an alternate title score against this instead of
/// the pattern's own threshold. Aliases are a weaker signal than the canonical
/// title, so they need a near-exact hit before they can win a match.
const ALIAS_MIN_SCORE: f64 = 0.80;

fn tvdb_id_of(media_want: &nats_jobs::MediaWant) -> Option<i64> {
    let identifiers = match media_want {
        nats_jobs::MediaWant::TvEpisode { identifiers, .. }
        | nats_jobs::MediaWant::TvSeason { identifiers, .. } => identifiers.as_ref()?,
        _ => return None,
    };
    identifiers.tvdb_id.as_ref()?.parse::<i64>().ok()
}

fn alternate_titles_of(media_want: &nats_jobs::MediaWant) -> &[String] {
    match media_want {
        nats_jobs::MediaWant::TvEpisode {
            alternate_titles, ..
        }
        | nats_jobs::MediaWant::TvSeason {
            alternate_titles, ..
        } => alternate_titles.as_deref().unwrap_or_default(),
        _ => &[],
    }
}

/// Resolve Sonarr scene mapping titles and XEM title aliases for a TV
/// `MediaWant`. Returns an empty vec for non-TV wants or wants with no TVDB ID.
pub async fn resolve(
    scene_mappings: &SceneMappingCache,
    xem_cache: &XemCache,
    http_client: &Client,
    pool: &PgPool,
    media_want: &nats_jobs::MediaWant,
) -> Vec<SceneTitle> {
    let Some(tvdb_id) = tvdb_id_of(media_want) else {
        return Vec::new();
    };

    let mut titles = scene_mappings.get_titles_async(tvdb_id).await;

    // Add XEM title aliases (anime alternate names, etc.)
    for xt in xem_cache.get_titles(http_client, pool, tvdb_id).await {
        // Avoid duplicates — skip if we already have this title+season from scene mappings
        let dominated = titles
            .iter()
            .any(|st| st.title == xt.title && (st.season == xt.season || st.season == -1));
        if !dominated {
            titles.push(SceneTitle {
                title: xt.title,
                season: xt.season,
                filter_regex: None,
            });
        }
    }

    titles
}

/// Add alias-derived entries to a pattern's search terms and match targets.
///
/// Scene/XEM titles are curated, so they become search queries as well as match
/// targets. `alternate_titles` come straight from the metadata provider and
/// include foreign-language variants, so they only ever become match targets —
/// querying indexers for "Het Blok Australie" would just burn API budget.
pub fn augment(
    media_want: &nats_jobs::MediaWant,
    scene_titles: &[SceneTitle],
    patterns: &mut Vec<String>,
    match_targets: &mut Vec<MatchTarget>,
) {
    for st in scene_titles {
        // Skip filtered scene mappings — entries with filterRegex are ambiguous
        // and risk false positives without per-release regex checking
        if st.filter_regex.is_some() {
            debug!(
                title = %st.title,
                filter_regex = ?st.filter_regex,
                "Skipping scene mapping with filterRegex"
            );
            continue;
        }

        if !season_applies(st.season, media_want) {
            continue;
        }

        push_alias_targets(media_want, &st.title, "scene", match_targets);

        if !patterns.contains(&st.title) {
            patterns.push(st.title.clone());
        }
    }

    for title in alternate_titles_of(media_want) {
        push_alias_targets(media_want, title, "alt", match_targets);
    }
}

/// Scene mappings are scoped to a season, with -1 meaning "all seasons".
fn season_applies(scene_season: i32, media_want: &nats_jobs::MediaWant) -> bool {
    if scene_season == -1 {
        return true;
    }
    match media_want {
        nats_jobs::MediaWant::TvEpisode { season, .. }
        | nats_jobs::MediaWant::TvSeason { season, .. } => scene_season.to_string() == *season,
        _ => false,
    }
}

fn push_alias_targets(
    media_want: &nats_jobs::MediaWant,
    title: &str,
    id_prefix: &str,
    match_targets: &mut Vec<MatchTarget>,
) {
    let title = title.trim();
    if title.is_empty() {
        return;
    }

    match media_want {
        nats_jobs::MediaWant::TvEpisode {
            season, episode, ..
        } => {
            match_targets.push(MatchTarget {
                id: format!("{id_prefix}_{title}"),
                target: crate::media::format_episode_target(title, season, episode),
                min_score: Some(ALIAS_MIN_SCORE),
                semantic_target: Some(nats_jobs::MatchedTarget::Episode {
                    season: season.clone(),
                    episode: episode.clone(),
                }),
            });
        }
        nats_jobs::MediaWant::TvSeason { season, .. } => {
            let semantic = Some(nats_jobs::MatchedTarget::Season {
                season: season.clone(),
            });
            if let Ok(s_num) = season.parse::<u32>() {
                match_targets.push(MatchTarget {
                    id: format!("{id_prefix}_{title}_compact"),
                    target: format!("{title} S{s_num:02}"),
                    min_score: Some(ALIAS_MIN_SCORE),
                    semantic_target: semantic.clone(),
                });
            }
            match_targets.push(MatchTarget {
                id: format!("{id_prefix}_{title}"),
                target: format!("{title} Season {season}"),
                min_score: Some(ALIAS_MIN_SCORE),
                semantic_target: semantic,
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::score_against_targets;

    fn season_want(season: &str, alternate_titles: Option<Vec<String>>) -> nats_jobs::MediaWant {
        nats_jobs::MediaWant::TvSeason {
            show_name: "The Block".to_string(),
            season: season.to_string(),
            year: Some(2003),
            episodes_needed: None,
            total_episodes: None,
            pack_preference: "auto".to_string(),
            last_air_date: None,
            identifiers: None,
            alternate_titles,
            quality_profile_id: None,
            quality_overrides: None,
        }
    }

    fn scene_title(title: &str, season: i32) -> SceneTitle {
        SceneTitle {
            title: title.to_string(),
            season,
            filter_regex: None,
        }
    }

    /// The prod bug: "The Block" S22 never matched any "The Block AU" release,
    /// because the matcher's extra-show-name-word guard caps a release carrying
    /// an unexpected word at 0.5. Sonarr's scene mapping list has carried
    /// "The Block AU" for tvdb 74768 all along — it just never reached the
    /// want and backlog search paths.
    #[test]
    fn scene_title_matches_regional_release() {
        let media_want = season_want("22", None);
        let mut patterns = crate::media::search_terms_from_media_want(&media_want);
        let mut match_targets = crate::media::match_targets_from_media_want(&media_want);

        assert!(
            score_against_targets(
                "The.Block.AU.S22E06.1080p.HDTV.x264-NGP",
                &match_targets,
                0.65
            )
            .is_none(),
            "precondition: the canonical title alone must not match the AU release"
        );

        augment(
            &media_want,
            &[scene_title("The Block AU", -1)],
            &mut patterns,
            &mut match_targets,
        );

        let result = score_against_targets(
            "The.Block.AU.S22E06.1080p.HDTV.x264-NGP",
            &match_targets,
            0.65,
        )
        .expect("scene title should match the AU release");
        assert_eq!(result.matched_target, "The Block AU S22");
        assert!(
            patterns.contains(&"The Block AU".to_string()),
            "scene titles must also be searched for, not just matched against"
        );
    }

    #[test]
    fn scene_title_scoped_to_other_season_is_ignored() {
        let media_want = season_want("22", None);
        let mut patterns = Vec::new();
        let mut match_targets = Vec::new();

        augment(
            &media_want,
            &[scene_title("The Block Street", 13)],
            &mut patterns,
            &mut match_targets,
        );

        assert!(
            match_targets.is_empty() && patterns.is_empty(),
            "a season-13 scene mapping must not apply to a season-22 want"
        );
    }

    #[test]
    fn filtered_scene_mappings_are_skipped() {
        let media_want = season_want("22", None);
        let mut patterns = Vec::new();
        let mut match_targets = Vec::new();

        augment(
            &media_want,
            &[SceneTitle {
                title: "Ambiguous".to_string(),
                season: -1,
                filter_regex: Some("^something$".to_string()),
            }],
            &mut patterns,
            &mut match_targets,
        );

        assert!(match_targets.is_empty() && patterns.is_empty());
    }

    /// `alternate_titles` ride along on every TV MediaWant and were being
    /// dropped on the floor. They match but never become search queries —
    /// the provider list includes foreign-language variants.
    #[test]
    fn alternate_titles_match_but_are_not_searched() {
        let media_want = season_want("22", Some(vec!["Het Blok Australie".to_string()]));
        let mut patterns = Vec::new();
        let mut match_targets = Vec::new();

        augment(&media_want, &[], &mut patterns, &mut match_targets);

        assert!(
            patterns.is_empty(),
            "alternate titles must not become indexer queries"
        );
        assert!(
            score_against_targets(
                "Het.Blok.Australie.S22E06.1080p.WEB-DL",
                &match_targets,
                0.65
            )
            .is_some(),
            "alternate titles must still be matchable"
        );
    }

    #[test]
    fn episode_want_gets_episode_shaped_alias_targets() {
        let media_want = nats_jobs::MediaWant::TvEpisode {
            show_name: "The Block".to_string(),
            season: "22".to_string(),
            episode: "6".to_string(),
            year: None,
            air_date: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        let mut patterns = Vec::new();
        let mut match_targets = Vec::new();

        augment(
            &media_want,
            &[scene_title("The Block AU", -1)],
            &mut patterns,
            &mut match_targets,
        );

        assert!(
            score_against_targets(
                "The.Block.AU.S22E06.1080p.HDTV.x264-NGP",
                &match_targets,
                0.65
            )
            .is_some(),
            "episode wants must get alias targets too"
        );
    }
}
