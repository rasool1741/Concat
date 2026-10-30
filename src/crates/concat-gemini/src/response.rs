// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Reading Gemini's replies.
//!
//! A reply is trusted for nothing: every number is checked to be finite,
//! every time to be a time, and what does not fit is an error, not a guess.

use serde_json::Value;

use crate::error::GeminiError;
use crate::{Cue, Word};

fn bad(reason: &str) -> GeminiError {
    GeminiError::BadResponse(reason.to_owned())
}

/// A time as the API writes it: `"0.100s"`, a bare number, `"12.5"`,
/// `"01:02.5"` or `"1:02:03"`. Seconds, finite, never negative.
pub fn parse_time(value: &Value) -> Option<f64> {
    let seconds = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => parse_time_text(s)?,
        _ => return None,
    };
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

fn parse_time_text(text: &str) -> Option<f64> {
    let text = text.trim();
    let text = text.strip_suffix('s').unwrap_or(text).trim();
    let mut total = 0.0;
    let parts: Vec<&str> = text.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for part in &parts {
        total = total * 60.0 + part.trim().replace(',', ".").parse::<f64>().ok()?;
    }
    Some(total)
}

/// Why a reply carries no usable output, when its status says so.
fn check_status(reply: &Value) -> Result<(), GeminiError> {
    match reply.get("status").and_then(Value::as_str) {
        Some("failed") => Err(bad("the request failed")),
        Some("cancelled") => Err(bad("the request was cancelled")),
        Some("incomplete") => Err(bad("the reply stopped early")),
        _ => Ok(()),
    }
}

fn model_text_items(reply: &Value) -> Vec<&Value> {
    let mut items = Vec::new();
    if let Some(steps) = reply.get("steps").and_then(Value::as_array) {
        for step in steps {
            if step.get("type").and_then(Value::as_str) != Some("model_output") {
                continue;
            }
            if let Some(content) = step.get("content").and_then(Value::as_array) {
                items.extend(
                    content
                        .iter()
                        .filter(|c| c.get("type").and_then(Value::as_str) == Some("text")),
                );
            }
        }
    }
    items
}

/// The reply's text, all of it.
pub fn output_text(reply: &Value) -> Result<String, GeminiError> {
    check_status(reply)?;
    let items = model_text_items(reply);
    if !items.is_empty() {
        let text: String = items
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        return Ok(text);
    }
    if let Some(text) = reply.get("output_text").and_then(Value::as_str) {
        return Ok(text.to_owned());
    }
    // The older reply shape.
    if let Some(parts) = reply
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
    {
        let text: String = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        if !text.is_empty() {
            return Ok(text);
        }
    }
    Err(bad("no text in the reply"))
}

/// The words the transcription model timed, in order. Empty when the reply
/// carries no word annotations; times are never filled in.
pub fn word_infos(reply: &Value) -> Result<Vec<Word>, GeminiError> {
    check_status(reply)?;
    let mut words = Vec::new();
    for item in model_text_items(reply) {
        let Some(notes) = item.get("annotations").and_then(Value::as_array) else {
            continue;
        };
        for note in notes {
            if note.get("type").and_then(Value::as_str) != Some("word_info") {
                continue;
            }
            let text = note
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let start = note.get("start_offset").and_then(parse_time);
            let end = note.get("end_offset").and_then(parse_time);
            match (text, start, end) {
                (Some(text), Some(start), Some(end)) if end >= start => words.push(Word {
                    text: text.to_owned(),
                    start,
                    end,
                }),
                _ => return Err(GeminiError::BadTiming("a word has no valid time".into())),
            }
        }
    }
    Ok(words)
}

/// The JSON the model wrote, with a code fence taken off if it added one.
fn json_of(text: &str) -> Result<Value, GeminiError> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|rest| rest.trim_end().trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    serde_json::from_str(inner).map_err(|_| bad("the text is not valid JSON"))
}

/// Cues from a structured reply: `{"cues": [...]}` or a bare array. A cue
/// with no valid time is an error; the caller decides what to do with it.
pub fn parse_cues(text: &str) -> Result<Vec<Cue>, GeminiError> {
    let value = json_of(text)?;
    let list = value
        .get("cues")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| bad("no cues in the reply"))?;
    let mut cues = Vec::with_capacity(list.len());
    for item in list {
        let start = item.get("start").and_then(parse_time);
        let end = item.get("end").and_then(parse_time);
        let words = item.get("text").and_then(Value::as_str).unwrap_or("");
        match (start, end) {
            (Some(start), Some(end)) => cues.push(Cue {
                start,
                end,
                text: words.trim().to_owned(),
                words: Vec::new(),
            }),
            _ => return Err(GeminiError::BadTiming("a cue has no valid time".into())),
        }
    }
    Ok(cues)
}

/// Numbered lines from a translation or correction reply.
pub fn parse_lines(text: &str) -> Result<Vec<(u32, String)>, GeminiError> {
    let value = json_of(text)?;
    let list = value
        .get("lines")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| bad("no lines in the reply"))?;
    list.iter()
        .map(|item| {
            let id = item
                .get("id")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| bad("a line has no id"))?;
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("a line has no text"))?;
            Ok((id, text.to_owned()))
        })
        .collect()
}

