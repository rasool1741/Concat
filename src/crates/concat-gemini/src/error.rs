// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! What can go wrong, in words a person can act on. No variant carries the
//! API key: messages that came from the server are scrubbed of it first.

use thiserror::Error;

/// Why a Gemini call failed.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum GeminiError {
    /// No key was given.
    #[error("no Gemini API key is set")]
    NoKey,
    /// The server refused the key.
    #[error("the Gemini API key was rejected: check it in Settings")]
    InvalidKey,
    /// The quota or the rate limit is spent.
    #[error("Gemini's quota or rate limit is reached{}", retry_hint(*.retry_after))]
    Quota {
        /// Seconds the server asked to wait, when it said.
        retry_after: Option<u64>,
    },
    /// The model does not exist for this key.
    #[error("the model \"{0}\" is not available for this key")]
    ModelUnavailable(String),
    /// The machine could not reach the service.
    #[error("cannot reach Gemini: check the internet connection")]
    Offline,
    /// The request is bigger than the API takes.
    #[error("the audio is too large for one request")]
    TooLarge,
    /// The audio could not be used.
    #[error("the audio could not be read: {0}")]
    BadAudio(String),
    /// The reply was not what was asked for.
    #[error("Gemini's reply was incomplete or invalid: {0}")]
    BadResponse(String),
    /// Timings in the reply cannot be trusted.
    #[error("the timings in Gemini's reply are not valid: {0}")]
    BadTiming(String),
    /// Any other refusal from the service.
    #[error("Gemini returned an error ({status}): {message}")]
    Api {
        /// HTTP status.
        status: u16,
        /// The server's words, with any key removed.
        message: String,
    },
    /// The person cancelled.
    #[error("cancelled")]
    Cancelled,
}

fn retry_hint(after: Option<u64>) -> String {
    match after {
        Some(seconds) => format!(": try again in {seconds} s"),
        None => String::new(),
    }
}

/// `text` with every copy of `key` replaced, so a server that echoes a key
/// back cannot put it on screen or in a log.
pub fn redact(text: &str, key: &str) -> String {
    if key.is_empty() {
        return text.to_owned();
    }
    text.replace(key, "[key]")
}
