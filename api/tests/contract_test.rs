//! Contract tests ensuring Kobload can deserialize types shared with Kobbler.
//!
//! These tests use canonical fixtures from koblibs to verify that
//! field renames, additions, or removals are caught immediately.

mod common;

use nats_jobs::test_fixtures;

#[test]
fn kobload_deserializes_want_request() {
    let fixture = test_fixtures::sample_want_request();
    let json = serde_json::to_value(&fixture).unwrap();

    // Kobload deserializes WantRequest in the want_request job handler
    let deserialized: nats_jobs::WantRequest = serde_json::from_value(json).unwrap();
    assert_eq!(deserialized.source_system, "kobbler");
    assert!(!deserialized.patterns.is_empty());
    assert!(deserialized.media_want.is_some());
}

#[test]
fn kobload_deserializes_search_request() {
    let fixture = test_fixtures::sample_search_request();
    let json = serde_json::to_value(&fixture).unwrap();

    let deserialized: nats_jobs::SearchRequest = serde_json::from_value(json).unwrap();
    assert!(!deserialized.query.is_empty());
    assert!(deserialized.media_type.is_some());
}

#[test]
fn kobload_serializes_want_response() {
    // Kobload produces WantResponse — verify it round-trips
    let fixture = test_fixtures::sample_want_response_downloading();
    let json = serde_json::to_value(&fixture).unwrap();

    // Verify the JSON has expected fields for Kobbler to consume
    assert_eq!(json["status"], "downloading");
    assert!(json["download_id"].is_string());
    assert!(json["watch_pattern_id"].is_string());
    assert!(json["metadata"]["book_id"].is_string());
}

#[test]
fn kobload_serializes_search_response() {
    let fixture = test_fixtures::sample_search_response();
    let json = serde_json::to_value(&fixture).unwrap();

    assert!(json["results"].is_array());
    assert_eq!(json["results"][0]["guid"], "abc-123");
    assert!(json["results"][0]["size"].is_number());
}

#[test]
fn kobload_deserializes_watch_pattern_upsert() {
    let fixture = test_fixtures::sample_watch_pattern_upsert();
    let json = serde_json::to_value(&fixture).unwrap();

    let deserialized: nats_jobs::WatchPatternUpsert = serde_json::from_value(json).unwrap();
    assert_eq!(deserialized.source_system, "kobbler");
    assert!(deserialized.enabled);
}

#[test]
fn kobload_deserializes_watch_pattern_delete() {
    let fixture = test_fixtures::sample_watch_pattern_delete();
    let json = serde_json::to_value(&fixture).unwrap();

    let deserialized: nats_jobs::WatchPatternDelete = serde_json::from_value(json).unwrap();
    assert_eq!(deserialized.source_system, "kobbler");
}

#[test]
fn kobload_serializes_download_completed_event() {
    let fixture = test_fixtures::sample_download_completed();
    let json = serde_json::to_value(&fixture).unwrap();

    // Verify the shape matches what Kobbler expects in KobloadCallbackRequest
    assert!(json["download_id"].is_string());
    assert_eq!(json["status"], "completed");
    assert!(json["files"].is_array());
    assert!(json["files"][0]["absolute_path"].is_string());
    assert!(json["files"][0]["size"].is_number());
    assert!(json["files"][0]["extension"].is_string());
}

#[test]
fn kobload_serializes_watch_matched_event() {
    let fixture = test_fixtures::sample_watch_matched_event();
    let json = serde_json::to_value(&fixture).unwrap();

    assert!(json["watch_pattern_id"].is_string());
    assert_eq!(json["action"], "downloaded");
    assert!(json["match_score"].is_f64());
}
