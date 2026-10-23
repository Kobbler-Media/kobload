//! Names the user taught Kobload from the review queue. They are scene-naming
//! knowledge only, never learned on their own, and reach matching by being
//! merged into the want before anything is derived from it.

use nats_jobs::MediaWant;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::warn;

use crate::parsing::{canonical_sport_text, country_for_code};
use crate::repositories::learned_aliases::{self as repo, LearnedAliasRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasScope {
    TvShow,
    Title,
    SportWant,
    SportLeague,
    SportLocation,
}

impl AliasScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TvShow => "tv_show",
            Self::Title => "title",
            Self::SportWant => "sport_want",
            Self::SportLeague => "sport_league",
            Self::SportLocation => "sport_location",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::TvShow,
            Self::Title,
            Self::SportWant,
            Self::SportLeague,
            Self::SportLocation,
        ]
        .into_iter()
        .find(|scope| scope.as_str() == s)
    }
}

/// What an alias is attached to. A want answers to several keys per scope
/// (a TVDB id and its show name); `keys[0]` is the one a new alias is stored
/// under, the rest let a want that lacks the id still find it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScopeTarget {
    pub scope: AliasScope,
    pub keys: Vec<String>,
    pub label: String,
}

fn norm(s: &str) -> String {
    kob_normalize::canonical(s)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn sport_place(s: &str) -> String {
    canonical_sport_text(country_for_code(s.trim()).unwrap_or(s))
}

fn present(s: Option<&String>) -> Option<&str> {
    s.map(|v| v.trim()).filter(|v| !v.is_empty())
}

pub fn scope_targets(want: &MediaWant) -> Vec<ScopeTarget> {
    match want {
        MediaWant::TvEpisode {
            show_name,
            identifiers,
            ..
        }
        | MediaWant::TvSeason {
            show_name,
            identifiers,
            ..
        } => {
            let tvdb = identifiers
                .as_ref()
                .and_then(|i| present(i.tvdb_id.as_ref()))
                .map(|id| format!("tvdb:{id}"));
            vec![ScopeTarget {
                scope: AliasScope::TvShow,
                keys: tvdb
                    .into_iter()
                    .chain([format!("show:{}", norm(show_name))])
                    .collect(),
                label: show_name.clone(),
            }]
        }
        MediaWant::Movie {
            title,
            year,
            identifiers,
            ..
        } => {
            let ids = identifiers.as_ref();
            let keys = [
                ids.and_then(|i| present(i.tmdb_id.as_ref()))
                    .map(|id| format!("movie:tmdb:{id}")),
                ids.and_then(|i| present(i.imdb_id.as_ref()))
                    .map(|id| format!("movie:imdb:{id}")),
                Some(format!(
                    "movie:{}:{}",
                    norm(title),
                    year.map(|y| y.to_string()).unwrap_or_default()
                )),
            ];
            vec![ScopeTarget {
                scope: AliasScope::Title,
                keys: keys.into_iter().flatten().collect(),
                label: match year {
                    Some(y) => format!("{title} ({y})"),
                    None => title.clone(),
                },
            }]
        }
        MediaWant::Book {
            title,
            author,
            isbn,
            identifiers,
            ..
        }
        | MediaWant::Audiobook {
            title,
            author,
            isbn,
            identifiers,
            ..
        } => {
            let ids = identifiers.as_ref();
            let keys = [
                ids.and_then(|i| present(i.hardcover_id.as_ref()))
                    .map(|id| format!("book:hardcover:{id}")),
                ids.and_then(|i| present(i.isbn13.as_ref()).or(present(i.isbn10.as_ref())))
                    .or(present(isbn.as_ref()))
                    .map(|id| format!("book:isbn:{id}")),
                Some(format!(
                    "book:{}:{}",
                    norm(title),
                    author.as_deref().map(norm).unwrap_or_default()
                )),
            ];
            vec![ScopeTarget {
                scope: AliasScope::Title,
                keys: keys.into_iter().flatten().collect(),
                label: title.clone(),
            }]
        }
        MediaWant::SportEvent {
            league_name,
            season,
            round,
            location,
            session_type,
            meta,
            ..
        } => {
            let event = format!(
                "{}|{}|{}|{}|{}",
                norm(league_name),
                season.trim(),
                round.map(|r| r.to_string()).unwrap_or_default(),
                location.as_deref().map(sport_place).unwrap_or_default(),
                session_type.as_deref().map(norm).unwrap_or_default(),
            );
            let mut targets = vec![
                ScopeTarget {
                    scope: AliasScope::SportWant,
                    keys: vec![event],
                    label: [
                        Some(league_name.as_str()),
                        Some(season.as_str()),
                        location.as_deref(),
                        session_type.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" "),
                },
                league_target(league_name),
            ];
            let places: Vec<String> = [
                meta.country_iso.as_ref(),
                meta.country.as_ref(),
                meta.venue.as_ref(),
                location.as_ref(),
            ]
            .into_iter()
            .filter_map(present)
            .map(sport_place)
            .filter(|p| !p.is_empty())
            .fold(Vec::new(), |mut acc, p| {
                if !acc.contains(&p) {
                    acc.push(p);
                }
                acc
            });
            if !places.is_empty() {
                let label = present(meta.country.as_ref())
                    .or(present(location.as_ref()))
                    .or(present(meta.venue.as_ref()))
                    .unwrap_or_default()
                    .to_string();
                targets.push(ScopeTarget {
                    scope: AliasScope::SportLocation,
                    keys: places.into_iter().map(|p| format!("place:{p}")).collect(),
                    label,
                });
            }
            targets
        }
        MediaWant::SportSeason { league_name, .. } => vec![league_target(league_name)],
        MediaWant::Comic { .. }
        | MediaWant::Music { .. }
        | MediaWant::Game { .. }
        | MediaWant::Software { .. } => Vec::new(),
    }
}

fn league_target(league_name: &str) -> ScopeTarget {
    ScopeTarget {
        scope: AliasScope::SportLeague,
        keys: vec![format!("league:{}", canonical_sport_text(league_name))],
        label: league_name.to_string(),
    }
}

fn push_unique(list: &mut Option<Vec<String>>, alias: &str) {
    let list = list.get_or_insert_with(Vec::new);
    if !list.iter().any(|a| a.eq_ignore_ascii_case(alias)) {
        list.push(alias.to_string());
    }
}

/// Merge the aliases that apply to `want` into its alias lists. Returns
/// whether any did.
pub fn merge(want: &mut MediaWant, aliases: &[LearnedAliasRow]) -> bool {
    let targets = scope_targets(want);
    let applicable: Vec<(AliasScope, &str)> = aliases
        .iter()
        .filter_map(|a| {
            let scope = AliasScope::parse(&a.scope)?;
            targets
                .iter()
                .any(|t| t.scope == scope && t.keys.contains(&a.scope_key))
                .then_some((scope, a.alias.as_str()))
        })
        .collect();
    for &(scope, alias) in &applicable {
        match (scope, &mut *want) {
            (
                AliasScope::TvShow,
                MediaWant::TvEpisode {
                    alternate_titles, ..
                }
                | MediaWant::TvSeason {
                    alternate_titles, ..
                },
            )
            | (
                AliasScope::Title,
                MediaWant::Movie {
                    alternate_titles, ..
                }
                | MediaWant::Book {
                    alternate_titles, ..
                }
                | MediaWant::Audiobook {
                    alternate_titles, ..
                },
            ) => push_unique(alternate_titles, alias),
            (AliasScope::SportWant, MediaWant::SportEvent { meta, .. }) => {
                push_unique(&mut meta.event_aliases, alias);
            }
            (AliasScope::SportLocation, MediaWant::SportEvent { meta, .. }) => {
                push_unique(&mut meta.location_aliases, alias);
            }
            (AliasScope::SportLeague, MediaWant::SportEvent { meta, .. }) => {
                push_unique(&mut meta.league_aliases, alias);
            }
            (AliasScope::SportLeague, MediaWant::SportSeason { league_aliases, .. }) => {
                push_unique(league_aliases, alias);
            }
            _ => {}
        }
    }
    !applicable.is_empty()
}

#[derive(Debug, thiserror::Error)]
pub enum RememberError {
    #[error("a {0:?} alias does not apply to this want")]
    ScopeNotApplicable(AliasScope),
    #[error("the alias is empty")]
    Empty,
    #[error(transparent)]
    Repository(#[from] crate::repositories::RepositoryError),
}

pub async fn remember(
    pool: &PgPool,
    want: &MediaWant,
    scope: AliasScope,
    alias: &str,
) -> Result<LearnedAliasRow, RememberError> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err(RememberError::Empty);
    }
    let target = scope_targets(want)
        .into_iter()
        .find(|t| t.scope == scope)
        .ok_or(RememberError::ScopeNotApplicable(scope))?;
    Ok(repo::insert(
        pool,
        &repo::NewLearnedAlias {
            scope: scope.as_str(),
            scope_key: &target.keys[0],
            scope_label: &target.label,
            alias,
        },
    )
    .await?)
}

