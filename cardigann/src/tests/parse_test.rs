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

#[test]
fn plain_scalars_keep_their_source_spelling() {
    let def = crate::load_definition(
        r#"
id: t
name: T
caps:
  categorymappings:
    - { id: 1_0, cat: Other }
    - { id: 01, cat: Movies }
    - { id: 7, cat: TV }
search:
  path: /browse
  inputs:
    search: "{{ .Keywords }}"
    incldead: on
    nonboolean: yes
  rows:
    selector: tr
login:
  inputs:
    keeplogged: yes
    remember: off
"#,
    )
    .unwrap();
    let ids: Vec<&str> = def
        .caps
        .categorymappings
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["1_0", "01", "7"]);
    assert_eq!(def.search.inputs["incldead"].as_str(), "on");
    assert_eq!(def.search.inputs["nonboolean"].as_str(), "yes");
    let login = def.login.unwrap();
    assert_eq!(login.inputs["keeplogged"].as_str(), "yes");
    assert_eq!(login.inputs["remember"].as_str(), "off");
}
