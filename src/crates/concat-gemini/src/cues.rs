// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Words into cues: where a subtitle ends and its lines break.

use crate::{Cue, Word};

/// How words are grouped. A cue ends at the first rule that says so.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupOptions {
    /// A silence this long between two words ends the cue, in seconds.
    pub max_gap: f64,
    /// The longest a cue stays on screen, in seconds.
    pub max_duration: f64,
    /// The most characters in one cue.
    pub max_chars: usize,
    /// A sentence end ends the cue once it has this many characters.
    pub min_chars_at_sentence_end: usize,
    /// A cue longer than this is set on two lines.
    pub line_chars: usize,
}

impl Default for GroupOptions {
    fn default() -> Self {
        GroupOptions {
            max_gap: 0.8,
            max_duration: 6.0,
            max_chars: 70,
            min_chars_at_sentence_end: 12,
            line_chars: 38,
        }
    }
}

fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', ')', '»', '”'])
        .ends_with(['.', '!', '?', '؟', '۔', '…', '。', '！', '？'])
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// Cues from timed words. Word timings are kept on each cue, and a cue's
/// own time is its first word's start to its last word's end.
pub fn group_words(words: &[Word], options: &GroupOptions) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let mut length = 0;
    for word in words {
        if let Some(last) = current.last() {
            let gap = word.start - last.end;
            let next_len = length + 1 + chars(&word.text);
            let span = word.end - current[0].start;
            if gap > options.max_gap || next_len > options.max_chars || span > options.max_duration
            {
                cues.push(finish(std::mem::take(&mut current), options));
                length = 0;
            }
        }
        length += if current.is_empty() { 0 } else { 1 } + chars(&word.text);
        current.push(word.clone());
        if ends_sentence(&word.text) && length >= options.min_chars_at_sentence_end {
            cues.push(finish(std::mem::take(&mut current), options));
            length = 0;
        }
    }
    if !current.is_empty() {
        cues.push(finish(current, options));
    }
    cues
}

fn finish(words: Vec<Word>, options: &GroupOptions) -> Cue {
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Cue {
        start: words[0].start,
        end: words[words.len() - 1].end,
        text: break_lines(&text, options.line_chars),
        words,
    }
}

/// Sets `text` on two lines at the space nearest its middle when it is
/// longer than `line_chars`. Only spaces are break points, so a half-space
/// (ZWNJ) inside a Persian word is never split.
pub fn break_lines(text: &str, line_chars: usize) -> String {
    if chars(text) <= line_chars || text.contains('\n') {
        return text.to_owned();
    }
    let middle = chars(text) / 2;
    let mut best: Option<(usize, usize)> = None;
    for (at, (byte, c)) in text.char_indices().enumerate() {
        if c == ' ' {
            let distance = at.abs_diff(middle);
            if best.is_none_or(|(_, d)| distance < d) {
                best = Some((byte, distance));
            }
        }
    }
    match best {
        Some((byte, _)) => format!("{}\n{}", &text[..byte], &text[byte + 1..]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.into(),
            start,
            end,
        }
    }

    #[test]
    fn a_pause_ends_a_cue() {
        let words = [w("one", 0.0, 0.4), w("two", 0.5, 0.9), w("three", 3.0, 3.4)];
        let cues = group_words(&words, &GroupOptions::default());
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "one two");
        assert_eq!((cues[0].start, cues[0].end), (0.0, 0.9));
        assert_eq!(cues[1].words.len(), 1);
    }

    #[test]
    fn a_sentence_end_ends_a_cue_in_persian_too() {
        let words = [
            w("امروز", 0.0, 0.4),
            w("هوا", 0.5, 0.8),
            w("خوب", 0.9, 1.2),
            w("است؟", 1.3, 1.7),
            w("بله", 1.8, 2.1),
        ];
        let cues = group_words(&words, &GroupOptions::default());
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "امروز هوا خوب است؟");
        assert_eq!(cues[1].text, "بله");
    }

    #[test]
    fn a_long_run_is_cut_by_duration() {
        let words: Vec<Word> = (0..20)
            .map(|i| w("word", i as f64 * 0.5, i as f64 * 0.5 + 0.4))
            .collect();
        let cues = group_words(&words, &GroupOptions::default());
        assert!(cues.len() >= 2);
        assert!(cues.iter().all(|c| c.end - c.start <= 6.0 + 1e-9));
        let total: usize = cues.iter().map(|c| c.words.len()).sum();
        assert_eq!(total, 20, "no word lost");
    }

    #[test]
    fn two_lines_split_at_a_space_and_keep_the_half_space() {
        let text = "این یک جمله‌ی بسیار طولانی برای آزمایش شکستن خط است";
        let broken = break_lines(text, 20);
        assert_eq!(broken.matches('\n').count(), 1);
        assert!(broken.contains("جمله‌ی"), "ZWNJ word stays whole");
        assert_eq!(broken.replace('\n', " "), text);
        assert_eq!(break_lines("short", 20), "short");
        assert_eq!(
            break_lines("oneverylongwordwithnospaces", 5),
            "oneverylongwordwithnospaces"
        );
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(group_words(&[], &GroupOptions::default()).is_empty());
    }
}
