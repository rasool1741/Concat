// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Subtitles from the Gemini API.
//!
//! The pieces, each testable without a network:
//!
//! - [`chunk`] cuts long audio into overlapping windows and stitches the
//!   words back together without a gap or a repeat at the seams.
//! - [`request`] builds the JSON bodies: transcription, translation,
//!   correction and a single line's regeneration.
//! - [`response`] reads a reply: text, word timings, and the cues a model
//!   was asked to return.
//! - [`cues`] groups words into cues and [`validate`] checks them.
//! - [`client`] is the blocking HTTP client, with the key kept out of every
//!   error and every `Debug` print.
//!
//! The crate depends on nothing else in the workspace: the window converts
//! [`Cue`] to its own types at the edge.

pub mod chunk;
pub mod client;
pub mod cues;
pub mod error;
pub mod request;
pub mod response;
pub mod validate;

pub use client::{ApiKey, Client, ModelInfo};
pub use error::GeminiError;

/// One timed word, in seconds from the start of the whole audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// The word as spoken, without surrounding space.
    pub text: String,
    /// Seconds from the start.
    pub start: f64,
    /// Seconds from the start; never before `start`.
    pub end: f64,
}

/// One subtitle: when it shows, when it goes, its words and, when the model
/// gave them, each word's timing.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    /// Seconds from the start.
    pub start: f64,
    /// Seconds from the start; never before `start`.
    pub end: f64,
    /// The words, a line break between lines.
    pub text: String,
    /// Word timings, empty when the model gave none. Timings that were not
    /// measured are never invented to fill this.
    pub words: Vec<Word>,
}
