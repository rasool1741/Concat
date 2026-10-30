// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The blocking HTTP client.
//!
//! The key lives in an [`ApiKey`] whose `Debug` prints nothing of it, goes
//! out only in the `x-goog-api-key` header (never in a URL, so it is not in
//! a proxy's or a browser's history), and is scrubbed from every error
//! message. Calls are meant for a worker thread: they block, check a cancel
//! flag between attempts, and retry only the failures that can pass.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::error::{GeminiError, redact};

/// The service's address.
pub const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";

/// What one request may weigh: the API takes 20 MB with everything in it,
/// so a body over this is refused before it is sent.
pub const MAX_BODY_BYTES: usize = 19 * 1024 * 1024;

/// An API key, held so it cannot be printed by accident.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    /// A key as the person typed it; surrounding space is dropped.
    pub fn new(key: &str) -> ApiKey {
        ApiKey(key.trim().to_owned())
    }

    /// The key is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

/// A model the key can see.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    /// The id without the `models/` prefix.
    pub id: String,
    /// A name for a list.
    pub display_name: String,
    /// What the server says it supports.
    pub methods: Vec<String>,
}

/// The client.
pub struct Client {
    key: ApiKey,
    base: String,
    agent: ureq::Agent,
    cancel: Arc<AtomicBool>,
    retries: u32,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("key", &self.key)
            .finish()
    }
}

impl Client {
    /// A client for the real service.
    pub fn new(key: ApiKey) -> Client {
        Client::with_base_url(key, DEFAULT_BASE)
    }

    /// A client for another address; tests point it at a local server.
    pub fn with_base_url(key: ApiKey, base: &str) -> Client {
        Client {
            key,
            base: base.trim_end_matches('/').to_owned(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(300))
                .build(),
            cancel: Arc::new(AtomicBool::new(false)),
            retries: 2,
        }
    }

    /// How many more times a failure that can pass is tried.
    pub fn retries(mut self, retries: u32) -> Client {
        self.retries = retries;
        self
    }

