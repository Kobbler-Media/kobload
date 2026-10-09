use std::collections::HashMap;

use crate::template::render;

#[test]
fn test_multiple_variables() {
    let mut vars = HashMap::new();
    vars.insert("Keywords".to_string(), "test".to_string());
    vars.insert("Config.apikey".to_string(), "abc123".to_string());
    assert_eq!(
        render("q={{ .Keywords }}&apikey={{ .Config.apikey }}", &vars),
        "q=test&apikey=abc123"
    );
}

#[test]
fn test_empty_template() {
    let vars = HashMap::new();
    assert_eq!(render("", &vars), "");
}

#[test]
fn test_unclosed_brace() {
    let vars = HashMap::new();
    assert_eq!(render("{{ .Missing", &vars), "{{ .Missing");
}
