//! Pure chart geometry: data -> rects and labels.
//!
//! Tower's Activity view draws bar charts with filled rectangles and text only.
//! Nothing here knows about colours or fonts: series are referred to by index
//! and `layout.rs` maps an index to a `Color`, then paints what this returns.

use super::clip::Rect;

/// One bar column (e.g. one day): stacked segment values, bottom first, one per series.
pub struct Column {
    pub label: String,
    pub values: Vec<f64>,
}

pub struct BarChartSpec<'a> {
    pub columns: &'a [Column],
    pub series: usize,
    /// Gap between bars as a fraction of the slot width.
    pub gap: f32,
    /// Show an x label every n columns; 0 = auto so labels don't overlap given `label_w`.
    pub label_every: usize,
    pub label_w: f32,
    /// Pixels reserved on the left for y tick labels.
    pub axis_w: f32,
    /// Pixels reserved at the bottom for x labels.
    pub label_h: f32,
}

pub struct Segment {
    pub column: usize,
    pub series: usize,
    pub rect: Rect,
    pub value: f64,
}

pub struct Tick {
    pub y: f32,
    pub value: f64,
    pub label: String,
}

pub struct XLabel {
    pub x_center: f32,
    pub y: f32,
    pub text: String,
}

pub struct BarChart {
    pub plot: Rect,
    pub segments: Vec<Segment>,
    pub ticks: Vec<Tick>,
    pub labels: Vec<XLabel>,
    /// Axis top (the "nice" max), in data units.
    pub max: f64,
    /// Per column, the full column rect (for hit-testing and hover).
    pub slots: Vec<Rect>,
}