    /// The flag that stops a call in progress: set it from the window.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// The models the key can see.
    pub fn list_models(&self) -> Result<Vec<ModelInfo>, GeminiError> {
        let mut models = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut url = format!("{}/v1beta/models?pageSize=200", self.base);
            if let Some(token) = &page {
                url.push_str("&pageToken=");
                url.push_str(token);
            }
            let reply = self.send(|agent| agent.get(&url), None)?;
            if let Some(list) = reply.get("models").and_then(Value::as_array) {
                for item in list {
                    let Some(name) = item.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    models.push(ModelInfo {
                        id: name.strip_prefix("models/").unwrap_or(name).to_owned(),
                        display_name: item
                            .get("displayName")
                            .and_then(Value::as_str)
                            .unwrap_or(name)
                            .to_owned(),
                        methods: item
                            .get("supportedGenerationMethods")
                            .and_then(Value::as_array)
                            .map(|m| {
                                m.iter()
                                    .filter_map(|s| s.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                }
            }
            match reply.get("nextPageToken").and_then(Value::as_str) {
                Some(token) if !token.is_empty() => page = Some(token.to_owned()),
                _ => break,
            }
        }
        Ok(models)
    }

    /// Settings' "Test connection": the key works, and, when `model` is
    /// given, the key can see it. Returns the models for the picker.
    pub fn test_connection(&self, model: Option<&str>) -> Result<Vec<ModelInfo>, GeminiError> {
        let models = self.list_models()?;
        if let Some(model) = model {
            let wanted = model.strip_prefix("models/").unwrap_or(model);
            if !models.iter().any(|m| m.id == wanted) {
                return Err(GeminiError::ModelUnavailable(wanted.to_owned()));
            }
        }
        Ok(models)
    }

    /// Sends one interaction and returns the reply's JSON.
    pub fn interact(&self, body: &Value) -> Result<Value, GeminiError> {
        let text = serde_json::to_string(body)
            .map_err(|_| GeminiError::BadResponse("the request could not be built".into()))?;
        if text.len() > MAX_BODY_BYTES {
            return Err(GeminiError::TooLarge);
        }
        let url = format!("{}/v1beta/interactions", self.base);
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.send(|agent| agent.post(&url), Some((&text, &model)))
    }

    fn send(
        &self,
        make: impl Fn(&ureq::Agent) -> ureq::Request,
        body: Option<(&str, &str)>,
    ) -> Result<Value, GeminiError> {
        if self.key.is_empty() {
            return Err(GeminiError::NoKey);
        }
        let mut attempt = 0;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(GeminiError::Cancelled);
            }
            let request = make(&self.agent)
                .set("x-goog-api-key", self.key.expose())
                .set("Content-Type", "application/json");
            let result = match body {
                Some((text, _)) => request.send_string(text),
                None => request.call(),
            };
            let error = match result {
                Ok(response) => {
                    let text = response.into_string().map_err(|_| GeminiError::Offline)?;
                    return serde_json::from_str(&text)
                        .map_err(|_| GeminiError::BadResponse("the reply is not JSON".into()));
                }
                Err(ureq::Error::Status(status, response)) => {
                    let retry_after = response
                        .header("retry-after")
                        .and_then(|v| v.trim().parse::<u64>().ok());
                    let text = response.into_string().unwrap_or_default();
                    self.status_error(status, &text, retry_after, body.map(|b| b.1))
                }
                Err(ureq::Error::Transport(_)) => GeminiError::Offline,
            };
            let passes = matches!(
                error,
                GeminiError::Offline
                    | GeminiError::Api {
                        status: 500..=599,
                        ..
                    }
            );
            if !passes || attempt >= self.retries {
                return Err(error);
            }
            attempt += 1;
            self.pause(Duration::from_millis(500 * 2u64.pow(attempt)))?;
        }
    }

    /// Sleeps in short slices so a cancel is heard at once.
    fn pause(&self, total: Duration) -> Result<(), GeminiError> {
        let mut left = total;
        let slice = Duration::from_millis(50);
        while !left.is_zero() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(GeminiError::Cancelled);
            }
            let now = left.min(slice);
            std::thread::sleep(now);
            left -= now;
        }
        Ok(())
    }

