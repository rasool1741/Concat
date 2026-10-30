// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! A whole subtitle run: the audio in windows, each sent, the replies put
//! back together and checked.
//!
//! The audio itself is the caller's: [`run`] is handed a function that reads
//! a span as 16 kHz mono floats, so this crate links no decoder and the run
//! can be tested with a fake one.

use crate::chunk::{self, Chunk};
use crate::client::Client;
use crate::cues::{GroupOptions, group_words};
use crate::error::GeminiError;
use crate::request::{AudioInput, cues_body, transcribe_body};
use crate::response::{output_text, parse_cues, word_infos};
use crate::validate::{Report, validate};
use crate::{Cue, wav};

/// The rate the audio is read and sent at.
pub const SAMPLE_RATE: u32 = 16_000;

/// How the model is asked for timings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The transcription model, with word timestamps; cues are built here.
    WordTimestamps,
    /// Any other model, asked for timed cues; its times are the model's own
    /// estimate and are checked, not trusted.
    TimedCues,
}

/// What a run is asked to do.
#[derive(Clone, Debug)]
pub struct JobOptions {
    /// The model's id.
    pub model: String,
    /// BCP-47 code of the speech, `None` for the model to detect.
    pub language: Option<String>,
    /// How timings are had.
    pub mode: Mode,
    /// The longest window sent at once, seconds. Five minutes of 16 kHz
    /// mono WAV is 9.6 MB, well under the request limit.
    pub chunk_seconds: f64,
    /// How much neighbouring windows share, seconds.
    pub overlap_seconds: f64,
    /// How words become cues.
    pub group: GroupOptions,
}

impl JobOptions {
    /// The defaults for `model`.
    pub fn new(model: &str, language: Option<&str>, mode: Mode) -> JobOptions {
        JobOptions {
            model: model.to_owned(),
            language: language.map(str::to_owned),
            mode,
            chunk_seconds: 300.0,
            overlap_seconds: 2.0,
            group: GroupOptions::default(),
        }
    }
}

/// How much a run will send, for telling the person before it starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// Requests that will be made.
    pub requests: usize,
    /// Audio that will be uploaded, in megabytes, overlap included.
    pub megabytes: f64,
}

/// What `total` seconds of audio costs in requests and upload.
pub fn estimate(total: f64, options: &JobOptions) -> Estimate {
    let chunks = chunk::plan(total, options.chunk_seconds, options.overlap_seconds);
    let seconds: f64 = chunks.iter().map(Chunk::len).sum();
    Estimate {
        requests: chunks.len(),
        megabytes: seconds * f64::from(SAMPLE_RATE) * 2.0 / 1_048_576.0,
    }
}

/// Where a run is, for the progress bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    /// Reading window `n` of `of` from the file.
    Reading {
        /// 1-based.
        n: usize,
        /// Windows in all.
        of: usize,
    },
    /// Waiting on Gemini for window `n` of `of`.
    Waiting {
        /// 1-based.
        n: usize,
        /// Windows in all.
        of: usize,
    },
    /// Putting the windows together.
    Finishing,
}

/// A finished run.
#[derive(Clone, Debug)]
pub struct Output {
    /// Cues on the audio's clock, checked.
    pub cues: Vec<Cue>,
    /// What checking changed.
    pub report: Report,
    /// Requests made.
    pub requests: usize,
}

