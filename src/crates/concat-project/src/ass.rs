// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Advanced SubStation Alpha (`.ass`) subtitles from a lane of title clips.
//!
//! SubRip keeps words and times; ASS keeps the look too: font, size, colour,
//! outline, shadow, plate, alignment and position. Each title clip becomes
//! one event with a style of its own, so a lane that mixes looks exports
//! each as it is.
//!
//! The look is carried as far as ASS can carry it, and what it cannot is
//! reported in [`Export::warnings`] rather than dropped without a word: an
//! ASS box and an outline share one colour, for instance, so a title with
//! both keeps the box and says the outline was not written.
//!
//! The words are written as they are, in Unicode. A player's own text engine
//! (libass) shapes Arabic-script text and runs the bidirectional algorithm,
//! so nothing here reorders a character.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::model::{Clip, ClipKind, TextAlign, TextStyle, Timeline};

/// An `.ass` file and what it could not carry.
#[derive(Clone, Debug, PartialEq)]
pub struct Export {
    /// The file's text, ready to write as UTF-8.
    pub text: String,
    /// What the format cannot hold, one line each, for the person to read
    /// before relying on the file.
    pub warnings: Vec<String>,
    /// How many events were written.
    pub events: usize,
}

/// Writes the title clips of one lane (or of every lane when `track_id` is
/// `None`) as ASS, for a frame `width` by `height` pixels.
pub fn write(timeline: &Timeline, track_id: Option<&str>, width: u32, height: u32) -> Export {
    let (width, height) = (width.max(1), height.max(1));
    let mut clips: Vec<&Clip> = timeline
        .clips
        .iter()
        .map(|clip| &**clip)
        .filter(|clip| clip.kind == ClipKind::Text)
        .filter(|clip| track_id.is_none_or(|id| clip.track_id == id))
        .filter(|clip| clip.text.is_some())
        .collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));

    let mut warnings: Vec<String> = Vec::new();
    let mut note = |text: &str| {
        if !warnings.iter().any(|w| w == text) {
            warnings.push(text.to_owned());
        }
    };

    let mut styles: Vec<(String, String)> = Vec::new();
    let mut by_line: HashMap<String, String> = HashMap::new();
    let mut events = String::new();

    for clip in &clips {
        let Some(style) = clip.text.as_ref() else {
            continue;
        };
        if style.background_radius > 0.0 && !style.background.is_empty() {
            note("A plate's rounded corners are written square: ASS boxes have no corner radius.");
        }
        if !style.background.is_empty() && style.stroke_width > 0.0 {
            note(
                "A title with a plate and an outline keeps the plate: ASS draws both in one colour.",
            );
        }
        if style.shadow {
            note("The shadow is written with ASS's hard offset: its softness is not carried.");
        }
        if clip.rotation.abs() > f64::EPSILON {
            note("A rotated title is written upright: rotation is not carried.");
        }
        if !clip.keys.is_empty() {
            note("Keyframed motion and fades are not carried: ASS gets the title still.");
        }
        let line = style_line(style, clip, width, height);
        let name = match by_line.get(&line) {
            Some(name) => name.clone(),
            None => {
                let name = format!("S{}", styles.len() + 1);
                by_line.insert(line.clone(), name.clone());
                styles.push((name.clone(), line));
                name
            }
        };
        let _ = writeln!(
            events,
            "Dialogue: 0,{},{},{},,0,0,0,,{}{}",
            stamp(clip.start),
            stamp(clip.start + clip.duration),
            name,
            position_tag(style, clip, width, height),
            escape(&style.content),
        );
    }

    let mut text = String::new();
    let _ = writeln!(text, "[Script Info]");
    let _ = writeln!(text, "; Written by Concat");
    let _ = writeln!(text, "ScriptType: v4.00+");
    let _ = writeln!(text, "PlayResX: {width}");
    let _ = writeln!(text, "PlayResY: {height}");
    let _ = writeln!(text, "WrapStyle: 0");
    let _ = writeln!(text, "ScaledBorderAndShadow: yes");
    let _ = writeln!(text);
    let _ = writeln!(text, "[V4+ Styles]");
    let _ = writeln!(
        text,
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, \
Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding"
    );
    for (name, line) in &styles {
        let _ = writeln!(text, "Style: {name},{line}");
    }
    let _ = writeln!(text);
    let _ = writeln!(text, "[Events]");
    let _ = writeln!(
        text,
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"
    );
    text.push_str(&events);
    Export {
        text,
        warnings,
        events: clips.len(),
    }
}

