use base64::{Engine as _, engine::general_purpose};
use rand::RngExt;
use sqlx::PgPool;
use tracing::info;

use crate::auth::hash_password;
use crate::repositories::{
    auth as auth_repo, media_types as media_types_repo, quality_profiles as quality_repo,
};

pub async fn bootstrap_admin(
    pool: &PgPool,
    email: &str,
    password: Option<String>,
) -> Result<(), sqlx::Error> {
    let existing = auth_repo::user_count(pool).await.map_err(|e| match e {
        crate::repositories::RepositoryError::Database(e) => e,
        _ => sqlx::Error::RowNotFound,
    })?;
    if existing > 0 {
        return Ok(());
    }

    let password = password.unwrap_or_else(|| {
        let mut raw = [0u8; 18];
        rand::rng().fill(&mut raw[..]);
        general_purpose::URL_SAFE_NO_PAD.encode(raw)
    });

    let password_hash =
        hash_password(&password).map_err(|_| sqlx::Error::Protocol("password hash".into()))?;

    auth_repo::create_user(pool, email, &password_hash)
        .await
        .map_err(|e| match e {
            crate::repositories::RepositoryError::Database(e) => e,
            _ => sqlx::Error::RowNotFound,
        })?;

    info!(
        "Kobload admin created: email={} password={}",
        email, password
    );
    Ok(())
}

// Gap-fill each default rather than bailing when the table is non-empty: a
// single-type backfill migration (e.g. 0077 seeding "sports") otherwise leaves a
// fresh DB with only that row, and the old count>0 guard skipped the rest. Uses
// ON CONFLICT DO NOTHING so user-renamed types are never clobbered.
pub async fn ensure_default_media_types(pool: &PgPool) -> Result<(), sqlx::Error> {
    let types = [
        ("ebooks", "Books"),
        ("audiobooks", "Audiobooks"),
        ("movies", "Movies"),
        ("tv", "TV"),
        ("music", "Music"),
        ("games", "Games"),
        ("software", "Software"),
        ("xxx", "XXX"),
        ("sports", "Sports"),
        ("misc", "Other"),
    ];

    for (key, name) in types {
        let _ = media_types_repo::insert_default(pool, key, name).await;
    }

    Ok(())
}

/// Default rejected sources for TV/movie profiles (configurable by user).
const VIDEO_REJECTED_SOURCES: &[&str] = &["cam", "ts", "telecine"];
const VIDEO_REJECTED_TERMS: &[&str] = &[
    "ASL",
    "BSL",
    ".Signed.",
    "Sign.Zone",
    "SignZone",
    // Audio-described alt tracks — narration mixed over the programme audio.
    // Matching is separator-insensitive, so the dotted form also covers
    // "with Audio Description" and "Audio_Description".
    "Audio.Description",
    "AudioDescription",
    "Audio.Described",
    "3D",
    "HSBS",
    "Half-SBS",
    "HOU",
    "Half-OU",
];

/// Expose the seeded default rejected terms so quality tests can assert the
/// shipped presets actually block what they claim to.
#[cfg(test)]
pub fn default_video_rejected_terms() -> &'static [&'static str] {
    VIDEO_REJECTED_TERMS
}

struct QualityPreset {
    name: &'static str,
    media_kind: &'static str,
    is_default: bool,
    min_resolution: Option<&'static str>,
    max_resolution: Option<&'static str>,
    preferred_sources: &'static [&'static str],
    preferred_codecs: &'static [&'static str],
    preferred_audio: &'static [&'static str],
    preferred_languages: &'static [&'static str],
    rejected_sources: &'static [&'static str],
    rejected_terms: &'static [&'static str],
    reject_hardcoded_subs: bool,
    use_trash_filters: bool,
    reject_remux: bool,
    upgrade_cutoff_score: Option<f64>,
}

