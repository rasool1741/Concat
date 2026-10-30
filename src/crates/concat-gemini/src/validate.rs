// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Checking cues before they reach the timeline.
//!
//! Nothing is invented here: a bad cue is dropped or trimmed and reported,
//! so the person sees what was changed and why.

use crate::Cue;

/// One thing found and what was done about it.
#[derive(Clone, Debug, PartialEq)]
pub enum Issue {
    /// A cue with no words; dropped.
    Empty {
        /// Its start, seconds.
        at: f64,
    },
    /// A cue that ends before it starts or has a time that is not a number;
    /// dropped.
    BadTime {
        /// Its start, seconds.
        at: f64,
    },
    /// A cue wholly outside the audio; dropped.
    OutsideAudio {
        /// Its start, seconds.
        at: f64,
    },
    /// A cue that runs past the end of the audio; trimmed to it.
    PastEnd {
        /// Its start, seconds.
        at: f64,
    },
    /// A cue that started before the one ahead ended; that one was
    /// shortened to make room.
    Overlap {
        /// The later cue's start, seconds.
        at: f64,
    },
    /// The same words at the same time twice; one dropped.
    Duplicate {
        /// Its start, seconds.
        at: f64,
    },
}

/// What validation found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Everything found, in cue order.
    pub issues: Vec<Issue>,
}

impl Report {
    /// Nothing needed changing.
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// Cues thrown away.
    pub fn dropped(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    Issue::Empty { .. }
                        | Issue::BadTime { .. }
                        | Issue::OutsideAudio { .. }
                        | Issue::Duplicate { .. }
                )
            })
            .count()
    }
}

/// The shortest a cue may be after another trims it, in seconds.
const MIN_LEN: f64 = 0.2;

/// Cleans `cues` for audio `total` seconds long, in place: sorted, no
/// empty, backwards, repeated or overlapping cues. Returns what changed.
pub fn validate(cues: &mut Vec<Cue>, total: f64) -> Report {
    let mut report = Report::default();
    let mut kept: Vec<Cue> = Vec::with_capacity(cues.len());
    for mut cue in cues.drain(..) {
        if !cue.start.is_finite() || !cue.end.is_finite() || cue.end < cue.start || cue.start < 0.0
        {
            report.issues.push(Issue::BadTime { at: cue.start });
            continue;
        }
        if cue.text.trim().is_empty() {
            report.issues.push(Issue::Empty { at: cue.start });
            continue;
        }
        if cue.start >= total {
            report.issues.push(Issue::OutsideAudio { at: cue.start });
            continue;
        }
        if cue.end > total {
            cue.end = total;
            report.issues.push(Issue::PastEnd { at: cue.start });
        }
        kept.push(cue);
    }
    kept.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Cue> = Vec::with_capacity(kept.len());
    for cue in kept {
        if let Some(prev) = out.last_mut() {
            if (prev.start - cue.start).abs() < 1e-6 && prev.text == cue.text {
                report.issues.push(Issue::Duplicate { at: cue.start });
                continue;
            }
            if cue.start < prev.end {
                report.issues.push(Issue::Overlap { at: cue.start });
                if cue.start - prev.start >= MIN_LEN {
                    prev.end = cue.start;
                } else {
                    // No room to trim the earlier one; push this one later.
                    let mut moved = cue;
                    moved.start = prev.end;
                    if moved.end < moved.start {
                        moved.end = moved.start;
                    }
                    out.push(moved);
                    continue;
                }
            }
        }
        out.push(cue);
    }
    *cues = out;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: f64, end: f64, text: &str) -> Cue {
        Cue {
            start,
            end,
            text: text.into(),
            words: Vec::new(),
        }
    }

    #[test]
    fn good_cues_pass_untouched() {
        let mut cues = vec![cue(0.0, 1.0, "a"), cue(1.0, 2.0, "b")];
        let before = cues.clone();
        assert!(validate(&mut cues, 10.0).is_clean());
        assert_eq!(cues, before);
    }

    #[test]
    fn bad_ones_are_dropped_and_reported() {
        let mut cues = vec![
            cue(0.0, 1.0, "  "),
            cue(3.0, 2.0, "backwards"),
            cue(f64::NAN, 1.0, "nan"),
            cue(50.0, 51.0, "after the end"),
            cue(1.0, 2.0, "ok"),
        ];
        let report = validate(&mut cues, 10.0);
        assert_eq!(cues.len(), 1);
        assert_eq!(report.dropped(), 4);
    }

    #[test]
    fn a_cue_past_the_end_is_trimmed() {
        let mut cues = vec![cue(8.0, 12.0, "late")];
        let report = validate(&mut cues, 10.0);
        assert_eq!(cues[0].end, 10.0);
        assert_eq!(report.issues, vec![Issue::PastEnd { at: 8.0 }]);
    }

    #[test]
    fn overlaps_are_resolved_and_sorted() {
        let mut cues = vec![cue(2.0, 4.0, "b"), cue(0.0, 3.0, "a")];
        let report = validate(&mut cues, 10.0);
        assert_eq!(cues[0].text, "a");
        assert_eq!(cues[0].end, 2.0, "the earlier cue makes room");
        assert_eq!(report.issues, vec![Issue::Overlap { at: 2.0 }]);
        for pair in cues.windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
    }

    #[test]
    fn a_stacked_pair_is_pushed_not_crushed() {
        let mut cues = vec![cue(1.0, 3.0, "a"), cue(1.05, 2.0, "b")];
        validate(&mut cues, 10.0);
        assert_eq!(cues.len(), 2);
        assert!(cues[1].start >= cues[0].end);
    }

    #[test]
    fn duplicates_collapse() {
        let mut cues = vec![cue(1.0, 2.0, "same"), cue(1.0, 2.0, "same")];
        let report = validate(&mut cues, 10.0);
        assert_eq!(cues.len(), 1);
        assert_eq!(report.dropped(), 1);
    }
}
