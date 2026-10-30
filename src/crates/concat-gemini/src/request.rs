// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The JSON bodies sent to Gemini's `interactions` endpoint.
//!
//! Two ways to transcribe, because the dedicated transcription model gives
//! word timings of its own and a general model does not:
//!
//! - [`transcribe_body`]: `gemini-3.5-transcribe` style, word timestamps
//!   asked for through `transcription_config`.
//! - [`cues_body`]: any other model, asked for timed cues as structured JSON.
//!
//! Translation and correction send ids and text only, so a reply cannot move
//! a cue in time. Every instruction forbids summarising or inventing.

use base64::Engine;
use serde_json::{Value, json};

/// Where the audio comes from.
#[derive(Clone, Debug)]
pub enum AudioInput {
    /// The audio itself, base64 in the request.
    Inline {
        /// e.g. `audio/wav`.
        mime: String,
        /// Standard base64.
        data: String,
    },
    /// A file already uploaded to the Files API.
    File {
        /// The uploaded file's URI.
        uri: String,
    },
}

impl AudioInput {
    /// Inline audio from raw bytes.
    pub fn inline(mime: &str, bytes: &[u8]) -> AudioInput {
        AudioInput::Inline {
            mime: mime.to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    fn part(&self) -> Value {
        match self {
            AudioInput::Inline { mime, data } => {
                json!({ "type": "audio", "data": data, "mime_type": mime })
            }
            AudioInput::File { uri } => json!({ "type": "file", "file_uri": uri }),
        }
    }
}

/// The rules every prompt carries.
const FAITHFUL: &str = "Write exactly what is said, in the language it is said. \
Do not summarise, skip sentences, add commentary, or change the meaning. \
Keep names, numbers and technical terms as spoken.";

/// Transcription with the dedicated model. `language` is a BCP-47 code such
/// as `fa-IR`; `None` lets the model detect it.
pub fn transcribe_body(
    model: &str,
    audio: &AudioInput,
    language: Option<&str>,
    word_timestamps: bool,
) -> Value {
    let mut mode = json!({ "type": "verbatim" });
    if word_timestamps {
        mode["timestamp_granularities"] = json!(["word"]);
    }
    let mut config = json!({ "mode": mode });
    if let Some(code) = language.filter(|c| !c.is_empty()) {
        config["language_codes"] = json!([code]);
    }
    json!({
        "model": model_name(model),
        "input": [audio.part()],
        "generation_config": { "transcription_config": config },
    })
}

/// Transcription with a general model: timed cues back as JSON.
pub fn cues_body(model: &str, audio: &AudioInput, language: Option<&str>) -> Value {
    let spoken = match language.filter(|c| !c.is_empty()) {
        Some(code) => format!("The speech is in {code}."),
        None => "Detect the spoken language.".to_owned(),
    };
    let prompt = format!(
        "Transcribe this audio as subtitles. {spoken} {FAITHFUL} \
Return cues in order. Each cue has start and end in seconds from the start of \
this audio, measured from what you hear, and the text of one or two short lines. \
Do not overlap cues. If you cannot place a time, leave the cue out rather than guess."
    );
    json!({
        "model": model_name(model),
        "input": [ { "type": "text", "text": prompt }, audio.part() ],
        "response_format": {
            "type": "text",
            "mime_type": "application/json",
            "schema": {
                "type": "object",
                "properties": {
                    "cues": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "start": { "type": "number" },
                                "end": { "type": "number" },
                                "text": { "type": "string" }
                            },
                            "required": ["start", "end", "text"]
                        }
                    }
                },
                "required": ["cues"]
            }
        }
    })
}

/// Translation of numbered lines. Ids come back unchanged.
pub fn translate_body(
    model: &str,
    lines: &[(u32, String)],
    target: &str,
    source: Option<&str>,
    glossary: &[String],
) -> Value {
    let from = source
        .filter(|s| !s.is_empty())
        .map(|s| format!(" from {s}"))
        .unwrap_or_default();
    let terms = if glossary.is_empty() {
        String::new()
    } else {
        format!(" Keep these terms as written: {}.", glossary.join(", "))
    };
    let prompt = format!(
        "Translate these subtitle lines{from} into {target}. Keep each line's id and \
return one translated line per input line, in the same order, never merging or \
splitting lines. Keep meaning, tone, names and numbers. Keep line breaks (\\n) \
where they are. Do not add notes.{terms}"
    );
    texts_body(model, &prompt, lines)
}