/// Checks that a translation returned exactly the ids that were sent, each
/// once, and returns the texts in the order they were sent.
pub fn match_lines(
    sent: &[(u32, String)],
    got: Vec<(u32, String)>,
) -> Result<Vec<String>, GeminiError> {
    let mut by_id = std::collections::HashMap::new();
    for (id, text) in got {
        if by_id.insert(id, text).is_some() {
            return Err(bad("a line came back twice"));
        }
    }
    let mut out = Vec::with_capacity(sent.len());
    for (id, _) in sent {
        out.push(
            by_id
                .remove(id)
                .ok_or_else(|| bad("a line is missing from the reply"))?,
        );
    }
    if !by_id.is_empty() {
        return Err(bad("the reply has lines that were not sent"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(text: &str, notes: Value) -> Value {
        json!({ "status": "completed", "steps": [
            { "type": "thought" },
            { "type": "model_output", "content": [
                { "type": "text", "text": text, "annotations": notes } ] } ] })
    }

    #[test]
    fn times_in_every_shape() {
        assert_eq!(parse_time(&json!("0.100s")), Some(0.1));
        assert_eq!(parse_time(&json!(2.5)), Some(2.5));
        assert_eq!(parse_time(&json!("01:02.5")), Some(62.5));
        assert_eq!(parse_time(&json!("1:02:03")), Some(3723.0));
        assert_eq!(parse_time(&json!("00:01,500")), Some(1.5));
        assert_eq!(parse_time(&json!(-1)), None);
        assert_eq!(parse_time(&json!("soon")), None);
        assert_eq!(parse_time(&json!(null)), None);
    }

    #[test]
    fn text_from_the_model_output_step() {
        let r = reply("سلام", json!([]));
        assert_eq!(output_text(&r).unwrap(), "سلام");
    }

    #[test]
    fn text_from_the_older_shape() {
        let r = json!({ "candidates": [ { "content": { "parts": [ { "text": "hi" } ] } } ] });
        assert_eq!(output_text(&r).unwrap(), "hi");
    }

    #[test]
    fn failed_and_empty_replies_are_errors() {
        assert!(output_text(&json!({ "status": "failed" })).is_err());
        assert!(output_text(&json!({ "status": "completed", "steps": [] })).is_err());
    }

    #[test]
    fn words_with_offsets() {
        let r = reply(
            "سلام دنیا",
            json!([
                { "type": "word_info", "text": "سلام", "start_offset": "0.100s", "end_offset": "0.400s" },
                { "type": "word_info", "text": "دنیا", "start_offset": "0.5s", "end_offset": "0.9s" }
            ]),
        );
        let words = word_infos(&r).unwrap();
        assert_eq!(words.len(), 2);
        assert_eq!((words[1].start, words[1].end), (0.5, 0.9));
    }

    #[test]
    fn a_word_without_a_time_is_refused_not_guessed() {
        let r = reply(
            "x",
            json!([{ "type": "word_info", "text": "x", "start_offset": "0.1s" }]),
        );
        assert!(matches!(word_infos(&r), Err(GeminiError::BadTiming(_))));
        let backwards = reply(
            "x",
            json!([{ "type": "word_info", "text": "x", "start_offset": "2s", "end_offset": "1s" }]),
        );
        assert!(word_infos(&backwards).is_err());
    }

    #[test]
    fn no_annotations_means_no_words() {
        assert!(word_infos(&reply("hi", json!([]))).unwrap().is_empty());
    }

    #[test]
    fn cues_object_array_and_fenced() {
        let object = r#"{"cues":[{"start":0,"end":1.5,"text":" سلام "}]}"#;
        let cues = parse_cues(object).unwrap();
        assert_eq!(cues[0].text, "سلام");
        assert_eq!(cues[0].end, 1.5);
        let fenced = "```json\n[{\"start\":\"0:01\",\"end\":\"0:02\",\"text\":\"a\"}]\n```";
        assert_eq!(parse_cues(fenced).unwrap()[0].start, 1.0);
        assert!(parse_cues("not json").is_err());
        assert!(matches!(
            parse_cues(r#"{"cues":[{"start":"x","end":1,"text":"a"}]}"#),
            Err(GeminiError::BadTiming(_))
        ));
    }

    #[test]
    fn lines_must_match_what_was_sent() {
        let sent = vec![(1, "a".to_owned()), (2, "b".to_owned())];
        let ok = parse_lines(r#"{"lines":[{"id":2,"text":"B"},{"id":1,"text":"A"}]}"#).unwrap();
        assert_eq!(match_lines(&sent, ok).unwrap(), ["A", "B"]);
        let missing = vec![(1, "A".to_owned())];
        assert!(match_lines(&sent, missing).is_err());
        let extra = vec![(1, "A".into()), (2, "B".into()), (3, "C".into())];
        assert!(match_lines(&sent, extra).is_err());
        let twice = vec![(1, "A".into()), (1, "A".into()), (2, "B".into())];
        assert!(match_lines(&sent, twice).is_err());
    }
}