/// The fields of one style after its name.
fn style_line(style: &TextStyle, clip: &Clip, width: u32, height: u32) -> String {
    let h = f64::from(height);
    let w = f64::from(width);
    let alpha_scale = (style.opacity * clip.opacity).clamp(0.0, 1.0);
    let primary = colour(&style.color, alpha_scale, "#ffffff");
    let has_plate = !style.background.is_empty();
    // BorderStyle 3 draws a box in the outline colour; 1 an outline.
    let (border_style, outline_colour, outline) = if has_plate {
        let padding = style.background_padding_y.max(style.background_padding_x) * h;
        (
            3,
            colour(&style.background, alpha_scale, "#000000"),
            padding,
        )
    } else {
        (
            1,
            colour(&style.stroke_color, alpha_scale, "#000000"),
            style.stroke_width * h,
        )
    };
    let shadow = if style.shadow {
        (h * 0.004).max(1.0)
    } else {
        0.0
    };
    let back = colour("#000000", 0.55 * alpha_scale, "#000000");
    let scale = (clip.scale * 100.0).max(1.0);
    let margin = if style.max_width > 0.0 {
        ((w - style.max_width * w * clip.scale) / 2.0).max(0.0)
    } else {
        0.0
    };
    format!(
        "{font},{size:.1},{primary},{primary},{outline_colour},{back},{bold},{italic},0,0,\
{scale:.0},{scale:.0},{spacing:.2},0,{border_style},{outline:.2},{shadow:.2},{an},\
{margin:.0},{margin:.0},0,1",
        font = font_name(&style.font_family),
        size = (style.font_size * h).max(1.0),
        bold = if style.font_weight >= 600.0 { -1 } else { 0 },
        italic = if style.italic { -1 } else { 0 },
        spacing = style.tracking * h,
        an = alignment(style.align),
    )
}

/// The numpad alignment of the block's anchor: its middle row, left, centre
/// or right, which is where a title's position sits.
fn alignment(align: TextAlign) -> u8 {
    match align {
        TextAlign::Left => 4,
        TextAlign::Center => 5,
        TextAlign::Right => 6,
    }
}

/// `{\pos(x,y)}` for the anchor: the frame's centre moved by the clip's
/// offset, as fractions of the frame.
fn position_tag(style: &TextStyle, clip: &Clip, width: u32, height: u32) -> String {
    let w = f64::from(width);
    let h = f64::from(height);
    let x = w / 2.0 + clip.offset_x * w;
    let y = h / 2.0 + clip.offset_y * h;
    let _ = style;
    format!("{{\\pos({x:.0},{y:.0})}}")
}

/// A family without the quotes a name with spaces carries; the first of a
/// comma-separated list, since ASS takes one.
fn font_name(family: &str) -> String {
    let first = family.split(',').next().unwrap_or("").trim();
    let name = first.trim_matches(|c| c == '"' || c == '\'').trim();
    if name.is_empty() {
        "Arial".to_owned()
    } else {
        name.replace(',', " ")
    }
}

