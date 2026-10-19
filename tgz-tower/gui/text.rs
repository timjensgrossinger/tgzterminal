//! Text measurement abstraction plus ellipsis and word wrap.
//!
//! Layout only ever talks to [`Measure`], so it can be tested with a fake
//! fixed-advance measurer and driven by real shaped glyphs at paint time.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Font {
    /// View titles, drawer headline.
    Heading,
    /// Bold body: names, column headers, buttons.
    Strong,
    Body,
    Small,
    /// Bold small: chips, counters, section labels.
    SmallStrong,
}

pub trait Measure {
    /// Advance width of `text` in pixels.
    fn width(&mut self, text: &str, font: Font) -> f32;
    /// Height of one line box in pixels.
    fn line_height(&self, font: Font) -> f32;
}

pub const ELLIPSIS: &str = "…";

/// `text` if it fits in `max_w`, otherwise the longest prefix (on a char
/// boundary) followed by an ellipsis. Empty if not even the ellipsis fits.
pub fn ellipsize(m: &mut dyn Measure, text: &str, font: Font, max_w: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if m.width(text, font) <= max_w {
        return text.to_string();
    }
    let ell = m.width(ELLIPSIS, font);
    if ell > max_w {
        return String::new();
    }
    let bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).skip(1).collect();
    // Binary search the largest prefix that fits with the ellipsis.
    let (mut lo, mut hi) = (0usize, bounds.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let prefix = &text[..bounds[mid - 1]];
        if m.width(prefix.trim_end(), font) + ell <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return ELLIPSIS.to_string();
    }
    format!("{}{}", text[..bounds[lo - 1]].trim_end(), ELLIPSIS)
}

/// Greedy word wrap into at most `max_lines` lines; the last line is
/// ellipsized if text remains. Words longer than a line are ellipsized.
pub fn wrap(
    m: &mut dyn Measure,
    text: &str,
    font: Font,
    max_w: f32,
    max_lines: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    if max_lines == 0 || max_w <= 0.0 {
        return lines;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        if lines.len() + 1 == max_lines {
            let rest = words[i..].join(" ");
            lines.push(ellipsize(m, &rest, font, max_w));
            return lines;
        }
        let mut line = words[i].to_string();
        i += 1;
        if m.width(&line, font) > max_w {
            lines.push(ellipsize(m, &line, font, max_w));
            continue;
        }
        while i < words.len() {
            let candidate = format!("{} {}", line, words[i]);
            if m.width(&candidate, font) <= max_w {
                line = candidate;
                i += 1;
            } else {
                break;
            }
        }
        lines.push(line);
    }
    lines
}

/// "950", "12.4k", "1.2M".
pub fn tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 100_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// "12.4k tok · $0.03", "$0.03" or "" for an empty usage.
pub fn usage_label(input: u64, output: u64, cost: Option<f64>) -> String {
    let mut parts = Vec::new();
    if input + output > 0 {
        parts.push(format!("{} tok", tokens(input + output)));
    }
    if let Some(c) = cost {
        if c > 0.0 {
            parts.push(if c < 0.01 {
                "<$0.01".to_string()
            } else {
                format!("${:.2}", c)
            });
        }
    }
    parts.join(" · ")
}

/// "now", "4m", "1h 35m", "3d".
pub fn age(now_unix: u64, then_unix: u64) -> String {
    let s = now_unix.saturating_sub(then_unix);
    if s < 60 {
        "now".to_string()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        let m = (s % 3600) / 60;
        if m == 0 {
            format!("{}h", s / 3600)
        } else {
            format!("{}h {}m", s / 3600, m)
        }
    } else {
        format!("{}d", s / 86400)
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;

    /// Every char advances `size * 0.5`; line height is `size * 1.25`.
    pub struct FakeMeasure;

    pub fn size(font: Font) -> f32 {
        match font {
            Font::Heading => 20.0,
            Font::Strong | Font::Body => 14.0,
            Font::Small | Font::SmallStrong => 12.0,
        }
    }

    impl Measure for FakeMeasure {
        fn width(&mut self, text: &str, font: Font) -> f32 {
            text.chars().count() as f32 * size(font) * 0.5
        }
        fn line_height(&self, font: Font) -> f32 {
            size(font) * 1.25
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeMeasure;
    use super::*;

    #[test]
    fn ellipsize_keeps_fitting_text() {
        let mut m = FakeMeasure;
        assert_eq!(ellipsize(&mut m, "hello", Font::Body, 100.0), "hello");
    }

    #[test]
    fn ellipsize_cuts_and_appends() {
        let mut m = FakeMeasure;
        // 7 px per char; 50 px fits 6 chars + ellipsis (7).
        let s = ellipsize(&mut m, "abcdefghijkl", Font::Body, 50.0);
        assert!(s.ends_with(ELLIPSIS));
        assert!(m.width(&s, Font::Body) <= 50.0, "{}", s);
        assert_eq!(s, "abcdef…");
    }

    #[test]
    fn ellipsize_handles_multibyte_and_tiny_widths() {
        let mut m = FakeMeasure;
        let s = ellipsize(&mut m, "äöüßäöüß", Font::Body, 30.0);
        assert!(s.ends_with(ELLIPSIS));
        assert_eq!(ellipsize(&mut m, "abc", Font::Body, 3.0), "");
        assert_eq!(ellipsize(&mut m, "abc", Font::Body, 0.0), "");
    }

    #[test]
    fn wrap_breaks_on_words_and_limits_lines() {
        let mut m = FakeMeasure;
        let lines = wrap(&mut m, "one two three four five six", Font::Body, 70.0, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "one two");
        assert!(lines[1].ends_with(ELLIPSIS));
        let all = wrap(&mut m, "one two", Font::Body, 1000.0, 3);
        assert_eq!(all, vec!["one two".to_string()]);
        assert!(wrap(&mut m, "", Font::Body, 100.0, 3).is_empty());
    }

    #[test]
    fn token_counts_read_naturally() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_400), "12.4k");
        assert_eq!(tokens(250_000), "250k");
        assert_eq!(tokens(1_200_000), "1.2M");
        assert_eq!(usage_label(10_000, 2_400, Some(0.031)), "12.4k tok · $0.03");
        assert_eq!(usage_label(0, 0, Some(0.001)), "<$0.01");
        assert_eq!(usage_label(0, 0, None), "");
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(100, 90), "now");
        assert_eq!(age(1000, 1000 - 4 * 60), "4m");
        assert_eq!(age(10_000, 10_000 - 95 * 60), "1h 35m");
        assert_eq!(age(10_000, 10_000 - 7200), "2h");
        assert_eq!(age(1_000_000, 1_000_000 - 3 * 86400), "3d");
        assert_eq!(age(0, 100), "now", "clock skew never panics");
    }
}
