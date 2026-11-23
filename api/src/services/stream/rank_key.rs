use super::continuity;
use nats_jobs::types::StreamGroup;

// A copy on the account cannot fail to start, so it wins a close call against a hint, but
// not against a clearly better copy.
const READY_EDGE: f64 = 0.1;

pub struct RankFacts {
    pub group: StreamGroup,
    pub hardcoded: bool,
    pub language_missing: bool,
    pub suspect: bool,
    pub continuity: continuity::Rank,
    pub risk: u8,
    pub ready: bool,
    pub likely: bool,
}

/// In the available tier a ready copy skips the `open_*` checks, which ask whether a release
/// is worth trying: it stays above every unvouched release, and its real language, fit and
/// risk only count against other cached copies. Continuity sits below the vouched check there
/// for the same reason; any other order is cyclic once ready and RD+ compete on score.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct RankKey {
    cannot_start: bool,
    hardcoded: bool,
    group: StreamGroup,
    open_language: bool,
    open_suspect: bool,
    outside_tier_continuity: continuity::Rank,
    open_risk: u8,
    unvouched: bool,
    language_missing: bool,
    suspect: bool,
    continuity: continuity::Rank,
    risk: u8,
}

impl RankFacts {
    fn in_cached_tier(&self) -> bool {
        self.group == StreamGroup::Available
    }

    pub fn key(&self) -> RankKey {
        let tier = self.in_cached_tier();
        let certain = tier && self.ready;
        RankKey {
            cannot_start: self.group >= StreamGroup::Uncached,
            hardcoded: self.hardcoded,
            group: self.group,
            open_language: self.language_missing && !certain,
            open_suspect: self.suspect && !certain,
            outside_tier_continuity: if tier {
                continuity::Rank::None
            } else {
                self.continuity
            },
            open_risk: if certain { 0 } else { self.risk },
            unvouched: !(self.likely || certain),
            language_missing: self.language_missing,
            suspect: self.suspect,
            continuity: self.continuity,
            risk: self.risk,
        }
    }

    pub fn score_edge(&self) -> f64 {
        if self.in_cached_tier() && self.ready {
            READY_EDGE
        } else {
            0.0
        }
    }
}
