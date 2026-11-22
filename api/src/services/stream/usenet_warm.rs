use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use uuid::Uuid;

use nats_jobs::NzbRef;
use nats_jobs::types::{StreamAvailability, StreamOption, StreamPlayback, StreamSourceType};

/// Each warm import of an NZB we do not hold yet costs an indexer grab, and a prewarm burst
/// walks about thirteen releases. So only the option a search would auto-play is imported
/// ahead of a tap; the rest of the walk waits for one.
const TTL: Duration = Duration::from_secs(30 * 60);

static AUTO_PLAY: LazyLock<DashMap<(Uuid, NzbRef), Instant>> = LazyLock::new(DashMap::new);

// Mirrors kobbler's `stream_prewarm::pickable`: the first option that may start now and
// has not failed is the one its prewarm resolves.
fn pickable(option: &StreamOption) -> bool {
    let available = option.cached
        || matches!(
            option.availability,
            Some(StreamAvailability::Ready | StreamAvailability::Likely)
        );
    available && option.playback != Some(StreamPlayback::Failed)
}

pub fn remember_auto_play(options: &[StreamOption]) {
    AUTO_PLAY.retain(|_, at| at.elapsed() < TTL);
    let Some(pick) = options.iter().find(|o| pickable(o)) else {
        return;
    };
    if pick.source_type != StreamSourceType::Usenet {
        return;
    }
    let Some(nzb_ref) = &pick.nzb_ref else {
        return;
    };
    AUTO_PLAY.insert((pick.debrid_provider_id, nzb_ref.clone()), Instant::now());
}

pub fn may_warm(provider_id: Uuid, nzb_ref: &NzbRef) -> bool {
    AUTO_PLAY
        .get(&(provider_id, nzb_ref.clone()))
        .is_some_and(|at| at.elapsed() < TTL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn option(guid: &str, source: &str, extra: Value) -> StreamOption {
        let mut raw = json!({
            "id": guid, "release_title": guid, "size": 1, "indexer": "idx",
            "total_score": 1.0, "match_score": 1.0, "quality_score": 1.0,
            "debrid_provider": "usenet", "debrid_provider_id": Uuid::nil(),
            "cached": false, "source_type": source,
            "nzb_ref": { "indexer_id": "idx", "guid": guid },
        });
        raw.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(raw).unwrap()
    }

    fn picked(options: &[StreamOption]) -> Vec<&str> {
        remember_auto_play(options);
        options
            .iter()
            .filter(|o| may_warm(o.debrid_provider_id, o.nzb_ref.as_ref().unwrap()))
            .map(|o| o.id.as_str())
            .collect()
    }

    #[test]
    fn the_warm_gate_admits_the_option_kobblers_prewarm_picks() {
        let cases: [(&str, Vec<StreamOption>, Vec<&str>); 4] = [
            (
                "a failed top pick is skipped",
                vec![
                    option(
                        "l6-failed",
                        "usenet",
                        json!({ "availability": "ready", "playback": "failed" }),
                    ),
                    option("l6-next", "usenet", json!({ "availability": "likely" })),
                ],
                vec!["l6-next"],
            ),
            (
                "an unknown top pick is skipped",
                vec![
                    option("l6-unknown", "usenet", json!({ "availability": "unknown" })),
                    option("l6-cached", "usenet", json!({ "cached": true })),
                ],
                vec!["l6-cached"],
            ),
            (
                "a torrent pick admits no usenet grab",
                vec![
                    option("l6-torrent", "torrent", json!({ "availability": "ready" })),
                    option("l6-behind", "usenet", json!({ "availability": "likely" })),
                ],
                vec![],
            ),
            (
                "nothing pickable admits nothing",
                vec![option(
                    "l6-dead",
                    "usenet",
                    json!({ "availability": "uncached" }),
                )],
                vec![],
            ),
        ];
        for (label, options, expected) in cases {
            assert_eq!(picked(&options), expected, "{label}");
        }
    }
}
