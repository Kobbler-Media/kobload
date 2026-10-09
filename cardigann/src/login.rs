//! Authentication flows for Cardigann definitions.
//!
//! Supports: cookie injection, POST/GET credential submission, form-based login.

use std::collections::HashMap;

use reqwest::Client;

use crate::definition::{Definition, Login};
use crate::template;

/// Perform login for a definition, returning cookies or error.
pub async fn perform_login(
    client: &Client,
    def: &Definition,
    config: &HashMap<String, String>,
) -> Result<(), LoginError> {
    let login = match &def.login {
        Some(l) => l,
        None => return Ok(()), // No login required
    };

    let base_url = get_base_url(def, config);
    let method = login.method.as_deref().unwrap_or("post");

    match method {
        "cookie" => login_cookie(login, config),
        "post" | "get" => login_credentials(client, &base_url, login, config, method).await,
        "form" => login_form(client, &base_url, login, config).await,
        _ => Err(LoginError::UnsupportedMethod(method.to_string())),
    }
}

/// Test that login is working by fetching the test page.
pub async fn test_login(
    client: &Client,
    def: &Definition,
    config: &HashMap<String, String>,
) -> Result<(), LoginError> {
    let base_url = get_base_url(def, config);

    let test = match def.login.as_ref().and_then(|l| l.test.as_ref()) {
        Some(t) => t,
        None => return Ok(()),
    };

    let test_url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        test.path.trim_start_matches('/')
    );
    let resp = client
        .get(&test_url)
        .send()
        .await
        .map_err(|e| LoginError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(LoginError::TestFailed(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }

    if let Some(selector) = &test.selector {
        let body = resp
            .text()
            .await
            .map_err(|e| LoginError::Network(e.to_string()))?;
        // Scraper types scoped to block — no awaits after
        check_test_selector(&body, selector)?;
    }

    Ok(())
}

fn check_test_selector(body: &str, selector: &str) -> Result<(), LoginError> {
    let doc = scraper::Html::parse_document(body);
    let sel = scraper::Selector::parse(selector)
        .map_err(|_| LoginError::TestFailed("invalid test selector".to_string()))?;
    if doc.select(&sel).next().is_none() {
        return Err(LoginError::TestFailed(
            "test selector not found — login may have failed".to_string(),
        ));
    }
    Ok(())
}

fn login_cookie(login: &Login, config: &HashMap<String, String>) -> Result<(), LoginError> {
    let _cookie = login
        .cookie
        .as_ref()
        .map(|c| template::render(c, config))
        .or_else(|| config.get("cookie").cloned())
        .ok_or(LoginError::MissingConfig("cookie".to_string()))?;

    Ok(())
}

async fn login_credentials(
    client: &Client,
    base_url: &str,
    login: &Login,
    config: &HashMap<String, String>,
    method: &str,
) -> Result<(), LoginError> {
    let path = login.path.as_deref().unwrap_or("/");
    let url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );

    let mut form_data = HashMap::new();
    for (key, value) in &login.inputs {
        form_data.insert(key.clone(), template::render(value, config));
    }

    let resp = if method == "get" {
        client.get(&url).query(&form_data).send().await
    } else {
        client.post(&url).form(&form_data).send().await
    };

    let resp = resp.map_err(|e| LoginError::Network(e.to_string()))?;

    if !resp.status().is_success() && !resp.status().is_redirection() {
        return Err(LoginError::LoginFailed(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }

    // Check for error selectors
    if let Some(error_selectors) = &login.error {
        let body = resp
            .text()
            .await
            .map_err(|e| LoginError::Network(e.to_string()))?;
        check_login_errors(&body, error_selectors)?;
    }

    Ok(())
}

fn check_login_errors(
    body: &str,
    error_selectors: &[crate::definition::SelectorBlock],
) -> Result<(), LoginError> {
    let doc = scraper::Html::parse_document(body);
    for error_def in error_selectors {
        if let Some(sel_str) = &error_def.selector {
            if let Ok(sel) = scraper::Selector::parse(sel_str) {
                if let Some(el) = doc.select(&sel).next() {
                    let error_text = el.text().collect::<String>().trim().to_string();
                    if !error_text.is_empty() {
                        return Err(LoginError::LoginFailed(error_text));
                    }
                }
            }
        }
    }
    Ok(())
}

async fn login_form(
    client: &Client,
    base_url: &str,
    login: &Login,
    config: &HashMap<String, String>,
) -> Result<(), LoginError> {
    let path = login.path.as_deref().unwrap_or("/");
    let url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );

    // Fetch the form page
    let page = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LoginError::Network(e.to_string()))?
        .text()
        .await
        .map_err(|e| LoginError::Network(e.to_string()))?;

    // Extract form data synchronously (scraper types don't cross await)
    let form_selector = login.form.as_deref().unwrap_or("form");
    let (form_url, mut form_data) = extract_form_data(&page, base_url, path, form_selector)?;

    // Override with login inputs
    for (key, value) in &login.inputs {
        form_data.insert(key.clone(), template::render(value, config));
    }

    // Submit
    let resp = client
        .post(&form_url)
        .form(&form_data)
        .send()
        .await
        .map_err(|e| LoginError::Network(e.to_string()))?;

    if !resp.status().is_success() && !resp.status().is_redirection() {
        return Err(LoginError::LoginFailed(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }

    Ok(())
}

fn extract_form_data(
    page: &str,
    base_url: &str,
    path: &str,
    form_selector: &str,
) -> Result<(String, HashMap<String, String>), LoginError> {
    let doc = scraper::Html::parse_document(page);
    let sel = scraper::Selector::parse(form_selector)
        .map_err(|_| LoginError::LoginFailed("invalid form selector".to_string()))?;

    let form_el = doc
        .select(&sel)
        .next()
        .ok_or(LoginError::LoginFailed("form not found".to_string()))?;

    let action = form_el.value().attr("action").unwrap_or(path);
    let form_url = if action.starts_with("http") {
        action.to_string()
    } else {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            action.trim_start_matches('/')
        )
    };

    let mut form_data = HashMap::new();
    let input_sel = scraper::Selector::parse("input").unwrap();
    for input in form_el.select(&input_sel) {
        let name = input.value().attr("name").unwrap_or("");
        let value = input.value().attr("value").unwrap_or("");
        if !name.is_empty() {
            form_data.insert(name.to_string(), value.to_string());
        }
    }

    Ok((form_url, form_data))
}

fn get_base_url(def: &Definition, config: &HashMap<String, String>) -> String {
    config
        .get("url")
        .or_else(|| config.get("sitelink"))
        .cloned()
        .or_else(|| def.links.first().cloned())
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("unsupported login method: {0}")]
    UnsupportedMethod(String),
    #[error("missing config: {0}")]
    MissingConfig(String),
    #[error("login failed: {0}")]
    LoginFailed(String),
    #[error("login test failed: {0}")]
    TestFailed(String),
    #[error("network error: {0}")]
    Network(String),
}
