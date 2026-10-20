//! Marching squares: isolines over a scalar grid.
//!
//! The map's 0 °C level overlay is a height field on the radar's polar grid
//! (the melting-layer product); drawing *isohypses* — lines of constant
//! height, like isobars — shows where the snow is melting at 6 kft versus
//! 10 kft far better than a color wash that hides the echoes underneath.
//!
//! Segments are returned independently (no polyline stitching): the caller
//! projects their endpoints and draws them, so disjoint contours, holes and
//! NaN gaps all fall out naturally.

/// One isoline segment in grid coordinates: fractional cell indices, so a
/// point at `(1.5, 2.0)` sits halfway between grid rows 1 and 2.
pub type Segment = [(f32, f32); 2];

/// Extract every segment of the `level` isocontour from a `width × height`
/// row-major grid.
///
/// Cells touching a non-finite value are skipped — a missing measurement is
/// not a zero, and drawing a contour through it would invent data.
pub fn contour_segments(width: usize, height: usize, values: &[f32], level: f32) -> Vec<Segment> {
    let mut out = Vec::new();
    if width < 2 || height < 2 || values.len() < width * height {
        return out;
    }
    let at = |x: usize, y: usize| values[y * width + x];
    for y in 0..height - 1 {
        for x in 0..width - 1 {
            let c = [at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y + 1)];
            if c.iter().any(|v| !v.is_finite()) {
                continue;
            }
            // Which edges the contour crosses. Edge k runs from corner k to
            // corner (k+1)%4; the crossing point is linearly interpolated.
            let inside: [bool; 4] = std::array::from_fn(|k| c[k] >= level);
            let n = inside.iter().filter(|v| **v).count();
            if n == 0 || n == 4 {
                continue;
            }
            let mut hits: Vec<(f32, f32)> = Vec::with_capacity(4);
            for k in 0..4 {
                let k2 = (k + 1) % 4;
                if inside[k] == inside[k2] {
                    continue;
                }
                let t = (level - c[k]) / (c[k2] - c[k]);
                let t = t.clamp(0.0, 1.0);
                let p = match k {
                    0 => (x as f32 + t, y as f32),
                    1 => (x as f32 + 1.0, y as f32 + t),
                    2 => (x as f32 + 1.0 - t, y as f32 + 1.0),
                    _ => (x as f32, y as f32 + 1.0 - t),
                };
                hits.push(p);
            }
            // One or two crossings: pair them up. (Four crossings in a cell
            // is the ambiguous saddle; pairing in order is a fine choice for
            // smooth height fields.)
            if hits.len() == 2 {
                out.push([hits[0], hits[1]]);
            } else if hits.len() == 4 {
                out.push([hits[0], hits[1]]);
                out.push([hits[2], hits[3]]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A linear ramp crosses a level in one straight vertical line.
    #[test]
    fn ramp_makes_a_straight_contour() {
        let (w, h) = (6usize, 5usize);
        let values: Vec<f32> = (0..h).flat_map(|_| (0..w).map(|x| x as f32)).collect();
        let segs = contour_segments(w, h, &values, 2.5);
        assert_eq!(segs.len(), (h - 1) * 1, "one segment per cell row");
        for [a, b] in segs {
            assert!((a.0 - 2.5).abs() < 1.0e-4 && (b.0 - 2.5).abs() < 1.0e-4);
            assert!((a.1 - b.1).abs() - 1.0 < 1.0e-4);
        }
    }

    /// A hill above the level produces a closed ring; the segments all sit
    /// near the expected radius.
    #[test]
    fn hill_makes_a_closed_ring() {
        let (w, h) = (21usize, 21usize);
        let values: Vec<f32> = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let (dx, dy) = (x as f32 - 10.0, y as f32 - 10.0);
                    10.0 - (dx * dx + dy * dy).sqrt()
                })
            })
            .collect();
        // Level 5 → radius 5 cells.
        let segs = contour_segments(w, h, &values, 5.0);
        assert!(
            segs.len() >= 16,
            "a ring needs segments, got {}",
            segs.len()
        );
        for [a, _] in segs {
            let r = ((a.0 - 10.0).powi(2) + (a.1 - 10.0).powi(2)).sqrt();
            assert!((r - 5.0).abs() < 0.6, "off-ring point {a:?}");
        }
    }

    /// Missing data is a gap, not a zero crossing; degenerate grids are empty.
    #[test]
    fn gaps_and_degenerate_grids_are_skipped() {
        let (w, h) = (5usize, 3usize);
        let mut values: Vec<f32> = (0..w * h).map(|i| (i % w) as f32).collect();
        // Punch a hole through every cell that would cross level 2.5.
        values[0 * w + 2] = f32::NAN;
        values[1 * w + 2] = f32::NAN;
        values[2 * w + 2] = f32::NAN;
        assert!(contour_segments(w, h, &values, 2.5).is_empty());
        assert!(contour_segments(1, 5, &[1.0; 5], 0.5).is_empty());
        assert!(contour_segments(5, 5, &[1.0; 10], 0.5).is_empty());
    }
}
