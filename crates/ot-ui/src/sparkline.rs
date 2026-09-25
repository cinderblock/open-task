//! Compact time-series chart.
//!
//! Newest sample sits at the right edge; the x extent represents the series'
//! capacity, so a half-full series draws in the right half and grows leftward until
//! it fills the window and starts scrolling, the way Task Manager's graphs behave.
//!
//! The axis is linear for now. A log-scale time axis is planned (see the main plan):
//! the point placement below is the only thing that changes.

use ot_core::Series;
use ot_paint::{Color, DisplayList, Point, Rect};

#[derive(Debug, Clone, Copy)]
pub struct SparkStyle {
    pub line: Color,
    pub fill: Color,
    pub grid: Color,
    pub width: f32,
}

/// Draw `series` into `rect`, scaling values against `max`.
///
/// `scratch` is caller-owned so steady-state painting does not allocate. Points are
/// decimated to at most two per DIP, keeping the maximum in each bucket so spikes
/// survive, which is what matters on a monitoring graph.
pub fn paint(
    dl: &mut DisplayList,
    rect: Rect,
    series: &Series,
    max: f32,
    style: &SparkStyle,
    scratch: &mut Vec<Point>,
) {
    if rect.is_empty() {
        return;
    }

    // Reference lines at quarters.
    for q in 1..4 {
        let y = rect.bottom() - rect.h * (q as f32 / 4.0);
        dl.line(
            Point::new(rect.x, y),
            Point::new(rect.right(), y),
            style.grid,
            1.0,
        );
    }

    let n = series.len();
    if n < 2 || max <= 0.0 {
        return;
    }

    let cap = series.capacity().max(2);
    let dip_per_sample = rect.w / (cap - 1) as f32;
    let per_bucket = ((0.5 / dip_per_sample).ceil()).max(1.0) as usize;

    scratch.clear();
    let mut bucket_max = f32::MIN;
    let mut in_bucket = 0usize;
    for (i, s) in series.iter().enumerate() {
        bucket_max = bucket_max.max(s.value);
        in_bucket += 1;
        let last = i + 1 == n;
        if in_bucket == per_bucket || last {
            let x = rect.right() - (n - 1 - i) as f32 * dip_per_sample;
            let y = rect.bottom() - (bucket_max / max).clamp(0.0, 1.0) * rect.h;
            scratch.push(Point::new(x, y));
            bucket_max = f32::MIN;
            in_bucket = 0;
        }
    }
    if scratch.len() < 2 {
        return;
    }

    let first_x = scratch[0].x;
    let bottom = rect.bottom();
    dl.fill_polygon(
        scratch.iter().copied().chain([
            Point::new(rect.right(), bottom),
            Point::new(first_x, bottom),
        ]),
        style.fill,
    );
    dl.polyline(scratch.iter().copied(), style.line, style.width);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_paint::DrawCmd;

    fn style() -> SparkStyle {
        SparkStyle {
            line: Color::WHITE,
            fill: Color::WHITE.with_alpha(0.2),
            grid: Color::WHITE.with_alpha(0.1),
            width: 1.0,
        }
    }

    fn polyline_points(dl: &DisplayList) -> Vec<Point> {
        dl.cmds()
            .iter()
            .find_map(|c| match *c {
                DrawCmd::Polyline { points, .. } => Some(dl.points(points).to_vec()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn newest_sample_lands_on_right_edge() {
        let mut s = Series::new(100);
        for i in 0..10 {
            s.push(i, 50.0);
        }
        let mut dl = DisplayList::new();
        let rect = Rect::new(0.0, 0.0, 200.0, 50.0);
        paint(&mut dl, rect, &s, 100.0, &style(), &mut Vec::new());
        let pts = polyline_points(&dl);
        assert_eq!(pts.len(), 10);
        assert!((pts.last().unwrap().x - 200.0).abs() < 1e-3);
        assert!((pts[0].y - 25.0).abs() < 1e-3, "50% of 50 DIP tall is y=25");
    }

    #[test]
    fn decimation_keeps_peaks() {
        // 1000 samples into 100 DIPs => 5 samples per bucket; one spike must survive.
        let mut s = Series::new(1000);
        for i in 0..1000 {
            s.push(i, if i == 500 { 100.0 } else { 0.0 });
        }
        let mut dl = DisplayList::new();
        paint(
            &mut dl,
            Rect::new(0.0, 0.0, 100.0, 10.0),
            &s,
            100.0,
            &style(),
            &mut Vec::new(),
        );
        let pts = polyline_points(&dl);
        assert!(pts.len() <= 201);
        assert!(
            pts.iter().any(|p| p.y.abs() < 1e-3),
            "spike reached the top"
        );
    }

    #[test]
    fn too_few_samples_draws_only_grid() {
        let mut s = Series::new(10);
        s.push(0, 1.0);
        let mut dl = DisplayList::new();
        paint(
            &mut dl,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            &s,
            1.0,
            &style(),
            &mut Vec::new(),
        );
        assert!(dl.cmds().iter().all(|c| matches!(c, DrawCmd::Line { .. })));
    }
}
