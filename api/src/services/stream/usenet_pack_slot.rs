use crate::parsing::{ParsedMedia, parse_release};
use nats_jobs::MediaWant;
use nats_jobs::types::{StreamOption, StreamSourceType};

/// Packs rank below single-episode releases, so on a popular show a full page of singles
/// would always crowd them out, and a single that fails to stream would leave no pack to
/// fall back to.
pub fn truncate(options: &mut Vec<StreamOption>, limit: usize, want: Option<&MediaWant>) {
    if limit > 0
        && options.len() > limit
        && matches!(want, Some(MediaWant::TvEpisode { .. }))
        && !options[..limit].iter().any(is_usenet_pack)
        && let Some(pack) = options[limit..].iter().position(is_usenet_pack)
    {
        let slot = options[..limit]
            .iter()
            .rposition(|o| o.source_type == StreamSourceType::Usenet)
            .unwrap_or(limit - 1);
        options.swap(slot, limit + pack);
    }
    options.truncate(limit);
}

fn is_usenet_pack(option: &StreamOption) -> bool {
    option.source_type == StreamSourceType::Usenet
        && matches!(parse_release(&option.release_title, "tv").media, ParsedMedia::Tv(tv) if tv.is_pack)
}
