use serde_json::json;

pub fn motogp_race_want() -> nats_jobs::MediaWant {
    serde_json::from_value(json!({
        "kind": "sport_event",
        "league_name": "MotoGP",
        "season": "2026",
        "round": 16,
        "location": "Japan",
        "session_type": "Race",
        "event_date": "2026-10-04",
        "quality_profile_id": null,
        "quality_overrides": null,
        "venue": "Mobility Resort Motegi",
        "city": "Motegi",
        "region": "Tochigi",
        "country": "Japan",
        "country_iso": "JP",
        "class": "MotoGP",
        "location_aliases": ["Japan", "Mobility Resort Motegi", "Motegi", "Tochigi", "JP", "JPN", "Japanese"],
        "session_start": "2026-10-04T05:00:00+00:00",
    }))
    .unwrap()
}
