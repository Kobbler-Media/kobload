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
    unvouched: bool,
    open_risk: u8,
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
            unvouched: !(self.likely || certain),
            open_risk: if certain { 0 } else { self.risk },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cached() -> RankFacts {
        RankFacts {
            group: StreamGroup::Available,
            hardcoded: false,
            language_missing: false,
            suspect: false,
            continuity: continuity::Rank::None,
            risk: 0,
            ready: false,
            likely: true,
        }
    }

    fn unknown() -> RankFacts {
        RankFacts {
            likely: false,
            ..cached()
        }
    }

    fn ready() -> RankFacts {
        RankFacts {
            ready: true,
            likely: false,
            ..cached()
        }
    }

    #[test]
    fn test_full_precedence() {
        let order: Vec<(&str, RankFacts)> = vec![
            (
                "played, even at high risk",
                RankFacts {
                    group: StreamGroup::Played,
                    risk: 3,
                    ..unknown()
                },
            ),
            ("ready or RD+ or engine usenet", cached()),
            ("ready at risk 3", RankFacts { risk: 3, ..ready() }),
            (
                "RD+ or engine usenet at risk 1",
                RankFacts {
                    risk: 1,
                    ..cached()
                },
            ),
            (
                "RD+ or engine usenet at risk 3",
                RankFacts {
                    risk: 3,
                    ..cached()
                },
            ),
            ("unknown torrent", unknown()),
            (
                "unknown torrent at risk",
                RankFacts {
                    risk: 1,
                    ..unknown()
                },
            ),
            (
                "suspected fake, even if cached",
                RankFacts {
                    suspect: true,
                    ..cached()
                },
            ),
            (
                "wrong language, even if cached",
                RankFacts {
                    language_missing: true,
                    ..cached()
                },
            ),
            (
                "hardcoded subs, even if played",
                RankFacts {
                    group: StreamGroup::Played,
                    hardcoded: true,
                    ..unknown()
                },
            ),
            (
                "uncached",
                RankFacts {
                    group: StreamGroup::Uncached,
                    ..unknown()
                },
            ),
            (
                "failed",
                RankFacts {
                    group: StreamGroup::Failed,
                    ..unknown()
                },
            ),
        ];
        for pair in order.windows(2) {
            assert!(
                pair[0].1.key() < pair[1].1.key(),
                "{} must rank above {}",
                pair[0].0,
                pair[1].0
            );
        }
        assert!(
            ready().key() == cached().key(),
            "ready and RD+ compete on score"
        );
    }
}