const QUALITY_PRESETS: &[QualityPreset] = &[
    QualityPreset {
        name: "TV - Any",
        media_kind: "tv",
        is_default: false,
        min_resolution: None,
        max_resolution: None,
        preferred_sources: &[],
        preferred_codecs: &[],
        preferred_audio: &[],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: false,
        upgrade_cutoff_score: None,
    },
    QualityPreset {
        name: "TV - 720p",
        media_kind: "tv",
        is_default: false,
        min_resolution: None,
        max_resolution: Some("720p"),
        preferred_sources: &["web-dl", "webrip", "hdtv"],
        preferred_codecs: &["x264"],
        preferred_audio: &["aac", "ddp"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: true,
        upgrade_cutoff_score: Some(55.0),
    },
    QualityPreset {
        name: "TV - 1080p",
        media_kind: "tv",
        is_default: true,
        min_resolution: None,
        max_resolution: Some("1080p"),
        preferred_sources: &["web-dl", "webrip", "hdtv"],
        preferred_codecs: &["x265", "x264"],
        preferred_audio: &["aac", "ddp"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: true,
        upgrade_cutoff_score: Some(70.0),
    },
    QualityPreset {
        name: "TV - 4K",
        media_kind: "tv",
        is_default: false,
        min_resolution: None,
        max_resolution: None,
        preferred_sources: &["web-dl", "bluray"],
        preferred_codecs: &["x265", "av1"],
        preferred_audio: &["atmos", "dts", "ddp"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: false,
        upgrade_cutoff_score: Some(85.0),
    },
    QualityPreset {
        name: "TV - Remux",
        media_kind: "tv",
        is_default: false,
        min_resolution: Some("1080p"),
        max_resolution: None,
        preferred_sources: &["bluray"],
        preferred_codecs: &[],
        preferred_audio: &["truehd", "atmos", "dts"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: false,
        upgrade_cutoff_score: None,
    },
    QualityPreset {
        name: "Movies - Any",
        media_kind: "movie",
        is_default: false,
        min_resolution: None,
        max_resolution: None,
        preferred_sources: &[],
        preferred_codecs: &[],
        preferred_audio: &[],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: false,
        upgrade_cutoff_score: None,
    },
    QualityPreset {
        name: "Movies - 720p",
        media_kind: "movie",
        is_default: false,
        min_resolution: None,
        max_resolution: Some("720p"),
        preferred_sources: &["bluray", "web-dl", "webrip"],
        preferred_codecs: &["x264"],
        preferred_audio: &["aac", "ddp"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: true,
        upgrade_cutoff_score: Some(55.0),
    },
    QualityPreset {
        name: "Movies - 1080p",
        media_kind: "movie",
        is_default: true,
        min_resolution: None,
        max_resolution: Some("1080p"),
        preferred_sources: &["bluray", "web-dl", "webrip"],
        preferred_codecs: &["x265", "x264"],
        preferred_audio: &["dts", "ddp"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: true,
        upgrade_cutoff_score: Some(70.0),
    },
    QualityPreset {
        name: "Movies - 4K",
        media_kind: "movie",
        is_default: false,
        min_resolution: None,
        max_resolution: None,
        preferred_sources: &["bluray", "web-dl"],
        preferred_codecs: &["x265", "av1"],
        preferred_audio: &["truehd", "atmos", "dts"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: false,
        upgrade_cutoff_score: Some(85.0),
    },
    QualityPreset {
        name: "Movies - Remux",
        media_kind: "movie",
        is_default: false,
        min_resolution: Some("1080p"),
        max_resolution: None,
        preferred_sources: &["bluray"],
        preferred_codecs: &[],
        preferred_audio: &["truehd", "atmos", "dts"],
        preferred_languages: &["english"],
        rejected_sources: VIDEO_REJECTED_SOURCES,
        rejected_terms: VIDEO_REJECTED_TERMS,
        reject_hardcoded_subs: true,
        use_trash_filters: true,
        reject_remux: false,
        upgrade_cutoff_score: None,
    },
];

pub async fn ensure_default_quality_profiles(pool: &PgPool) -> Result<(), sqlx::Error> {
    let existing = quality_repo::quality_profile_count(pool)
        .await
        .map_err(|e| match e {
            crate::repositories::RepositoryError::Database(e) => e,
            _ => sqlx::Error::RowNotFound,
        })?;
    if existing > 0 {
        return Ok(());
    }

    seed_quality_presets(pool).await;
    Ok(())
}

/// Upsert all quality profile presets and delete any non-preset profiles.
/// Preserves IDs of existing profiles that match by (name, media_kind).
pub async fn seed_quality_presets(pool: &PgPool) {
    let mut kept_ids = Vec::new();
    for preset in QUALITY_PRESETS {
        let input = quality_repo::QualityProfileInput {
            name: preset.name.to_string(),
            media_kind: preset.media_kind.to_string(),
            is_default: preset.is_default,
            min_resolution: preset.min_resolution.map(|s| s.to_string()),
            max_resolution: preset.max_resolution.map(|s| s.to_string()),
            preferred_sources: preset
                .preferred_sources
                .iter()
                .map(|s| s.to_string())
                .collect(),
            preferred_codecs: preset
                .preferred_codecs
                .iter()
                .map(|s| s.to_string())
                .collect(),
            preferred_audio: preset
                .preferred_audio
                .iter()
                .map(|s| s.to_string())
                .collect(),
            preferred_languages: preset
                .preferred_languages
                .iter()
                .map(|s| s.to_string())
                .collect(),
            rejected_languages: Vec::new(),
            rejected_terms: preset
                .rejected_terms
                .iter()
                .map(|s| s.to_string())
                .collect(),
            rejected_sources: preset
                .rejected_sources
                .iter()
                .map(|s| s.to_string())
                .collect(),
            reject_hardcoded_subs: preset.reject_hardcoded_subs,
            reject_remux: preset.reject_remux,
            use_trash_filters: preset.use_trash_filters,
            upgrade_cutoff_score: preset.upgrade_cutoff_score,
        };
        if let Ok(id) = quality_repo::upsert_quality_profile(pool, input).await {
            kept_ids.push(id);
        }
    }
    let _ = quality_repo::delete_quality_profiles_not_in(pool, &kept_ids).await;
}