/// Negative, NaN and infinite values count as 0.
fn clean(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

/// "Nice" axis top and step: `0..=top` split in 2-5 steps of 1/2/2.5/5 x 10^k.
///
/// Picks the smallest top that covers `max`; on a tie the larger step (fewer
/// gridlines) wins. `max <= 0` (or NaN) gives `(1.0, 0.5)`, an empty chart
/// with a 0 / 0.5 / 1 axis.
pub fn nice_scale(max: f64) -> (f64, f64) {
    if !(max.is_finite() && max > 0.0) {
        return (1.0, 0.5);
    }
    let exp = max.log10().floor() as i32;
    let mut best: Option<(f64, f64)> = None;
    for k in (exp - 1)..=(exp + 1) {
        let base = 10f64.powi(k);
        for m in [1.0, 2.0, 2.5, 5.0].iter() {
            let step = m * base;
            let n = (max / step - 1e-9).ceil().max(1.0);
            if n < 2.0 || n > 5.0 {
                continue;
            }
            let top = n * step;
            let better = match best {
                None => true,
                Some((bt, bs)) => {
                    top < bt - bt * 1e-9 || ((top - bt).abs() <= bt * 1e-9 && step > bs)
                }
            };
            if better {
                best = Some((top, step));
            }
        }
    }
    best.unwrap_or((max, max / 2.0))
}

/// Trim trailing zeros of a fixed-point rendering ("1.50" -> "1.5", "2.0" -> "2").
fn trim(s: String) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Scale `v` (>= 0) into a mantissa and suffix: 12 -> (12, ""), 1200 -> (1.2, "k").
fn scale_unit(v: f64) -> (f64, &'static str) {
    const UNITS: [&str; 4] = ["", "k", "M", "G"];
    let mut x = v;
    let mut i = 0;
    // Round at one decimal first so 999_960 reads "1M", not "1000k".
    while i + 1 < UNITS.len() && (x * 10.0).round() / 10.0 >= 1000.0 {
        x /= 1000.0;
        i += 1;
    }
    (x, UNITS[i])
}

/// Compact number for tick labels: 0, 5, 12, 1.2k, 3.4M.
pub fn compact(v: f64) -> String {
    let neg = v.is_finite() && v < 0.0;
    let v = clean(v.abs());
    let (x, unit) = scale_unit(v);
    let body = if unit.is_empty() {
        if x.fract() == 0.0 || x >= 100.0 {
            format!("{:.0}", x)
        } else if x >= 10.0 {
            trim(format!("{:.1}", x))
        } else {
            trim(format!("{:.2}", x))
        }
    } else {
        trim(format!("{:.1}", x))
    };
    if neg && body != "0" {
        format!("-{}{}", body, unit)
    } else {
        format!("{}{}", body, unit)
    }
}

/// Money variant of `compact`: "$0.42", "$12", "$1.2k".
pub fn compact_money(v: f64) -> String {
    let v = clean(v);
    let (x, unit) = scale_unit(v);
    if !unit.is_empty() {
        return format!("${}{}", trim(format!("{:.1}", x)), unit);
    }
    if x.fract() == 0.0 || x >= 10.0 {
        format!("${:.0}", x)
    } else {
        format!("${:.2}", x)
    }
}

/// Lay out a stacked bar chart inside `area`.
pub fn bar_chart(area: Rect, spec: &BarChartSpec) -> BarChart {
    let axis_w = spec.axis_w.max(0.0).min(area.w.max(0.0));
    let label_h = spec.label_h.max(0.0).min(area.h.max(0.0));
    let plot = Rect::new(
        area.x + axis_w,
        area.y,
        (area.w - axis_w).max(0.0),
        (area.h - label_h).max(0.0),
    );

    let series = spec.series;
    let totals: Vec<f64> = spec
        .columns
        .iter()
        .map(|c| c.values.iter().take(series).map(|v| clean(*v)).sum())
        .collect();
    let max_total = totals.iter().cloned().fold(0.0, f64::max);
    let (top, step) = nice_scale(max_total);

    let mut out = BarChart {
        plot,
        segments: Vec::new(),
        ticks: Vec::new(),
        labels: Vec::new(),
        max: top,
        slots: Vec::new(),
    };

    let steps = (top / step).round() as usize;
    for i in 0..=steps {
        let value = (i as f64 * step).min(top);
        let y = plot.bottom() - (value / top) as f32 * plot.h;
        out.ticks.push(Tick {
            y,
            value,
            label: compact(value),
        });
    }

    let n = spec.columns.len();
    if n == 0 {
        return out;
    }
    let slot_w = plot.w / n as f32;
    let gap = spec.gap.max(0.0).min(1.0);
    let bar_w = (slot_w * (1.0 - gap)).max(1.0).min(slot_w);
    let every = if spec.label_every > 0 {
        spec.label_every
    } else if slot_w > 0.0 {
        ((spec.label_w / slot_w).ceil() as usize).max(1)
    } else {
        n
    };

    for (ci, col) in spec.columns.iter().enumerate() {
        let slot = Rect::new(plot.x + ci as f32 * slot_w, plot.y, slot_w, plot.h);
        out.slots.push(slot);
        let x = slot.x + (slot_w - bar_w) / 2.0;
        let mut cursor = plot.bottom();
        for (si, raw) in col.values.iter().take(series).enumerate() {
            let value = clean(*raw);
            if value <= 0.0 {
                continue;
            }
            let h = ((value / top) as f32 * plot.h).max(1.0);
            let y = (cursor - h).max(plot.y);
            let h = cursor - y;
            if h <= 0.0 {
                continue;
            }
            out.segments.push(Segment {
                column: ci,
                series: si,
                rect: Rect::new(x, y, bar_w, h),
                value,
            });
            cursor = y;
        }
        if label_h > 0.0 && ci % every == 0 {
            out.labels.push(XLabel {
                x_center: slot.x + slot_w / 2.0,
                y: plot.bottom(),
                text: col.label.clone(),
            });
        }
    }
    out
}

pub struct Part {
    pub series: usize,
    pub rect: Rect,
    pub value: f64,
    pub share: f64,
}

/// One horizontal 100% bar split into parts (outcomes).
///
/// Zero total -> empty. Zero parts are skipped. Widths are proportional; each
/// non-zero part gets at least `min_w` px (taken from the largest part), and
/// the last part absorbs rounding so the parts exactly fill `area.w`.
pub fn share_bar(area: Rect, values: &[f64], min_w: f32) -> Vec<Part> {
    let vals: Vec<f64> = values.iter().map(|v| clean(*v)).collect();
    let total: f64 = vals.iter().sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let idx: Vec<usize> = (0..vals.len()).filter(|i| vals[*i] > 0.0).collect();
    let mut widths: Vec<f32> = idx
        .iter()
        .map(|i| (vals[*i] / total) as f32 * area.w)
        .collect();

    let min_w = min_w.max(0.0).min(area.w / idx.len() as f32);
    for p in 0..widths.len() {
        if widths[p] >= min_w {
            continue;
        }
        let need = min_w - widths[p];
        let mut big = 0;
        for q in 0..widths.len() {
            if widths[q] > widths[big] {
                big = q;
            }
        }
        if big != p {
            widths[big] -= need;
            widths[p] = min_w;
        }
    }

    let mut parts = Vec::with_capacity(idx.len());
    let mut x = area.x;
    let last = idx.len() - 1;
    for (k, i) in idx.iter().enumerate() {
        let w = if k == last {
            (area.right() - x).max(0.0)
        } else {
            widths[k]
        };
        parts.push(Part {
            series: *i,
            rect: Rect::new(x, area.y, w, area.h),
            value: vals[*i],
            share: vals[*i] / total,
        });
        x += w;
    }
    parts
}

/// Tiny sparkline as bars (for KPI tiles): one thin bar per value scaled to
/// the max, bottom-aligned, `gap` px between. Always one rect per value; a
/// zero value has zero height, a non-zero one at least 1px.
pub fn spark_bars(area: Rect, values: &[f64], gap: f32) -> Vec<Rect> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let vals: Vec<f64> = values.iter().map(|v| clean(*v)).collect();
    let max = vals.iter().cloned().fold(0.0, f64::max);
    let gap = gap.max(0.0);
    let bar_w = ((area.w - gap * (n - 1) as f32) / n as f32).max(0.0);
    vals.iter()
        .enumerate()
        .map(|(i, v)| {
            let h = if *v > 0.0 && max > 0.0 {
                ((*v / max) as f32 * area.h).max(1.0).min(area.h)
            } else {
                0.0
            };
            Rect::new(
                area.x + i as f32 * (bar_w + gap),
                area.bottom() - h,
                bar_w,
                h,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn col(label: &str, values: &[f64]) -> Column {
        Column {
            label: label.to_string(),
            values: values.to_vec(),
        }
    }

    fn spec(columns: &[Column]) -> BarChartSpec<'_> {
        BarChartSpec {
            columns,
            series: 3,
            gap: 0.2,
            label_every: 1,
            label_w: 30.0,
            axis_w: 30.0,
            label_h: 16.0,
        }
    }

    fn inside(r: &Rect, a: &Rect) -> bool {
        let e = 1e-3;
        r.x >= a.x - e
            && r.y >= a.y - e
            && r.right() <= a.right() + e
            && r.bottom() <= a.bottom() + e
    }

    #[test]
    fn nice_scale_cases() {
        let cases = [
            (0.0, 1.0, 0.5),
            (0.3, 0.3, 0.1),
            (1.0, 1.0, 0.5),
            (7.0, 7.5, 2.5),
            (10.0, 10.0, 5.0),
            (13.0, 15.0, 5.0),
            (99.0, 100.0, 50.0),
            (1234.0, 1250.0, 250.0),
        ];
        for (max, top, step) in cases.iter() {
            let (t, s) = nice_scale(*max);
            assert!(close(t, *top) && close(s, *step), "{} -> {} {}", max, t, s);
        }
        assert_eq!(nice_scale(f64::NAN), (1.0, 0.5));
        assert_eq!(nice_scale(-4.0), (1.0, 0.5));
    }

    #[test]
    fn compact_numbers() {
        assert_eq!(compact(0.0), "0");
        assert_eq!(compact(5.0), "5");
        assert_eq!(compact(12.0), "12");
        assert_eq!(compact(0.25), "0.25");
        assert_eq!(compact(1200.0), "1.2k");
        assert_eq!(compact(1000.0), "1k");
        assert_eq!(compact(3_400_000.0), "3.4M");
        assert_eq!(compact(999_960.0), "1M");
        assert_eq!(compact(f64::NAN), "0");
    }

    #[test]
    fn compact_money_values() {
        assert_eq!(compact_money(0.0), "$0");
        assert_eq!(compact_money(0.42), "$0.42");
        assert_eq!(compact_money(12.0), "$12");
        assert_eq!(compact_money(1234.0), "$1.2k");
        assert_eq!(compact_money(-3.0), "$0");
    }

    #[test]
    fn bar_chart_empty() {
        let s = spec(&[]);
        let c = bar_chart(Rect::new(0.0, 0.0, 200.0, 100.0), &s);
        assert!(c.segments.is_empty() && c.slots.is_empty() && c.labels.is_empty());
        assert!(close(c.max, 1.0));
        assert_eq!(c.ticks.len(), 3);
        assert_eq!(c.ticks[0].label, "0");
        assert_eq!(c.plot, Rect::new(30.0, 0.0, 170.0, 84.0));
    }

    #[test]
    fn bar_chart_single_column_is_centered() {
        let cols = [col("a", &[10.0, 0.0, 0.0])];
        let c = bar_chart(Rect::new(0.0, 0.0, 200.0, 100.0), &spec(&cols));
        assert_eq!(c.segments.len(), 1);
        let r = c.segments[0].rect;
        let slot = c.slots[0];
        assert!(close(c.max, 10.0));
        assert!((r.h - c.plot.h).abs() < 1e-3);
        assert!(((r.x + r.w / 2.0) - (slot.x + slot.w / 2.0)).abs() < 1e-3);
        assert!((r.w - slot.w * 0.8).abs() < 1e-3);
        assert!((r.bottom() - c.plot.bottom()).abs() < 1e-3);
    }

    #[test]
    fn bar_chart_stacks_bottom_first_and_contiguous() {
        let cols = [col("a", &[2.0, 3.0, 5.0])];
        let c = bar_chart(Rect::new(0.0, 0.0, 100.0, 116.0), &spec(&cols));
        assert_eq!(c.segments.len(), 3);
        for (i, s) in c.segments.iter().enumerate() {
            assert_eq!(s.series, i);
        }
        assert!(c.segments[0].rect.y > c.segments[1].rect.y);
        for w in c.segments.windows(2) {
            assert!((w[0].rect.y - w[1].rect.bottom()).abs() < 1e-3);
        }
    }

    #[test]
    fn bar_chart_min_one_pixel_and_no_zero_segments() {
        let cols = [
            col("a", &[1_000_000.0, 0.0, 0.0]),
            col("b", &[1.0, 0.0, 0.0]),
        ];
        let c = bar_chart(Rect::new(0.0, 0.0, 100.0, 100.0), &spec(&cols));
        let tiny = c.segments.iter().find(|s| s.column == 1).unwrap();
        assert!(tiny.rect.h >= 1.0);
        assert_eq!(c.segments.len(), 2);
        assert_eq!(c.ticks.last().unwrap().label, "1M");
    }

    #[test]
    fn bar_chart_bad_values_count_as_zero() {
        let cols = [col("a", &[f64::NAN, -5.0, 4.0])];
        let c = bar_chart(Rect::new(0.0, 0.0, 100.0, 100.0), &spec(&cols));
        assert_eq!(c.segments.len(), 1);
        assert_eq!(c.segments[0].series, 2);
    }

    #[test]
    fn bar_chart_all_rects_inside_area() {
        let mut seed: u64 = 12345;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f64) / ((1u64 << 31) as f64)
        };
        for round in 0..40 {
            let n = round % 12;
            let cols: Vec<Column> = (0..n)
                .map(|i| {
                    let vals: Vec<f64> = (0..3)
                        .map(|_| match (next() * 5.0) as u32 {
                            0 => 0.0,
                            1 => next() * 0.01,
                            2 => next() * 100.0,
                            3 => next() * 1e6,
                            _ => -next(),
                        })
                        .collect();
                    Column {
                        label: format!("d{}", i),
                        values: vals,
                    }
                })
                .collect();
            let area = Rect::new(
                7.0 + round as f32,
                3.0,
                40.0 + next() as f32 * 400.0,
                20.0 + next() as f32 * 200.0,
            );
            let c = bar_chart(
                area,
                &BarChartSpec {
                    label_every: 0,
                    ..spec(&cols)
                },
            );
            assert!(inside(&c.plot, &area));
            for s in &c.segments {
                assert!(inside(&s.rect, &area), "round {} {:?}", round, s.rect);
                assert!(inside(&s.rect, &c.plot));
            }
            for s in &c.slots {
                assert!(inside(s, &area));
            }
            for t in &c.ticks {
                assert!(t.y >= c.plot.y - 1e-3 && t.y <= c.plot.bottom() + 1e-3);
            }
        }
    }

    #[test]
    fn bar_chart_label_thinning() {
        let cols: Vec<Column> = (0..10)
            .map(|i| col(&i.to_string(), &[1.0, 0.0, 0.0]))
            .collect();
        let area = Rect::new(0.0, 0.0, 130.0, 60.0);
        let dense = bar_chart(
            area,
            &BarChartSpec {
                label_every: 0,
                label_w: 5.0,
                ..spec(&cols)
            },
        );
        assert_eq!(dense.labels.len(), 10);
        // Slots are 10px wide, labels 35px: every 4th column.
        let wide = bar_chart(
            area,
            &BarChartSpec {
                label_every: 0,
                label_w: 35.0,
                ..spec(&cols)
            },
        );
        let texts: Vec<&str> = wide.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["0", "4", "8"]);
        assert!(wide
            .labels
            .iter()
            .all(|l| (l.y - wide.plot.bottom()).abs() < 1e-3));
    }

    #[test]
    fn share_bar_fills_width_exactly() {
        let a = Rect::new(10.0, 5.0, 301.0, 8.0);
        let parts = share_bar(a, &[1.0, 0.0, 2.0, 4.0], 0.0);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].series, 0);
        assert_eq!(parts[1].series, 2);
        let sum: f32 = parts.iter().map(|p| p.rect.w).sum();
        assert!((sum - 301.0).abs() < 1e-3);
        assert!((parts.last().unwrap().rect.right() - a.right()).abs() < 1e-3);
        assert!((parts[0].share - 1.0 / 7.0).abs() < 1e-9);
        for w in parts.windows(2) {
            assert!((w[0].rect.right() - w[1].rect.x).abs() < 1e-3);
        }
    }

    #[test]
    fn share_bar_min_width_comes_from_largest() {
        let a = Rect::new(0.0, 0.0, 200.0, 8.0);
        let parts = share_bar(a, &[1000.0, 1.0, 0.0], 6.0);
        assert_eq!(parts.len(), 2);
        assert!(parts[1].rect.w >= 6.0 - 1e-3);
        let sum: f32 = parts.iter().map(|p| p.rect.w).sum();
        assert!((sum - 200.0).abs() < 1e-3);
        assert!(parts[0].rect.w > 150.0);
    }

    #[test]
    fn share_bar_zero_total() {
        let a = Rect::new(0.0, 0.0, 100.0, 8.0);
        assert!(share_bar(a, &[], 2.0).is_empty());
        assert!(share_bar(a, &[0.0, 0.0], 2.0).is_empty());
        assert!(share_bar(a, &[f64::NAN, -1.0], 2.0).is_empty());
    }

    #[test]
    fn spark_bars_scale_and_align() {
        let a = Rect::new(0.0, 0.0, 22.0, 10.0);
        let bars = spark_bars(a, &[0.0, 5.0, 10.0], 2.0);
        assert_eq!(bars.len(), 3);
        assert!((bars[0].h).abs() < 1e-6);
        assert!((bars[1].h - 5.0).abs() < 1e-3);
        assert!((bars[2].h - 10.0).abs() < 1e-3);
        for b in &bars {
            assert!((b.bottom() - 10.0).abs() < 1e-3);
            assert!((b.w - 6.0).abs() < 1e-3);
            assert!(inside(b, &a));
        }
        assert!((bars[1].x - 8.0).abs() < 1e-3);
        assert!(spark_bars(a, &[], 2.0).is_empty());
        let tiny = spark_bars(a, &[0.001, 1000.0], 1.0);
        assert!(tiny[0].h >= 1.0);
    }
}