/// Merge the stored aliases for `want` into it. Every path that scores a
/// want calls this once on the want it derives targets and gates from.
pub async fn apply(pool: &PgPool, want: &mut MediaWant) {
    let keys: Vec<String> = scope_targets(want)
        .into_iter()
        .flat_map(|t| t.keys)
        .collect();
    if keys.is_empty() {
        return;
    }
    match repo::for_keys(pool, &keys).await {
        Ok(aliases) => {
            merge(want, &aliases);
        }
        Err(e) => warn!(error = %e, "Failed to load learned aliases"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(scope: AliasScope, key: &str, alias: &str) -> LearnedAliasRow {
        LearnedAliasRow {
            id: uuid::Uuid::nil(),
            scope: scope.as_str().to_string(),
            scope_key: key.to_string(),
            scope_label: String::new(),
            alias: alias.to_string(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn sport() -> MediaWant {
        serde_json::from_value(serde_json::json!({
            "kind": "sport_event", "league_name": "MotoGP", "season": "2026", "round": 16,
            "location": "Japan", "session_type": "Race", "country_iso": "JP",
            "quality_profile_id": null, "quality_overrides": null,
        }))
        .unwrap()
    }

    #[test]
    fn sport_aliases_land_in_the_list_their_scope_feeds() {
        let mut want = sport();
        let targets = scope_targets(&want);
        let key = |scope| {
            targets
                .iter()
                .find(|t| t.scope == scope)
                .unwrap()
                .keys
                .last()
                .unwrap()
                .clone()
        };
        assert_eq!(key(AliasScope::SportLocation), "place:japan");
        assert!(merge(
            &mut want,
            &[
                row(
                    AliasScope::SportWant,
                    &key(AliasScope::SportWant),
                    "Motul GP"
                ),
                row(
                    AliasScope::SportLeague,
                    &key(AliasScope::SportLeague),
                    "MGP"
                ),
                row(AliasScope::SportLocation, "place:japan", "Nippon"),
                row(AliasScope::SportLocation, "place:qatar", "Lusail"),
            ],
        ));
        let MediaWant::SportEvent { meta, .. } = want else {
            unreachable!()
        };
        assert_eq!(meta.event_aliases, Some(vec!["Motul GP".to_string()]));
        assert_eq!(meta.league_aliases, Some(vec!["MGP".to_string()]));
        assert_eq!(meta.location_aliases, Some(vec!["Nippon".to_string()]));
    }

    #[test]
    fn a_tv_alias_stored_by_tvdb_id_reaches_alternate_titles_once() {
        let mut want = MediaWant::TvEpisode {
            show_name: "Kitsune Days".into(),
            season: "1".into(),
            episode: "2".into(),
            year: None,
            air_date: None,
            identifiers: Some(nats_jobs::TvIdentifiers {
                tmdb_id: None,
                tvdb_id: Some("42".into()),
                imdb_id: None,
            }),
            alternate_titles: Some(vec!["Renard Days".into()]),
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        merge(
            &mut want,
            &[
                row(AliasScope::TvShow, "tvdb:42", "renard days"),
                row(AliasScope::TvShow, "show:kitsune days", "Kitsune Nichi"),
                row(AliasScope::TvShow, "tvdb:43", "Other"),
            ],
        );
        let MediaWant::TvEpisode {
            alternate_titles, ..
        } = want
        else {
            unreachable!()
        };
        assert_eq!(
            alternate_titles,
            Some(vec!["Renard Days".to_string(), "Kitsune Nichi".to_string()])
        );
    }
}
