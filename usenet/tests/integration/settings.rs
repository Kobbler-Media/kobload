use std::collections::HashMap;
use std::path::PathBuf;

use kobload_usenet::{Settings, SettingsError};

const CONTROL: &str = "c0ntrol-key-c0ntrol-key-c0ntrol-key";
const STREAM: &str = "stream-secret-stream-secret-stream";
const METRICS: &str = "metrics-key-metrics-key-metrics-key";

fn settings(vars: &[(&str, &str)]) -> Result<Settings, SettingsError> {
    let mut env: HashMap<&str, &str> = HashMap::from([
        ("KOBLOAD_URL", "http://api:7080"),
        ("USENET_CONTROL_KEY", CONTROL),
        ("USENET_STREAM_SECRET", STREAM),
    ]);
    env.extend(vars.iter().copied());
    Settings::from_lookup(|name| env.get(name).map(|v| (*v).to_owned()))
}

#[test]
fn defaults_warm_four_connections_and_leave_metrics_closed() {
    let default = settings(&[]).unwrap();
    assert_eq!(default.min_connections, 4);
    assert_eq!(default.metrics_key, None);
    let set = settings(&[
        ("USENET_MIN_CONNECTIONS", "8"),
        ("USENET_METRICS_KEY", METRICS),
    ])
    .unwrap();
    assert_eq!(set.min_connections, 8);
    assert_eq!(set.metrics_key.as_deref(), Some(METRICS));
}

#[test]
fn the_stream_trace_is_off_unless_set_to_one() {
    for (value, on) in [
        (None, false),
        (Some("0"), false),
        (Some("yes"), false),
        (Some("1"), true),
    ] {
        let vars: Vec<_> = value
            .map(|v| ("USENET_TRACE_STREAMS", v))
            .into_iter()
            .collect();
        assert_eq!(settings(&vars).unwrap().trace_streams, on);
    }
}

#[test]
fn downloads_default_to_the_shared_mount_one_gb_floor_and_two_active_jobs() {
    let default = settings(&[]).unwrap();
    assert_eq!(default.download_dir, PathBuf::from("/downloads"));
    assert_eq!(default.free_space_floor, 1 << 30);
    assert_eq!(default.active_jobs, 2);
    let set = settings(&[
        ("USENET_DOWNLOAD_DIR", "/mnt/download/nzb/complete"),
        ("USENET_FREE_SPACE_FLOOR_GB", "20"),
        ("USENET_ACTIVE_JOBS", "3"),
    ])
    .unwrap();
    assert_eq!(
        set.download_dir,
        PathBuf::from("/mnt/download/nzb/complete")
    );
    assert_eq!(set.free_space_floor, 20 << 30);
    assert_eq!(set.active_jobs, 3);
}

#[test]
fn weak_or_shared_secrets_refuse_to_start() {
    let refused = [
        vec![("USENET_CONTROL_KEY", "dev-usenet-control-key")],
        vec![("USENET_STREAM_SECRET", "short")],
        vec![("USENET_STREAM_SECRET", CONTROL)],
        vec![("USENET_METRICS_KEY", "metrics")],
        vec![("USENET_METRICS_KEY", CONTROL)],
        vec![("USENET_MIN_CONNECTIONS", "many")],
        vec![("USENET_FREE_SPACE_FLOOR_GB", "-1")],
        vec![("USENET_ACTIVE_JOBS", "0")],
        vec![("USENET_DOWNLOAD_DIR", "relative/dir")],
    ];
    for vars in refused {
        assert!(settings(&vars).is_err(), "{vars:?}");
    }
}
