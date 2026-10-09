//! HTML/JSON/XML extraction via CSS selectors, JSONPath, or XPath-like queries.

use scraper::Selector;

use crate::definition::{FieldDef, FieldDefComplex};
use crate::filters::apply_filters;

/// Extract a field value from an HTML element using a field definition.
pub fn extract_field_html(
    row: &scraper::ElementRef<'_>,
    field: &FieldDef,
    base_url: &str,
) -> Option<String> {
    match field {
        FieldDef::Simple(selector_str) => {
            let value = select_text(row, selector_str)?;
            Some(value.trim().to_string())
        }
        FieldDef::Complex(complex) => extract_complex_html(row, complex, base_url),
    }
}

fn extract_complex_html(
    row: &scraper::ElementRef<'_>,
    def: &FieldDefComplex,
    base_url: &str,
) -> Option<String> {
    // Handle case blocks (test selectors, return matching value)
    if let Some(cases) = &def.case {
        for (selector_str, value) in cases {
            if let Ok(sel) = Selector::parse(selector_str) {
                if row.select(&sel).next().is_some() {
                    return Some(value.as_str().to_string());
                }
            }
        }
        return Some(String::new());
    }

    // Static text
    if let Some(text) = &def.text {
        let result = apply_filters(text.as_str(), &def.filters);
        return Some(result);
    }

    let selector_str = def.selector.as_deref()?;

    let value = if let Some(attr) = &def.attribute {
        select_attribute(row, selector_str, attr)?
    } else {
        select_text(row, selector_str)?
    };

    let mut result = value.trim().to_string();

    // Apply absolute URL resolution for link-like attributes
    if let Some(attr) = &def.attribute {
        if (attr == "href" || attr == "src") && !result.starts_with("http") {
            result = resolve_url(base_url, &result);
        }
    }

    result = apply_filters(&result, &def.filters);

    if result.is_empty() && def.optional == Some(true) {
        return None;
    }

    Some(result)
}

fn select_text(el: &scraper::ElementRef<'_>, selector_str: &str) -> Option<String> {
    let sel = Selector::parse(selector_str).ok()?;
    let found = el.select(&sel).next()?;
    Some(found.text().collect::<String>())
}

fn select_attribute(
    el: &scraper::ElementRef<'_>,
    selector_str: &str,
    attribute: &str,
) -> Option<String> {
    let sel = Selector::parse(selector_str).ok()?;
    let found = el.select(&sel).next()?;
    found.value().attr(attribute).map(|s| s.to_string())
}

fn resolve_url(base: &str, relative: &str) -> String {
    url::Url::parse(base)
        .ok()
        .and_then(|base_url| base_url.join(relative).ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| format!("{}{}", base.trim_end_matches('/'), relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scraper::Html;

    #[test]
    fn test_select_text_from_html() {
        let html =
            Html::parse_fragment("<table><tr><td class='title'>Test Title</td></tr></table>");
        let sel = Selector::parse("tr").unwrap();
        let row = html.select(&sel).next().unwrap();
        let result = select_text(&row, "td.title");
        assert_eq!(result, Some("Test Title".to_string()));
    }

    #[test]
    fn test_select_attribute() {
        let html = Html::parse_fragment(
            "<table><tr><td><a href='/download/123'>Link</a></td></tr></table>",
        );
        let sel = Selector::parse("tr").unwrap();
        let row = html.select(&sel).next().unwrap();
        let result = select_attribute(&row, "a", "href");
        assert_eq!(result, Some("/download/123".to_string()));
    }

    #[test]
    fn test_resolve_url() {
        assert_eq!(
            resolve_url("https://example.com", "/path"),
            "https://example.com/path"
        );
        assert_eq!(
            resolve_url("https://example.com/base/", "file.torrent"),
            "https://example.com/base/file.torrent"
        );
    }
}
