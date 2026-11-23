use crate::parsing::{ParsedMedia, parse_release};
use nats_jobs::MediaWant;
use nats_jobs::types::{StreamGroup, StreamOption, StreamSourceType};

pub const MAX_USENET_OPTIONS: usize = 20;

/// Every usenet option on our engine is "likely", so without a cap they crowd torrents out
/// of the page. Options are taken in rank order, so tiers count before score: a release that
/// played is never cut for a higher-scored one that failed.
///
/// Packs rank below single-episode releases, so on a popular show a full page of singles
/// would always crowd them out, and a single that fails to stream would leave no pack to
/// fall back to.
pub fn truncate(options: &mut Vec<StreamOption>, limit: usize, want: Option<&MediaWant>) {
    if options.len() <= limit {
        return;
    }
    let pack = matches!(want, Some(MediaWant::TvEpisode { .. }))
        .then(|| {
            options
                .iter()
                .position(|o| can_start(o) && is_usenet_pack(o))
        })
        .flatten();

    let mut usenet = 0;
    let (preferred, backfill): (Vec<usize>, Vec<usize>) = (0..options.len()).partition(|&i| {
        let option = &options[i];
        if !can_start(option) {
            return false;
        }
        if option.source_type != StreamSourceType::Usenet {
            return true;
        }
        usenet += 1;
        usenet <= MAX_USENET_OPTIONS || Some(i) == pack
    });
    let mut kept: Vec<usize> = preferred.into_iter().chain(backfill).take(limit).collect();

    if let Some(pack) = pack
        && limit > 1
        && !kept.contains(&pack)
    {
        let slot = kept[1..]
            .iter()
            .rposition(|&i| options[i].source_type != StreamSourceType::Usenet)
            .map_or(limit - 1, |p| p + 1);
        kept[slot] = pack;
    }

    let mut keep = vec![false; options.len()];
    for i in kept {
        keep[i] = true;
    }
    let mut flags = keep.into_iter();
    options.retain(|_| flags.next().unwrap_or(false));
}

fn can_start(option: &StreamOption) -> bool {
    option.group.is_none_or(|g| g < StreamGroup::Uncached)
}

fn is_usenet_pack(option: &StreamOption) -> bool {
    option.source_type == StreamSourceType::Usenet
        && matches!(parse_release(&option.release_title, "tv").media, ParsedMedia::Tv(tv) if tv.is_pack)
}
