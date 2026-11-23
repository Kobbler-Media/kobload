use kobload_api::services::stream::{KnownAvailability, RankCandidate, RankProvider};
use nats_jobs::types::{StreamFile, StreamOption, StreamSourceType};

pub async fn create_provider(pool: &sqlx::PgPool, provider_type: &str) -> RankProvider {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO debrid_providers (name, provider_type, api_key)
           VALUES ('test', $1, 'k') RETURNING id"#,
        provider_type
    )
    .fetch_one(pool)
    .await
    .expect("create provider");
    RankProvider { id }
}

// Ingest refuses anything but a real info hash, and tests name theirs by any letter.
pub fn hash(c: char) -> String {
    if c.is_ascii_hexdigit() {
        return c.to_string().repeat(40);
    }
    format!("{:02x}", c as u32 & 0xff).repeat(20)
}

pub fn option(provider: &RankProvider, release_key: &str, title: &str, score: f64) -> StreamOption {
    StreamOption {
        id: format!("torrent_{release_key}_{}", provider.id),
        release_title: title.to_string(),
        size: 2_000_000_000,
        indexer: "torrentio".to_string(),
        release_key: Some(release_key.to_string()),
        nzb_ref: None,
        magnet_url: None,
        quality: None,
        total_score: score,
        match_score: score,
        quality_score: 0.0,
        debrid_provider: "real_debrid".to_string(),
        debrid_provider_id: provider.id,
        cached: false,
        source_type: StreamSourceType::Torrent,
        files: Vec::new(),
        publish_date: None,
        binge_group: None,
        playback: None,
        reported_bad: false,
        availability: None,
        group: None,
        audio_languages: Vec::new(),
    }
}

pub fn with_file(mut option: StreamOption, file_id: i32) -> StreamOption {
    option.id = format!("{}_f{file_id}", option.id);
    option.files = vec![StreamFile {
        id: file_id,
        path: format!("E{file_id}.mkv"),
        size: 2_000_000_000,
    }];
    option
}

pub fn unknown(options: &[StreamOption]) -> Vec<RankCandidate> {
    options
        .iter()
        .cloned()
        .map(|option| RankCandidate {
            option,
            availability: KnownAvailability::Unknown,
        })
        .collect()
}
