//! Filter chain for Cardigann field extraction.
//!
//! Filters transform extracted values through a pipeline.

use chrono::{NaiveDateTime, Utc};
use regex::Regex;

use crate::definition::FilterDef;

/// Apply a chain of filters to a value.
pub fn apply_filters(value: &str, filters: &[FilterDef]) -> String {
    let mut result = value.to_string();
    for filter in filters {
        result = apply_filter(&result, filter);
    }
    result
}

fn apply_filter(value: &str, filter: &FilterDef) -> String {
    match filter.name.as_str() {
        "regexp" => filter_regexp(value, filter.args.as_str()),
        "re_replace" => {
            let args = filter.args.as_list();
            if args.len() >= 2 {
                filter_re_replace(value, args[0], args[1])
            } else {
                value.to_string()
            }
        }
        "replace" => {
            let args = filter.args.as_list();
            if args.len() >= 2 {
                value.replace(args[0], args[1])
            } else {
                value.to_string()
            }
        }
        "split" => {
            let args = filter.args.as_list();
            let sep = args.first().copied().unwrap_or(" ");
            let idx: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            value.split(sep).nth(idx).unwrap_or("").trim().to_string()
        }
        "trim" => value.trim().to_string(),
        "prepend" => format!("{}{}", filter.args.as_str(), value),
        "append" => format!("{}{}", value, filter.args.as_str()),
        "tolower" => value.to_lowercase(),
        "toupper" => value.to_uppercase(),
        "urlencode" => url::form_urlencoded::byte_serialize(value.as_bytes()).collect(),
        "urldecode" => url::form_urlencoded::parse(value.as_bytes())
            .map(|(k, v)| {
                if v.is_empty() {
                    k.to_string()
                } else {
                    format!("{k}={v}")
                }
            })
            .collect::<Vec<_>>()
            .join("&"),
        "htmldecode" => html_decode(value),
        "dateparse" | "timeparse" => filter_dateparse(value, filter.args.as_str()),
        "timeago" | "fuzzytime" => filter_timeago(value),
        "querystring" => filter_querystring(value, filter.args.as_str()),
        "validfilename" => filter_validfilename(value),
        "diacritics" => filter_diacritics(value),
        "validate" => {
            if url::Url::parse(value).is_ok() {
                value.to_string()
            } else {
                String::new()
            }
        }
        _ => {
            tracing::warn!(filter = %filter.name, "Unknown filter");
            value.to_string()
        }
    }
}

fn filter_regexp(value: &str, pattern: &str) -> String {
    let Ok(re) = Regex::new(pattern) else {
        return value.to_string();
    };
    re.captures(value)
        .and_then(|caps| caps.get(1).or(caps.get(0)))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

fn filter_re_replace(value: &str, pattern: &str, replacement: &str) -> String {
    let Ok(re) = Regex::new(pattern) else {
        return value.to_string();
    };
    re.replace_all(value, replacement).to_string()
}

fn filter_dateparse(value: &str, format: &str) -> String {
    if format.is_empty() {
        // Try common formats
        for fmt in &[
            "%Y-%m-%d %H:%M:%S",
            "%d-%m-%Y %H:%M:%S",
            "%b %d, %Y",
            "%d %b %Y",
        ] {
            if let Ok(dt) = NaiveDateTime::parse_from_str(value.trim(), fmt) {
                return dt.and_utc().to_rfc3339();
            }
        }
        return value.to_string();
    }

    // Convert Go time format to chrono format (basic mapping)
    let chrono_fmt = go_time_to_chrono(format);
    NaiveDateTime::parse_from_str(value.trim(), &chrono_fmt)
        .map(|dt| dt.and_utc().to_rfc3339())
        .unwrap_or_else(|_| value.to_string())
}

fn filter_timeago(value: &str) -> String {
    let lower = value.to_lowercase();
    let now = Utc::now();

    // Parse relative time strings like "2 hours ago", "1 day ago", "just now"
    if lower.contains("just now") || lower.contains("now") {
        return now.to_rfc3339();
    }

    let re = Regex::new(r"(\d+)\s*(second|minute|hour|day|week|month|year)s?\s*ago").ok();
    if let Some(re) = re {
        if let Some(caps) = re.captures(&lower) {
            let amount: i64 = caps[1].parse().unwrap_or(0);
            let duration = match &caps[2] {
                "second" => chrono::Duration::seconds(amount),
                "minute" => chrono::Duration::minutes(amount),
                "hour" => chrono::Duration::hours(amount),
                "day" => chrono::Duration::days(amount),
                "week" => chrono::Duration::weeks(amount),
                "month" => chrono::Duration::days(amount * 30),
                "year" => chrono::Duration::days(amount * 365),
                _ => return value.to_string(),
            };
            return (now - duration).to_rfc3339();
        }
    }

    value.to_string()
}

fn filter_querystring(value: &str, param: &str) -> String {
    // Extract a query parameter from a URL
    url::Url::parse(value)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(k, _)| k == param)
                .map(|(_, v)| v.to_string())
        })
        .unwrap_or_default()
}

fn filter_validfilename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn filter_diacritics(value: &str) -> String {
    // Basic diacritics removal — strips combining marks
    value
        .chars()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .collect()
}

fn html_decode(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Convert Go time format to chrono strftime format (basic).
fn go_time_to_chrono(go_fmt: &str) -> String {
    go_fmt
        .replace("2006", "%Y")
        .replace("06", "%y")
        .replace("January", "%B")
        .replace("Jan", "%b")
        .replace("01", "%m")
        .replace("1", "%m")
        .replace("02", "%d")
        .replace("2", "%d")
        .replace("15", "%H")
        .replace("03", "%I")
        .replace("3", "%I")
        .replace("04", "%M")
        .replace("4", "%M")
        .replace("05", "%S")
        .replace("5", "%S")
        .replace("PM", "%p")
        .replace("pm", "%P")
        .replace("MST", "%Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regexp() {
        assert_eq!(filter_regexp("Size: 1.5 GB", r"Size:\s*(.+)"), "1.5 GB");
    }

    #[test]
    fn test_re_replace() {
        assert_eq!(
            filter_re_replace("hello world", "world", "rust"),
            "hello rust"
        );
    }

    #[test]
    fn test_split() {
        let filter = FilterDef {
            name: "split".to_string(),
            args: crate::definition::FilterArgs::List(vec![
                crate::definition::YamlString("|".to_string()),
                crate::definition::YamlString("1".to_string()),
            ]),
        };
        assert_eq!(apply_filter("a|b|c", &filter), "b");
    }

    #[test]
    fn test_prepend_append() {
        let filter = FilterDef {
            name: "prepend".to_string(),
            args: crate::definition::FilterArgs::Single(crate::definition::YamlString(
                "https://example.com".to_string(),
            )),
        };
        assert_eq!(apply_filter("/path", &filter), "https://example.com/path");
    }

    #[test]
    fn test_querystring() {
        assert_eq!(
            filter_querystring("https://example.com/dl?id=123&key=abc", "id"),
            "123"
        );
    }

    #[test]
    fn test_html_decode() {
        assert_eq!(html_decode("a &amp; b &lt; c"), "a & b < c");
    }

    #[test]
    fn test_timeago() {
        let result = filter_timeago("5 hours ago");
        assert!(!result.is_empty());
        assert!(result.contains('T')); // Should be a datetime
    }
}
