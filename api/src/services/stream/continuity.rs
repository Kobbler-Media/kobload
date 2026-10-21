use std::sync::LazyLock;

use regex::Regex;

use crate::quality::{ParsedQuality, parse_quality};
use nats_jobs::types::StreamOption;

/// Within close scores, each way a release looks different from the previous episode
/// costs this share of its score.
const STYLE_MARGIN: f64 = 0.05;

static CHANNELS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^0-9])([1-9][. ][0-2])(?:[^0-9]|$)").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    SamePack,
    SameBingeGroup,
    SameReleaseGroup,
    None,
}

/// What a viewer's previous episode looked like: switching resolution, HDR or audio
/// layout mid-binge is noticeable even when the new release scores a little higher.
#[derive(Debug, PartialEq, Eq)]
struct Style {
    resolution: Option<String>,
    hdr: (bool, bool),
    channels: Option<String>,
    atmos: bool,
}

impl Style {
    fn of(title: &str, quality: &ParsedQuality) -> Self {
        let lower = title.to_lowercase();
        Self {
            resolution: quality.resolution.clone(),
            hdr: (
                quality.is_dolby_vision,
                quality.is_hdr || quality.is_hdr10 || quality.is_hdr10plus || quality.is_hlg,
            ),
            channels: CHANNELS_RE.captures(&lower).map(|c| c[1].replace(' ', ".")),
            atmos: quality.is_atmos,
        }
    }

    fn mismatches(&self, other: &Self) -> usize {
        let differs = |a: &Option<String>, b: &Option<String>| a.is_some() && b.is_some() && a != b;
        [
            differs(&self.resolution, &other.resolution),
            self.hdr != other.hdr,
            differs(&self.channels, &other.channels) || self.atmos != other.atmos,
        ]
        .into_iter()
        .filter(|m| *m)
        .count()
    }
}

pub struct Continuity {
    info_hash: String,
    binge_group: Option<String>,
    release_group: Option<String>,
    style: Option<Style>,
}

impl Continuity {
    pub fn new(
        info_hash: String,
        binge_group: Option<String>,
        release_title: Option<&str>,
    ) -> Self {
        let parsed = release_title.map(|t| (t, parse_quality(t)));
        Self {
            info_hash,
            binge_group,
            release_group: parsed
                .as_ref()
                .and_then(|(_, q)| q.release_group.as_deref().map(str::to_lowercase)),
            style: parsed.map(|(t, q)| Style::of(t, &q)),
        }
    }

    pub fn rank(&self, option: &StreamOption, quality: &ParsedQuality) -> Rank {
        let group = quality.release_group.as_deref().map(str::to_lowercase);
        if option
            .info_hash
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(&self.info_hash))
        {
            Rank::SamePack
        } else if self.binge_group.is_some() && option.binge_group == self.binge_group {
            Rank::SameBingeGroup
        } else if self.release_group.is_some() && group == self.release_group {
            Rank::SameReleaseGroup
        } else {
            Rank::None
        }
    }

    pub fn style_penalty(&self, option: &StreamOption, quality: &ParsedQuality) -> f64 {
        self.style.as_ref().map_or(0.0, |previous| {
            let style = Style::of(&option.release_title, quality);
            STYLE_MARGIN * previous.mismatches(&style) as f64
        })
    }
}
