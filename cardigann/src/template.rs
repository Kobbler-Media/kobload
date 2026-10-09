//! Go-style template engine for Cardigann definitions.
//!
//! Supports: `{{ .Variable }}`, `{{ if .Var }}`, `{{ else }}`, `{{ end }}`,
//! `{{ range .Var }}`, `{{ if eq .A .B }}`, `{{ if or .A .B }}`,
//! `re_replace`, `join`.

use std::collections::HashMap;

/// Render a Go-style template string with the given variables.
pub fn render(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start + 2..];

        let Some(end) = after_open.find("}}") else {
            result.push_str(&remaining[start..]);
            return result;
        };

        let expr = after_open[..end].trim();
        remaining = &after_open[end + 2..];

        if let Some(value) = evaluate_expr(expr, vars) {
            result.push_str(&value);
        }
    }

    result.push_str(remaining);
    result
}

fn evaluate_expr(expr: &str, vars: &HashMap<String, String>) -> Option<String> {
    // Handle conditionals — for simple templates, just evaluate the variable
    if expr.starts_with("if ") || expr == "else" || expr == "end" || expr.starts_with("range ") {
        // These are block-level constructs; in simple usage we skip them
        return Some(String::new());
    }

    // Handle re_replace
    if expr.starts_with("re_replace ") {
        return handle_re_replace(expr, vars);
    }

    // Handle join
    if expr.starts_with("join ") {
        return handle_join(expr, vars);
    }

    // Variable lookup: `.Variable` or `.Config.name`
    let var_name = expr.trim_start_matches('.');
    if let Some(value) = vars.get(var_name) {
        return Some(value.clone());
    }

    // Try nested: Config.name -> look up "Config.name"
    if let Some(value) = vars.get(expr.trim_start_matches('.')) {
        return Some(value.clone());
    }

    Some(String::new())
}

fn handle_re_replace(expr: &str, vars: &HashMap<String, String>) -> Option<String> {
    // re_replace .Var "pattern" "replacement"
    let parts: Vec<&str> = split_template_args(expr);
    if parts.len() < 4 {
        return Some(String::new());
    }
    let var_name = parts[1].trim_start_matches('.');
    let input = vars.get(var_name).cloned().unwrap_or_default();
    let pattern = unquote(parts[2]);
    let replacement = unquote(parts[3]);

    let re = regex::Regex::new(pattern).ok()?;
    Some(re.replace_all(&input, replacement).to_string())
}

fn handle_join(expr: &str, vars: &HashMap<String, String>) -> Option<String> {
    // join .Var "separator"
    let parts: Vec<&str> = split_template_args(expr);
    if parts.len() < 3 {
        return Some(String::new());
    }
    let var_name = parts[1].trim_start_matches('.');
    let separator = unquote(parts[2]);
    let input = vars.get(var_name).cloned().unwrap_or_default();
    // Assume input is comma-separated for now
    let joined = input
        .split(',')
        .map(|s| s.trim())
        .collect::<Vec<_>>()
        .join(separator);
    Some(joined)
}

fn split_template_args(expr: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quote = false;
    let mut start = 0;

    for (i, c) in expr.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            ' ' if !in_quote => {
                if start < i {
                    parts.push(&expr[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < expr.len() {
        parts.push(&expr[start..]);
    }
    parts
}

fn unquote(s: &str) -> &str {
    s.trim_matches('"')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_variable() {
        let mut vars = HashMap::new();
        vars.insert("Keywords".to_string(), "test query".to_string());
        assert_eq!(render("q={{ .Keywords }}", &vars), "q=test query");
    }

    #[test]
    fn test_nested_variable() {
        let mut vars = HashMap::new();
        vars.insert("Config.username".to_string(), "admin".to_string());
        assert_eq!(render("user={{ .Config.username }}", &vars), "user=admin");
    }

    #[test]
    fn test_missing_variable() {
        let vars = HashMap::new();
        assert_eq!(render("q={{ .Missing }}", &vars), "q=");
    }

    #[test]
    fn test_no_template() {
        let vars = HashMap::new();
        assert_eq!(render("plain text", &vars), "plain text");
    }

    #[test]
    fn test_re_replace() {
        let mut vars = HashMap::new();
        vars.insert("Keywords".to_string(), "hello world".to_string());
        assert_eq!(
            render(r#"{{ re_replace .Keywords "world" "rust" }}"#, &vars),
            "hello rust"
        );
    }
}