/// Spelling, punctuation and obvious mis-hearings fixed, nothing rewritten.
pub fn correct_body(model: &str, lines: &[(u32, String)], language: Option<&str>) -> Value {
    let lang = language
        .filter(|s| !s.is_empty())
        .map(|s| format!(" The language is {s}."))
        .unwrap_or_default();
    let prompt = format!(
        "Correct spelling, spacing and punctuation in these subtitle lines.{lang} \
Do not rephrase, shorten, translate or add words. Keep each id, the order, and \
line breaks (\\n). For Persian keep the half-space (ZWNJ) where it belongs."
    );
    texts_body(model, &prompt, lines)
}

fn texts_body(model: &str, prompt: &str, lines: &[(u32, String)]) -> Value {
    let payload: Vec<Value> = lines
        .iter()
        .map(|(id, text)| json!({ "id": id, "text": text }))
        .collect();
    json!({
        "model": model_name(model),
        "input": format!("{prompt}\n\n{}", Value::Array(payload)),
        "response_format": {
            "type": "text",
            "mime_type": "application/json",
            "schema": {
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "integer" },
                                "text": { "type": "string" }
                            },
                            "required": ["id", "text"]
                        }
                    }
                },
                "required": ["lines"]
            }
        }
    })
}

/// `gemini-x` and `models/gemini-x` both name the same model; the endpoint
/// takes the `models/` form.
fn model_name(model: &str) -> String {
    if model.starts_with("models/") {
        model.to_owned()
    } else {
        format!("models/{model}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcribe_asks_for_word_timestamps_and_language() {
        let audio = AudioInput::inline("audio/wav", b"RIFF");
        let body = transcribe_body("gemini-3.5-transcribe", &audio, Some("fa-IR"), true);
        assert_eq!(body["model"], "models/gemini-3.5-transcribe");
        let config = &body["generation_config"]["transcription_config"];
        assert_eq!(config["mode"]["timestamp_granularities"][0], "word");
        assert_eq!(config["language_codes"][0], "fa-IR");
        assert_eq!(body["input"][0]["type"], "audio");
        assert_eq!(body["input"][0]["mime_type"], "audio/wav");
        assert_eq!(body["input"][0]["data"], "UklGRg==");
    }

    #[test]
    fn auto_language_sends_no_language_codes() {
        let audio = AudioInput::File {
            uri: "files/x".into(),
        };
        let body = transcribe_body("models/m", &audio, None, false);
        assert_eq!(body["model"], "models/m");
        let config = &body["generation_config"]["transcription_config"];
        assert!(config.get("language_codes").is_none());
        assert!(config["mode"].get("timestamp_granularities").is_none());
        assert_eq!(body["input"][0]["file_uri"], "files/x");
    }

    #[test]
    fn cues_prompt_forbids_inventing() {
        let audio = AudioInput::inline("audio/mp3", b"x");
        let body = cues_body("gemini-flash", &audio, None);
        let prompt = body["input"][0]["text"].as_str().unwrap();
        assert!(prompt.contains("Do not summarise"));
        assert!(prompt.contains("rather than guess"));
        assert_eq!(
            body["response_format"]["schema"]["properties"]["cues"]["items"]["required"],
            json!(["start", "end", "text"])
        );
    }

    #[test]
    fn translation_carries_ids_and_no_times() {
        let lines = vec![(7, "سلام دنیا".to_owned()), (8, "Hello".to_owned())];
        let body = translate_body("m", &lines, "English", Some("Persian"), &["Concat".into()]);
        let input = body["input"].as_str().unwrap();
        assert!(input.contains("\"id\":7"));
        assert!(input.contains("سلام دنیا"));
        assert!(input.contains("Keep these terms as written: Concat"));
        assert!(!input.contains("start"));
    }

    #[test]
    fn correction_never_rephrases() {
        let body = correct_body("m", &[(1, "x".into())], Some("fa"));
        assert!(body["input"].as_str().unwrap().contains("Do not rephrase"));
    }
}
