//! Bulk parse test for Prowlarr YAML definitions.
//!
//! Set `CARDIGANN_TEST_DEFS_DIR` to a directory of .yml definitions to test.
//! Skipped if the env var is unset.

#[test]
fn bulk_parse_definitions() {
    let dir = match std::env::var("CARDIGANN_TEST_DEFS_DIR") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("CARDIGANN_TEST_DEFS_DIR not set, skipping bulk parse test");
            return;
        }
    };

    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut errors: Vec<(String, String)> = Vec::new();

    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let yaml = std::fs::read_to_string(&path).unwrap();
        match crate::load_definition(&yaml) {
            Ok(_) => ok += 1,
            Err(e) => {
                errors.push((
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    e.to_string(),
                ));
                fail += 1;
            }
        }
    }

    eprintln!("Parsed {ok} OK, {fail} failed out of {} total", ok + fail);
    for (file, err) in &errors {
        // Truncate error for readability
        let short = if err.len() > 120 { &err[..120] } else { err };
        eprintln!("  FAIL {file}: {short}");
    }

    let total = ok + fail;
    assert!(total > 0, "no .yml files found in {dir}");
    // Allow up to 10% failures
    let max_fail = total / 10;
    assert!(
        fail <= max_fail,
        "{fail} definitions failed to parse (max allowed: {max_fail})"
    );
}