    fn status_error(
        &self,
        status: u16,
        text: &str,
        retry_after: Option<u64>,
        model: Option<&str>,
    ) -> GeminiError {
        let parsed: Option<Value> = serde_json::from_str(text).ok();
        let message = parsed
            .as_ref()
            .and_then(|v| v.pointer("/error/message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let message = redact(&message, self.key.expose());
        let lower = message.to_lowercase();
        match status {
            401 | 403 => GeminiError::InvalidKey,
            400 if lower.contains("api key") || lower.contains("api_key") => {
                GeminiError::InvalidKey
            }
            404 => GeminiError::ModelUnavailable(
                model
                    .map(|m| m.strip_prefix("models/").unwrap_or(m).to_owned())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "the requested model".to_owned()),
            ),
            413 => GeminiError::TooLarge,
            429 => GeminiError::Quota { retry_after },
            400 if lower.contains("audio") || lower.contains("mime") => {
                GeminiError::BadAudio(message)
            }
            _ => GeminiError::Api { status, message },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    /// A one-thread HTTP server that answers each connection with the next
    /// canned reply and records what it was asked.
    pub(crate) struct Mock {
        pub(crate) base: String,
        pub(crate) seen: Arc<Mutex<Vec<String>>>,
    }

    pub(crate) fn mock(replies: Vec<(u16, &'static str, &'static str)>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, extra, body) in replies {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut data = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    data.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&data).to_string());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Mock { base, seen }
    }

    fn client(mock: &Mock) -> Client {
        Client::with_base_url(ApiKey::new("  SECRET-KEY-123  "), &mock.base).retries(1)
    }

    #[test]
    fn the_key_is_never_printed() {
        let key = ApiKey::new("SECRET-KEY-123");
        assert!(!format!("{key:?}").contains("SECRET"));
        let client = Client::new(key);
        assert!(!format!("{client:?}").contains("SECRET"));
    }

    #[test]
    fn errors_scrub_an_echoed_key() {
        assert_eq!(
            redact("bad key SECRET-KEY-123 here", "SECRET-KEY-123"),
            "bad key [key] here"
        );
        let m = mock(vec![
            (
                500,
                "",
                r#"{"error":{"message":"oops SECRET-KEY-123"}}"#
            );
            3
        ]);
        let err = client(&m)
            .interact(&serde_json::json!({"model":"m"}))
            .unwrap_err();
        assert!(!err.to_string().contains("SECRET"), "{err}");
    }

    #[test]
    fn an_empty_key_is_refused_before_any_request() {
        let c = Client::new(ApiKey::new("   "));
        assert_eq!(c.list_models().unwrap_err(), GeminiError::NoKey);
    }

    #[test]
    fn the_key_travels_in_a_header_not_the_url() {
        let m = mock(vec![(
            200,
            "",
            r#"{"models":[{"name":"models/gemini-x","displayName":"X","supportedGenerationMethods":["generateContent"]}]}"#,
        )]);
        let models = client(&m).list_models().unwrap();
        assert_eq!(models[0].id, "gemini-x");
        let seen = m.seen.lock().unwrap()[0].clone();
        let first_line = seen.lines().next().unwrap();
        assert!(!first_line.contains("SECRET"), "key must not be in the URL");
        assert!(
            seen.to_lowercase()
                .contains("x-goog-api-key: secret-key-123")
        );
    }

    #[test]
    fn test_connection_checks_the_model() {
        let body = r#"{"models":[{"name":"models/gemini-a"}]}"#;
        let m = mock(vec![(200, "", body), (200, "", body)]);
        let c = client(&m);
        assert!(c.test_connection(Some("gemini-a")).is_ok());
        assert_eq!(
            c.test_connection(Some("models/nope")).unwrap_err(),
            GeminiError::ModelUnavailable("nope".into())
        );
    }

    #[test]
    fn statuses_become_plain_errors() {
        let cases: Vec<((u16, &'static str, &'static str), GeminiError)> = vec![
            (
                (403, "", r#"{"error":{"message":"denied"}}"#),
                GeminiError::InvalidKey,
            ),
            (
                (400, "", r#"{"error":{"message":"API key not valid"}}"#),
                GeminiError::InvalidKey,
            ),
            (
                (429, "Retry-After: 7\r\n", "{}"),
                GeminiError::Quota {
                    retry_after: Some(7),
                },
            ),
            (
                (404, "", "{}"),
                GeminiError::ModelUnavailable("gemini-q".into()),
            ),
            ((413, "", "{}"), GeminiError::TooLarge),
        ];
        for (reply, want) in cases {
            let m = mock(vec![reply]);
            let got = client(&m)
                .interact(&serde_json::json!({"model":"models/gemini-q"}))
                .unwrap_err();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn a_server_error_is_retried_then_succeeds() {
        let m = mock(vec![
            (503, "", "{}"),
            (200, "", r#"{"status":"completed"}"#),
        ]);
        let reply = client(&m)
            .interact(&serde_json::json!({"model":"m"}))
            .unwrap();
        assert_eq!(reply["status"], "completed");
        assert_eq!(m.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn nothing_listening_is_offline() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        let c = Client::with_base_url(ApiKey::new("k"), &base).retries(0);
        assert_eq!(c.list_models().unwrap_err(), GeminiError::Offline);
    }

    #[test]
    fn cancel_stops_before_sending() {
        let m = mock(vec![]);
        let c = client(&m);
        c.cancel_handle().store(true, Ordering::Relaxed);
        assert_eq!(c.list_models().unwrap_err(), GeminiError::Cancelled);
        assert!(m.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn an_oversized_body_is_refused_locally() {
        let m = mock(vec![]);
        let big = "a".repeat(MAX_BODY_BYTES + 1);
        let err = client(&m)
            .interact(&serde_json::json!({"model":"m","x":big}))
            .unwrap_err();
        assert_eq!(err, GeminiError::TooLarge);
        assert!(m.seen.lock().unwrap().is_empty());
    }
}