/// `#rrggbb[aa]` as ASS's `&HAABBGGRR`, where alpha 00 is opaque. `scale`
/// multiplies the colour's own opacity.
fn colour(css: &str, scale: f64, fallback: &str) -> String {
    let parse = |text: &str| -> Option<(u8, u8, u8, u8)> {
        let hex = text.trim().strip_prefix('#')?;
        let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
        match hex.len() {
            6 => Some((byte(0)?, byte(2)?, byte(4)?, 255)),
            8 => Some((byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    };
    let (r, g, b, a) = parse(css)
        .or_else(|| parse(fallback))
        .unwrap_or((255, 255, 255, 255));
    let opacity = (f64::from(a) / 255.0 * scale).clamp(0.0, 1.0);
    let alpha = ((1.0 - opacity) * 255.0).round() as u8;
    format!("&H{alpha:02X}{b:02X}{g:02X}{r:02X}")
}

/// Seconds as `H:MM:SS.cc`, centiseconds, below zero is zero.
fn stamp(seconds: f64) -> String {
    let centis = (seconds.max(0.0) * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        centis / 360_000,
        centis / 6_000 % 60,
        centis / 100 % 60,
        centis % 100
    )
}

/// A title's words as an ASS text field: a line break is `\N`, and a brace,
/// which would open an override block, is escaped.
fn escape(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    for (at, line) in lines.iter().enumerate() {
        if at > 0 {
            out.push_str("\\N");
        }
        for c in line.chars() {
            match c {
                '{' => out.push_str("\\{"),
                '}' => out.push_str("\\}"),
                '\\' => out.push_str("\\\\"),
                c => out.push(c),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Command;
    use crate::editor::Editor;

    fn lane_with(styles: &[(f64, f64, TextStyle)]) -> (Editor, String) {
        let mut editor = Editor::new();
        let mut lane = String::new();
        for (start, duration, style) in styles {
            let id = editor
                .apply(Command::AddTextClip {
                    track_id: if lane.is_empty() {
                        None
                    } else {
                        Some(lane.clone())
                    },
                    above: true,
                    start: *start,
                    style: Some(style.clone()),
                    duration: Some(*duration),
                    offset_y: Some(0.35),
                })
                .expect("adds")
                .created_id
                .expect("id");
            lane = editor
                .project()
                .active()
                .clip(&id)
                .expect("exists")
                .track_id
                .clone();
        }
        (editor, lane)
    }

    fn words(content: &str) -> TextStyle {
        TextStyle {
            content: content.to_owned(),
            ..TextStyle::default()
        }
    }

    #[test]
    fn times_and_colours_use_assas_forms() {
        assert_eq!(stamp(0.0), "0:00:00.00");
        assert_eq!(stamp(3725.5), "1:02:05.50");
        assert_eq!(stamp(-4.0), "0:00:00.00");
        // ASS is blue-green-red, and alpha 00 is opaque.
        assert_eq!(colour("#ff8000", 1.0, "#000000"), "&H000080FF");
        assert_eq!(colour("#ffffff80", 1.0, "#000000"), "&H7FFFFFFF");
        assert_eq!(colour("not a colour", 1.0, "#112233"), "&H00332211");
        assert_eq!(colour("#ffffff", 0.0, "#000000"), "&HFFFFFFFF");
    }

    #[test]
    fn a_persian_cue_is_written_as_it_is() {
        let (editor, lane) = lane_with(&[(1.0, 2.0, words("سلام دنیا\nخوش آمدید"))]);
        let out = write(editor.project().active(), Some(&lane), 1920, 1080);
        assert_eq!(out.events, 1);
        assert!(out.text.contains("PlayResX: 1920"));
        assert!(
            out.text
                .contains("Dialogue: 0,0:00:01.00,0:00:03.00,S1,,0,0,0,,")
        );
        assert!(out.text.contains("سلام دنیا\\Nخوش آمدید"));
        assert!(out.text.contains("{\\pos(960,"), "{}", out.text);
    }

    #[test]
    fn the_half_space_survives() {
        let (editor, lane) = lane_with(&[(0.0, 1.0, words("می‌روم"))]);
        let out = write(editor.project().active(), Some(&lane), 1280, 720);
        assert!(out.text.contains("می\u{200c}روم"));
    }

    #[test]
    fn like_styles_share_one_style_and_unlike_get_their_own() {
        let big = TextStyle {
            font_size: 0.12,
            ..words("b")
        };
        let (editor, lane) = lane_with(&[
            (0.0, 1.0, words("a")),
            (1.0, 1.0, words("a2")),
            (2.0, 1.0, big),
        ]);
        let out = write(editor.project().active(), Some(&lane), 1920, 1080);
        assert_eq!(out.text.matches("\nStyle: ").count(), 2);
        assert!(out.text.contains(",S1,,"));
        assert!(out.text.contains(",S2,,"));
    }

    #[test]
    fn braces_and_backslashes_cannot_open_an_override() {
        assert_eq!(escape("a{b}c\\d"), "a\\{b\\}c\\\\d");
        assert_eq!(escape("one\n\n two \n"), "one\\N two");
    }

    #[test]
    fn what_ass_cannot_carry_is_reported() {
        let rich = TextStyle {
            background: "#000000cc".into(),
            background_radius: 0.02,
            stroke_width: 0.01,
            shadow: true,
            ..words("x")
        };
        let (editor, lane) = lane_with(&[(0.0, 1.0, rich)]);
        let out = write(editor.project().active(), Some(&lane), 1920, 1080);
        assert!(out.warnings.iter().any(|w| w.contains("rounded corners")));
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("plate and an outline"))
        );
        assert!(out.warnings.iter().any(|w| w.contains("shadow")));
        // The plate is a BorderStyle 3 box.
        let style = out
            .text
            .lines()
            .find(|l| l.starts_with("Style: S1,"))
            .unwrap();
        assert!(style.contains(",3,"), "{style}");
    }

    #[test]
    fn alignment_follows_the_title() {
        let left = TextStyle {
            align: TextAlign::Left,
            ..words("l")
        };
        let right = TextStyle {
            align: TextAlign::Right,
            ..words("r")
        };
        let (editor, lane) = lane_with(&[(0.0, 1.0, left), (1.0, 1.0, right)]);
        let out = write(editor.project().active(), Some(&lane), 1920, 1080);
        let styles: Vec<&str> = out
            .text
            .lines()
            .filter(|l| l.starts_with("Style: "))
            .collect();
        assert!(styles[0].contains(",4,"), "{}", styles[0]);
        assert!(styles[1].contains(",6,"), "{}", styles[1]);
    }

    #[test]
    fn other_lanes_are_left_out() {
        let (mut editor, lane) = lane_with(&[(0.0, 1.0, words("on the lane"))]);
        editor
            .apply(Command::AddTextClip {
                track_id: None,
                above: true,
                start: 0.0,
                style: Some(words("elsewhere")),
                duration: Some(1.0),
                offset_y: None,
            })
            .expect("adds");
        let out = write(editor.project().active(), Some(&lane), 1920, 1080);
        assert_eq!(out.events, 1);
        assert!(!out.text.contains("elsewhere"));
        assert_eq!(write(editor.project().active(), None, 1920, 1080).events, 2);
    }
}
