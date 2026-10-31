// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Translating and correcting a lane of captions, batch by batch.
//!
//! Lines go out as `(id, text)` and come back the same way. Nothing about
//! time goes to the model, so nothing about time can come back changed: the
//! caller puts each returned text on the cue it belongs to by id. A reply
//! that drops, repeats, invents or blanks a line is an error, not a
//! partial result.

use std::sync::atomic::Ordering;

use crate::client::Client;
use crate::error::GeminiError;
use crate::{request, response};

/// What to do with the lines.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Into `target`, a language name such as "English" or "Persian".
    Translate {
        /// The language to write.
        target: String,
        /// The language they are in, when known.
        source: Option<String>,
        /// Terms to keep as written.
        glossary: Vec<String>,
    },
    /// Spelling, spacing and punctuation fixed; nothing rephrased.
    Correct {
        /// The language, when known.
        language: Option<String>,
    },
}

/// The most lines in one request, and the most characters: a batch ends at
/// whichever comes first.
pub const MAX_LINES: usize = 40;
/// See [`MAX_LINES`].
pub const MAX_CHARS: usize = 6000;

/// Lines in order cut into batches that stay inside both limits. A single
/// line over the character limit goes alone.
pub fn batches(lines: &[(u32, String)]) -> Vec<Vec<(u32, String)>> {
    let mut out: Vec<Vec<(u32, String)>> = Vec::new();
    let mut current: Vec<(u32, String)> = Vec::new();
    let mut chars = 0;
    for line in lines {
        let size = line.1.chars().count();
        if !current.is_empty() && (current.len() >= MAX_LINES || chars + size > MAX_CHARS) {
            out.push(std::mem::take(&mut current));
            chars = 0;
        }
        chars += size;
        current.push(line.clone());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Runs `edit` over `lines`, returning the new texts in the order given.
/// `progress` is told `(batches done, batches in all)` after each batch.
pub fn run(
    client: &Client,
    model: &str,
    lines: &[(u32, String)],
    edit: &Edit,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Vec<String>, GeminiError> {
    let groups = batches(lines);
    let total = groups.len();
    let cancel = client.cancel_handle();
    let mut out = Vec::with_capacity(lines.len());
    progress(0, total);
    for (done, group) in groups.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(GeminiError::Cancelled);
        }
        let body = match edit {
            Edit::Translate {
                target,
                source,
                glossary,
            } => request::translate_body(model, group, target, source.as_deref(), glossary),
            Edit::Correct { language } => request::correct_body(model, group, language.as_deref()),
        };
        let reply = client.interact(&body)?;
        let text = response::output_text(&reply)?;
        let got = response::parse_lines(&text)?;
        let texts = response::match_lines(group, got)?;
        for ((_, sent), got) in group.iter().zip(&texts) {
            if !sent.trim().is_empty() && got.trim().is_empty() {
                return Err(GeminiError::BadResponse("a line came back empty".into()));
            }
        }
        out.extend(texts);
        progress(done + 1, total);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ApiKey, tests::mock};

    fn line(id: u32, text: &str) -> (u32, String) {
        (id, text.to_owned())
    }

    #[test]
    fn batches_respect_both_limits_and_keep_order() {
        let many: Vec<_> = (0..95).map(|i| line(i, "short")).collect();
        let groups = batches(&many);
        assert_eq!(
            groups.iter().map(Vec::len).collect::<Vec<_>>(),
            [40, 40, 15]
        );
        let flat: Vec<u32> = groups.iter().flatten().map(|l| l.0).collect();
        assert_eq!(flat, (0..95).collect::<Vec<_>>());

        let long = "x".repeat(4000);
        let heavy = vec![line(1, &long), line(2, &long), line(3, "tail")];
        let groups = batches(&heavy);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 1);
        assert!(batches(&[]).is_empty());
    }

    fn reply(lines: &str) -> String {
        let text = format!("{{\\\"lines\\\":{lines}}}");
        format!(
            r#"{{"status":"completed","steps":[{{"type":"model_output","content":[{{"type":"text","text":"{text}"}}]}}]}}"#
        )
    }

    /// The mock serves `&'static str`, so the canned bodies are leaked for
    /// the length of the test run.
    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn a_translation_comes_back_by_id() {
        let body = leak(reply(
            r#"[{\"id\":2,\"text\":\"World\"},{\"id\":1,\"text\":\"Hello\"}]"#,
        ));
        let m = mock(vec![(200, "", body)]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let mut steps = Vec::new();
        let out = run(
            &client,
            "gemini-x",
            &[line(1, "سلام"), line(2, "دنیا")],
            &Edit::Translate {
                target: "English".into(),
                source: Some("Persian".into()),
                glossary: vec![],
            },
            &mut |d, t| steps.push((d, t)),
        )
        .unwrap();
        assert_eq!(out, ["Hello", "World"]);
        assert_eq!(steps, [(0, 1), (1, 1)]);
        // Only ids and words were sent: no start or end of any cue.
        let seen = m.seen.lock().unwrap()[0].clone();
        assert!(seen.contains("سلام") || seen.contains("\\u0633"));
        assert!(!seen.contains("\"start\""));
    }

    #[test]
    fn a_reply_with_a_missing_or_blank_line_is_an_error() {
        let missing = leak(reply(r#"[{\"id\":1,\"text\":\"Hello\"}]"#));
        let m = mock(vec![(200, "", missing)]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let edit = Edit::Correct { language: None };
        let sent = [line(1, "a"), line(2, "b")];
        assert!(matches!(
            run(&client, "m", &sent, &edit, &mut |_, _| {}),
            Err(GeminiError::BadResponse(_))
        ));

        let blank = leak(reply(
            r#"[{\"id\":1,\"text\":\"A\"},{\"id\":2,\"text\":\"  \"}]"#,
        ));
        let m = mock(vec![(200, "", blank)]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        assert!(run(&client, "m", &sent, &edit, &mut |_, _| {}).is_err());
    }

    #[test]
    fn cancel_is_heard_between_batches() {
        let m = mock(vec![]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        client.cancel_handle().store(true, Ordering::Relaxed);
        let out = run(
            &client,
            "m",
            &[line(1, "a")],
            &Edit::Correct { language: None },
            &mut |_, _| {},
        );
        assert_eq!(out, Err(GeminiError::Cancelled));
        assert!(m.seen.lock().unwrap().is_empty());
    }
}
