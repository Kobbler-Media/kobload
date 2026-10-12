use reqwest::{Client, StatusCode, header::HeaderMap};
use serde::{Serialize, de::DeserializeOwned};

/// HTTP test client with authentication helpers.
pub struct TestClient {
    client: Client,
    base_url: String,
    auth_token: Option<String>,
}

impl TestClient {
    /// Create a new test client for the given base URL.
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            auth_token: None,
        }
    }

    /// Set the authentication token for subsequent requests.
    pub fn with_auth(mut self, token: &str) -> Self {
        self.auth_token = Some(token.to_string());
        self
    }

    /// Make a GET request.
    pub async fn get(&self, path: &str) -> TestResponse {
        let mut req = self.client.get(self.url(path));
        if let Some(ref token) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let resp = req.send().await.expect("GET request failed");
        TestResponse::from_reqwest(resp).await
    }

    /// Make a GET request with extra headers (e.g. `Range`).
    pub async fn get_with_headers(&self, path: &str, headers: &[(&str, &str)]) -> TestResponse {
        let mut req = self.client.get(self.url(path));
        if let Some(ref token) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let resp = req.send().await.expect("GET request failed");
        TestResponse::from_reqwest(resp).await
    }

    /// Make a POST request without body.
    pub async fn post(&self, path: &str) -> TestResponse {
        let mut req = self.client.post(self.url(path));
        if let Some(ref token) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let resp = req.send().await.expect("POST request failed");
        TestResponse::from_reqwest(resp).await
    }

    /// Make a POST request with JSON body.
    pub async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> TestResponse {
        let mut req = self.client.post(self.url(path)).json(body);
        if let Some(ref token) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let resp = req.send().await.expect("POST request failed");
        TestResponse::from_reqwest(resp).await
    }

    /// Make a PUT request with JSON body.
    pub async fn put_json<T: Serialize>(&self, path: &str, body: &T) -> TestResponse {
        let mut req = self.client.put(self.url(path)).json(body);
        if let Some(ref token) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let resp = req.send().await.expect("PUT request failed");
        TestResponse::from_reqwest(resp).await
    }

    /// Make a PATCH request with JSON body.
    pub async fn patch_json<T: Serialize>(&self, path: &str, body: &T) -> TestResponse {
        let mut req = self.client.patch(self.url(path)).json(body);
        if let Some(ref token) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let resp = req.send().await.expect("PATCH request failed");
        TestResponse::from_reqwest(resp).await
    }

    /// Make a DELETE request.
    pub async fn delete(&self, path: &str) -> TestResponse {
        let mut req = self.client.delete(self.url(path));
        if let Some(ref token) = self.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let resp = req.send().await.expect("DELETE request failed");
        TestResponse::from_reqwest(resp).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

/// Response from a test HTTP request.
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    body: Vec<u8>,
}

impl TestResponse {
    async fn from_reqwest(resp: reqwest::Response) -> Self {
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp.bytes().await.expect("Failed to read body").to_vec();
        Self {
            status,
            headers,
            body,
        }
    }

    /// Parse the response body as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> T {
        let text = self.text();
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| panic!("Failed to parse response as JSON: {}", text))
    }

    /// Get the response body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// Get the raw response body.
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    /// Assert the response status is 200 OK.
    pub fn assert_ok(&self) -> &Self {
        assert_eq!(
            self.status,
            StatusCode::OK,
            "Expected 200 OK but got {}. Body: {}",
            self.status,
            self.text()
        );
        self
    }

    /// Assert the response status is successful (2xx).
    pub fn assert_success(&self) -> &Self {
        assert!(
            self.status.is_success(),
            "Expected 2xx success but got {}. Body: {}",
            self.status,
            self.text()
        );
        self
    }

    /// Assert the response status matches expected.
    pub fn assert_status(&self, expected: StatusCode) -> &Self {
        assert_eq!(
            self.status,
            expected,
            "Expected {} but got {}. Body: {}",
            expected,
            self.status,
            self.text()
        );
        self
    }
}
