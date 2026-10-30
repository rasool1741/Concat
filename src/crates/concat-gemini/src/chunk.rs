// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Long audio in overlapping windows, and the words put back together.
//!
//! A window overlaps the next by a fixed span so a word cut by one window's
//! edge is whole in the other. The seam between two windows is the middle of
//! their overlap: each window keeps the words whose midpoint falls on its
//! side of the seam, so no word is dropped at a seam and none is said twice.

use crate::{Cue, Word};

/// One window of the audio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chunk {
    /// Seconds into the audio where the window starts.
    pub start: f64,
    /// Seconds into the audio where it ends.
    pub end: f64,
    /// The first second this window is trusted for.
    pub keep_from: f64,
    /// The last second this window is trusted for.
    pub keep_to: f64,
}

impl Chunk {
    /// The window's length in seconds.
    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    /// A window never has no length, but clippy wants the pair.
    pub fn is_empty(&self) -> bool {
        self.len() <= 0.0
    }
}

/// Windows over `total` seconds, each `chunk_len` long and overlapping the
/// next by `overlap`. A tail shorter than twice the overlap is folded into
/// the last window rather than sent alone.
pub fn plan(total: f64, chunk_len: f64, overlap: f64) -> Vec<Chunk> {
    if !total.is_finite() || total <= 0.0 {
        return Vec::new();
    }
    let overlap = overlap.max(0.0);
    let chunk_len = chunk_len.max(overlap * 2.0 + 1.0);
    if total <= chunk_len {
        return vec![Chunk {
            start: 0.0,
            end: total,
            keep_from: 0.0,
            keep_to: total,
        }];
    }
    let step = chunk_len - overlap;
    let mut starts = vec![0.0];
    loop {
        let next = starts.len() as f64 * step;
        if total - next <= overlap * 2.0 || next >= total {
            break;
        }
        starts.push(next);
    }
    let mut chunks: Vec<Chunk> = starts
        .iter()
        .enumerate()
        .map(|(at, &start)| {
            let last = at + 1 == starts.len();
            Chunk {
                start,
                end: if last {
                    total
                } else {
                    (start + chunk_len).min(total)
                },
                keep_from: 0.0,
                keep_to: total,
            }
        })
        .collect();
    for at in 1..chunks.len() {
        let seam = chunks[at].start + (chunks[at - 1].end - chunks[at].start) / 2.0;
        chunks[at - 1].keep_to = seam;
        chunks[at].keep_from = seam;
    }
    chunks
}

/// The words of every window as one list on the whole audio's clock.
/// `parts` pairs a window with the words it returned, times counted from the
/// window's own start.
pub fn merge_words(parts: &[(Chunk, Vec<Word>)]) -> Vec<Word> {
    let mut all = Vec::new();
    for (chunk, words) in parts {
        for word in words {
            let start = word.start + chunk.start;
            let end = word.end + chunk.start;
            let middle = (start + end) / 2.0;
            if middle >= chunk.keep_from && (middle < chunk.keep_to || chunk.keep_to >= chunk.end) {
                all.push(Word {
                    text: word.text.clone(),
                    start,
                    end,
                });
            }
        }
    }
    all.sort_by(|a, b| a.start.total_cmp(&b.start));
    all
}

/// The same for cues, for a model that returns cues and not words.
pub fn merge_cues(parts: &[(Chunk, Vec<Cue>)]) -> Vec<Cue> {
    let mut all = Vec::new();
    for (chunk, cues) in parts {
        for cue in cues {
            let start = cue.start + chunk.start;
            let end = cue.end + chunk.start;
            let middle = (start + end) / 2.0;
            if middle >= chunk.keep_from && (middle < chunk.keep_to || chunk.keep_to >= chunk.end) {
                all.push(Cue {
                    start,
                    end,
                    text: cue.text.clone(),
                    words: cue
                        .words
                        .iter()
                        .map(|w| Word {
                            text: w.text.clone(),
                            start: w.start + chunk.start,
                            end: w.end + chunk.start,
                        })
                        .collect(),
                });
            }
        }
    }
    all.sort_by(|a, b| a.start.total_cmp(&b.start));
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.into(),
            start,
            end,
        }
    }

    #[test]
    fn short_audio_is_one_window() {
        let chunks = plan(120.0, 300.0, 2.0);
        assert_eq!(chunks.len(), 1);
        assert_eq!((chunks[0].start, chunks[0].end), (0.0, 120.0));
    }

    #[test]
    fn windows_cover_everything_and_overlap() {
        let chunks = plan(1000.0, 300.0, 4.0);
        assert!(chunks.len() >= 4);
        assert_eq!(chunks[0].start, 0.0);
        assert_eq!(chunks.last().unwrap().end, 1000.0);
        for pair in chunks.windows(2) {
            assert!(pair[1].start < pair[0].end, "windows must overlap");
            assert_eq!(pair[0].keep_to, pair[1].keep_from, "one seam");
            assert!(pair[0].keep_to > pair[1].start && pair[0].keep_to < pair[0].end);
        }
        assert!(chunks.iter().all(|c| c.len() <= 300.0 + 8.0));
    }

    #[test]
    fn a_tiny_tail_is_folded_in() {
        let chunks = plan(302.0, 300.0, 4.0);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn bad_totals_plan_nothing() {
        assert!(plan(0.0, 300.0, 2.0).is_empty());
        assert!(plan(f64::NAN, 300.0, 2.0).is_empty());
    }

    /// A word spoken across a seam appears once, on the whole clock.
    #[test]
    fn a_word_in_the_overlap_is_kept_once() {
        let chunks = plan(20.0, 12.0, 4.0);
        assert!(chunks.len() >= 2);
        // Both windows hear "mid" at 9.0..9.5 on the whole clock.
        let first = (chunks[0], vec![word("a", 1.0, 1.5), word("mid", 9.0, 9.5)]);
        let second_offset = chunks[1].start;
        let second = (
            chunks[1],
            vec![
                word("mid", 9.0 - second_offset, 9.5 - second_offset),
                word("z", 15.0 - second_offset, 15.5 - second_offset),
            ],
        );
        let merged = merge_words(&[first, second]);
        let texts: Vec<_> = merged.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["a", "mid", "z"]);
        assert!((merged[1].start - 9.0).abs() < 1e-9);
    }

    #[test]
    fn nothing_is_lost_when_each_window_hears_all_its_words() {
        let total = 100.0;
        let chunks = plan(total, 30.0, 4.0);
        // One word per second on the whole clock.
        let truth: Vec<Word> = (0..100)
            .map(|i| word(&format!("w{i}"), i as f64 + 0.1, i as f64 + 0.9))
            .collect();
        let parts: Vec<_> = chunks
            .iter()
            .map(|c| {
                let local = truth
                    .iter()
                    .filter(|w| w.start >= c.start && w.end <= c.end)
                    .map(|w| word(&w.text, w.start - c.start, w.end - c.start))
                    .collect();
                (*c, local)
            })
            .collect();
        let merged = merge_words(&parts);
        assert_eq!(merged, truth);
    }
}