/// Runs a whole transcription of `total` seconds. `read(start, len)` gives
/// that span as 16 kHz mono floats. Nothing is returned for a run that fails
/// or is cancelled part-way: a half-written subtitle is worse than none.
pub fn run(
    client: &Client,
    options: &JobOptions,
    total: f64,
    read: &mut dyn FnMut(f64, f64) -> Result<Vec<f32>, String>,
    progress: &mut dyn FnMut(Stage),
) -> Result<Output, GeminiError> {
    let chunks = chunk::plan(total, options.chunk_seconds, options.overlap_seconds);
    if chunks.is_empty() {
        return Err(GeminiError::BadAudio("the clip covers no time".into()));
    }
    let cancel = client.cancel_handle();
    let of = chunks.len();
    let mut word_parts = Vec::new();
    let mut cue_parts = Vec::new();
    let mut requests = 0;
    for (at, chunk) in chunks.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(GeminiError::Cancelled);
        }
        progress(Stage::Reading { n: at + 1, of });
        let samples = read(chunk.start, chunk.len()).map_err(GeminiError::BadAudio)?;
        if samples.is_empty() {
            return Err(GeminiError::BadAudio("the clip has no audio".into()));
        }
        let audio = AudioInput::inline("audio/wav", &wav::encode_pcm16(&samples, SAMPLE_RATE));
        progress(Stage::Waiting { n: at + 1, of });
        let language = options.language.as_deref();
        let reply = match options.mode {
            Mode::WordTimestamps => {
                client.interact(&transcribe_body(&options.model, &audio, language, true))?
            }
            Mode::TimedCues => client.interact(&cues_body(&options.model, &audio, language))?,
        };
        requests += 1;
        match options.mode {
            Mode::WordTimestamps => {
                let words = word_infos(&reply)?;
                if words.is_empty() && !output_text(&reply)?.trim().is_empty() {
                    return Err(GeminiError::BadTiming(
                        "the model returned text without word timings; try a general model".into(),
                    ));
                }
                word_parts.push((*chunk, words));
            }
            Mode::TimedCues => {
                cue_parts.push((*chunk, parse_cues(&output_text(&reply)?)?));
            }
        }
    }
    progress(Stage::Finishing);
    let mut cues = match options.mode {
        Mode::WordTimestamps => group_words(&chunk::merge_words(&word_parts), &options.group),
        Mode::TimedCues => chunk::merge_cues(&cue_parts),
    };
    let report = validate(&mut cues, total);
    Ok(Output {
        cues,
        report,
        requests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ApiKey;
    use crate::client::tests::mock;

    fn leak(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    fn words_reply(words: &[(&str, f64, f64)]) -> &'static str {
        let notes: Vec<String> = words
            .iter()
            .map(|(t, s, e)| {
                format!(
                    r#"{{"type":"word_info","text":"{t}","start_offset":"{s}s","end_offset":"{e}s"}}"#
                )
            })
            .collect();
        let text: Vec<&str> = words.iter().map(|w| w.0).collect();
        leak(format!(
            r#"{{"status":"completed","steps":[{{"type":"model_output","content":[{{"type":"text","text":"{}","annotations":[{}]}}]}}]}}"#,
            text.join(" "),
            notes.join(",")
        ))
    }

    fn silence(len: f64) -> Vec<f32> {
        vec![0.0; (len * f64::from(SAMPLE_RATE)) as usize]
    }

    #[test]
    fn a_short_clip_becomes_persian_cues() {
        let m = mock(vec![(
            200,
            "",
            words_reply(&[("سلام", 0.1, 0.5), ("دنیا،", 0.6, 1.0), ("خوبی؟", 1.2, 1.8)]),
        )]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let options = JobOptions::new("gemini-3.5-transcribe", Some("fa-IR"), Mode::WordTimestamps);
        let mut stages = Vec::new();
        let out = run(
            &client,
            &options,
            6.6,
            &mut |_, len| Ok(silence(len)),
            &mut |s| stages.push(s),
        )
        .unwrap();
        assert_eq!(out.requests, 1);
        assert_eq!(out.cues.len(), 1);
        assert_eq!(out.cues[0].text, "سلام دنیا، خوبی؟");
        assert_eq!(out.cues[0].words.len(), 3);
        assert!(out.report.is_clean());
        assert_eq!(stages.first(), Some(&Stage::Reading { n: 1, of: 1 }));
        assert_eq!(stages.last(), Some(&Stage::Finishing));
        // What was sent: the language, word timestamps, and a WAV.
        let seen = m.seen.lock().unwrap()[0].clone();
        assert!(seen.contains("fa-IR"));
        assert!(seen.contains("timestamp_granularities"));
        assert!(seen.contains("UklGR"), "base64 of RIFF");
    }

    #[test]
    fn two_windows_join_without_a_repeat() {
        let options = {
            let mut o = JobOptions::new("m", None, Mode::WordTimestamps);
            o.chunk_seconds = 12.0;
            o.overlap_seconds = 4.0;
            o
        };
        let chunks = chunk::plan(20.0, 12.0, 4.0);
        assert_eq!(chunks.len(), 2);
        let off = chunks[1].start;
        let first = words_reply(&[("a", 1.0, 1.5), ("mid", 9.0, 9.5)]);
        let second = words_reply(&[("mid", 9.0 - off, 9.5 - off), ("z", 15.0 - off, 15.5 - off)]);
        let m = mock(vec![(200, "", first), (200, "", second)]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let out = run(
            &client,
            &options,
            20.0,
            &mut |_, len| Ok(silence(len)),
            &mut |_| {},
        )
        .unwrap();
        let all: Vec<&str> = out
            .cues
            .iter()
            .flat_map(|c| c.words.iter().map(|w| w.text.as_str()))
            .collect();
        assert_eq!(all, ["a", "mid", "z"]);
        assert_eq!(out.requests, 2);
    }

    #[test]
    fn text_without_word_times_is_refused() {
        let reply = leak(
            r#"{"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"hello"}]}]}"#
                .to_owned(),
        );
        let m = mock(vec![(200, "", reply)]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let options = JobOptions::new("m", None, Mode::WordTimestamps);
        let err = run(
            &client,
            &options,
            5.0,
            &mut |_, l| Ok(silence(l)),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, GeminiError::BadTiming(_)), "{err:?}");
    }

    #[test]
    fn timed_cues_are_checked_not_trusted() {
        let text = r#"{\"cues\":[{\"start\":0,\"end\":2,\"text\":\"اول\"},{\"start\":1.5,\"end\":3,\"text\":\"دوم\"},{\"start\":90,\"end\":91,\"text\":\"خارج\"}]}"#;
        let reply = leak(format!(
            r#"{{"status":"completed","steps":[{{"type":"model_output","content":[{{"type":"text","text":"{text}"}}]}}]}}"#
        ));
        let m = mock(vec![(200, "", reply)]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let options = JobOptions::new("gemini-flash", None, Mode::TimedCues);
        let out = run(
            &client,
            &options,
            10.0,
            &mut |_, l| Ok(silence(l)),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(out.cues.len(), 2, "the cue past the end is dropped");
        assert!(out.cues[0].end <= out.cues[1].start, "overlap resolved");
        assert!(!out.report.is_clean());
    }

    #[test]
    fn a_failure_returns_nothing() {
        let m = mock(vec![(429, "", "{}")]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let options = JobOptions::new("m", None, Mode::WordTimestamps);
        let err = run(
            &client,
            &options,
            5.0,
            &mut |_, l| Ok(silence(l)),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, GeminiError::Quota { .. }));
    }

    #[test]
    fn cancel_is_heard_between_windows() {
        let m = mock(vec![]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        client
            .cancel_handle()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let options = JobOptions::new("m", None, Mode::WordTimestamps);
        let err = run(
            &client,
            &options,
            5.0,
            &mut |_, l| Ok(silence(l)),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(err, GeminiError::Cancelled);
    }

    #[test]
    fn unreadable_or_empty_audio_is_a_bad_audio_error() {
        let m = mock(vec![]);
        let client = Client::with_base_url(ApiKey::new("k"), &m.base).retries(0);
        let options = JobOptions::new("m", None, Mode::WordTimestamps);
        let err = run(
            &client,
            &options,
            5.0,
            &mut |_, _| Err("boom".into()),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(err, GeminiError::BadAudio("boom".into()));
        let err = run(&client, &options, 5.0, &mut |_, _| Ok(vec![]), &mut |_| {}).unwrap_err();
        assert!(matches!(err, GeminiError::BadAudio(_)));
        let err = run(
            &client,
            &options,
            0.0,
            &mut |_, l| Ok(silence(l)),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, GeminiError::BadAudio(_)));
    }

    #[test]
    fn the_estimate_counts_overlap() {
        let options = JobOptions::new("m", None, Mode::WordTimestamps);
        let one = estimate(60.0, &options);
        assert_eq!(one.requests, 1);
        assert!((one.megabytes - 60.0 * 32_000.0 / 1_048_576.0).abs() < 1e-9);
        let many = estimate(3600.0, &options);
        assert!(many.requests >= 12);
        assert!(many.megabytes > one.megabytes * 59.0);
    }
}
