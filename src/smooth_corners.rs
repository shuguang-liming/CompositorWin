//! Rounded rectangles matching Compositor's 60% Smooth Corners option.
//!
//! Geometry ported from Compositor 1.5.3, `Document/ShapeTool.swift`:
//! Copyright (c) 2026 Wonder Assembly LLC, distributed under the MIT License.
//! The original copyright and permission notice are retained in the project notices.
//! The cubic–arc–cubic construction follows Figma's corner smoothing:
//! <https://www.figma.com/blog/desperately-seeking-squircles/>.

/// A rectangle in local coordinates, from `(0, 0)` to `(width, height)`.
///
/// Construct once per raster, then call [`Self::contains`] for each antialiasing
/// sample. Membership uses the original cubic curves and circular arc directly;
/// there is no polygon approximation, allocation, or size-dependent tessellation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RoundedRectangle {
    width: f64,
    height: f64,
    radius: f64,
    corner: Option<SmoothCorner>,
}

#[derive(Clone, Copy, Debug)]
struct SmoothCorner {
    // All corner distances are divided by radius to keep the calculation stable
    // for both small previews and large shapes.
    reach: f64,
    arc_start: f64,
    cubic_linear: f64,
    cubic_quadratic: f64,
    cubic_cubic: f64,
}

impl RoundedRectangle {
    pub(crate) fn new(width: f64, height: f64, radius: f64, smoothing: bool) -> Self {
        let valid = width.is_finite() && height.is_finite() && width > 0. && height > 0.;
        let radius = if valid && !radius.is_nan() {
            radius.clamp(0., width.min(height) / 2.)
        } else {
            0.
        };
        let smoothing = if smoothing && radius > 0. {
            (width.min(height) / (2. * radius) - 1.).clamp(0., 0.6)
        } else {
            0.
        };
        let corner = (smoothing > 0.).then(|| {
            let reach = 1. + smoothing;
            let angle = smoothing * std::f64::consts::FRAC_PI_4;
            let tangent = (angle / 2.).tan();
            let c = tangent * angle.cos();
            let d = tangent * angle.sin();
            let arc_length =
                std::f64::consts::SQRT_2 * ((1. - smoothing) * std::f64::consts::FRAC_PI_4).sin();
            let b = (reach - arc_length - c - d) / 3.;
            let a = 2. * b;
            SmoothCorner {
                reach,
                arc_start: d,
                // Cubic x controls, in inward distances from the corner, are
                // [reach, reach-a, reach-a-b, reach-a-b-c]. The y controls are
                // [0, 0, 0, d], so y=d*t^3 gives an exact, monotone inverse.
                cubic_linear: -3. * a,
                cubic_quadratic: 3. * (a - b),
                cubic_cubic: -c,
            }
        });
        Self {
            width: if valid { width } else { 0. },
            height: if valid { height } else { 0. },
            radius,
            corner,
        }
    }

    /// Whether a finite sample lies inside the shape, including its boundary.
    pub(crate) fn contains(&self, x: f64, y: f64) -> bool {
        if self.width <= 0.
            || self.height <= 0.
            || !x.is_finite()
            || !y.is_finite()
            || x < 0.
            || y < 0.
            || x > self.width
            || y > self.height
        {
            return false;
        }
        if self.radius == 0. {
            return true;
        }
        let Some(corner) = self.corner else {
            // Keep the existing circular-corner raster's calculation, including
            // its sample coordinates, when smoothing is off or there is no room.
            let cx = x.clamp(self.radius, self.width - self.radius);
            let cy = y.clamp(self.radius, self.height - self.radius);
            return (x - cx).hypot(y - cy) <= self.radius;
        };

        // Reflect all four corners, then the diagonal. Only the first cubic and
        // half the arc remain: the second cubic is its reflection in x=y.
        let u = x.min(self.width - x);
        let v = y.min(self.height - y);
        let far = u.max(v);
        if far >= self.radius * corner.reach {
            return true;
        }
        let near = u.min(v) / self.radius;
        if near >= 1. - std::f64::consts::FRAC_1_SQRT_2 {
            return true;
        }
        let boundary = if near < corner.arc_start {
            let t = (near / corner.arc_start).cbrt();
            corner.reach
                + t * (corner.cubic_linear + t * (corner.cubic_quadratic + t * corner.cubic_cubic))
        } else {
            // The original arc has center (radius, radius). This form avoids
            // subtracting nearly equal values inside the square root.
            1. - (near * (2. - near)).sqrt()
        };
        far / self.radius >= boundary
    }
}

impl RoundedRectangle {
    /// A convex outline of the same cubic/arc boundary used by contains().
    /// Tolerance is in local units; callers choose a physical-screen-pixel error.
    /// Subdivision is bounded, and never allocates a width-by-height surface.
    pub(crate) fn outline(&self, tolerance: f64) -> Vec<[f64; 2]> {
        if self.width <= 0. || self.height <= 0. {
            return Vec::new();
        }
        if self.radius == 0. {
            return vec![
                [0., 0.],
                [self.width, 0.],
                [self.width, self.height],
                [0., self.height],
            ];
        }
        let tolerance = tolerance.max(1e-6);
        let mut corner_points = Vec::new();
        if let Some(corner) = self.corner {
            // Recover the original Bezier controls from the exact polynomial
            // coefficients used for sample membership, not a second squircle.
            let x0 = corner.reach;
            let x1 = x0 + corner.cubic_linear / 3.;
            let x2 = corner.cubic_quadratic / 3. + 2. * x1 - x0;
            let x3 = corner.cubic_cubic + 3. * x2 - 3. * x1 + x0;
            let controls = [[x0, 0.], [x1, 0.], [x2, 0.], [x3, corner.arc_start]]
                .map(|point| point.map(|value| value * self.radius));
            let mut lead = vec![controls[0]];
            flatten_cubic(controls, tolerance, 0, &mut lead);
            corner_points.extend_from_slice(&lead);
            let angle = (1. - corner.arc_start).clamp(-1., 1.).acos();
            let mut arc = Vec::new();
            append_arc(
                &mut arc,
                [self.radius; 2],
                self.radius,
                -std::f64::consts::FRAC_PI_2 - angle,
                -std::f64::consts::PI + angle,
                tolerance,
            );
            corner_points.extend(arc.into_iter().skip(1));
            corner_points.extend(
                lead.into_iter()
                    .rev()
                    .skip(1)
                    .map(|point| [point[1], point[0]]),
            );
        } else {
            append_arc(
                &mut corner_points,
                [self.radius; 2],
                self.radius,
                -std::f64::consts::FRAC_PI_2,
                -std::f64::consts::PI,
                tolerance,
            );
        }
        let mut points = Vec::with_capacity(corner_points.len() * 4);
        for index in 0..4 {
            points.extend(corner_points.iter().map(|&[x, y]| match index {
                0 => [self.width - x, y],
                1 => [self.width - y, self.height - x],
                2 => [x, self.height - y],
                _ => [y, x],
            }));
        }
        points
    }
}

/// Sample an exact circular arc to the requested chord error, with an explicit
/// 4096-segment ceiling for extreme inputs.
pub(crate) fn append_arc(
    points: &mut Vec<[f64; 2]>,
    center: [f64; 2],
    radius: f64,
    start: f64,
    end: f64,
    tolerance: f64,
) {
    let step = 2. * (1. - (tolerance.max(1e-6) / radius.max(1e-6)).min(1.)).acos();
    let segments = ((end - start).abs() / step.max(1e-6))
        .ceil()
        .clamp(1., 4096.) as usize;
    for index in 0..=segments {
        let angle = start + (end - start) * index as f64 / segments as f64;
        points.push([
            center[0] + radius * angle.cos(),
            center[1] + radius * angle.sin(),
        ]);
    }
}

fn flatten_cubic(controls: [[f64; 2]; 4], tolerance: f64, depth: u32, points: &mut Vec<[f64; 2]>) {
    let [a, b, c, d] = controls;
    let dx = d[0] - a[0];
    let dy = d[1] - a[1];
    let length_squared = dx * dx + dy * dy;
    let distance_squared = |point: [f64; 2]| {
        if length_squared == 0. {
            (point[0] - a[0]).powi(2) + (point[1] - a[1]).powi(2)
        } else {
            ((point[0] - a[0]) * dy - (point[1] - a[1]) * dx).powi(2) / length_squared
        }
    };
    if depth == 12 || distance_squared(b).max(distance_squared(c)) <= tolerance * tolerance {
        points.push(d);
        return;
    }
    let midpoint = |p: [f64; 2], q: [f64; 2]| [(p[0] + q[0]) / 2., (p[1] + q[1]) / 2.];
    let ab = midpoint(a, b);
    let bc = midpoint(b, c);
    let cd = midpoint(c, d);
    let abc = midpoint(ab, bc);
    let bcd = midpoint(bc, cd);
    let middle = midpoint(abc, bcd);
    flatten_cubic([a, ab, abc, middle], tolerance, depth + 1, points);
    flatten_cubic([middle, bcd, cd, d], tolerance, depth + 1, points);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circular(width: f64, height: f64, radius: f64, x: f64, y: f64) -> bool {
        let radius = radius.clamp(0., width.min(height) / 2.);
        let cx = x.clamp(radius, width - radius);
        let cy = y.clamp(radius, height - radius);
        (x - cx).hypot(y - cy) <= radius
    }

    #[test]
    fn preview_outline_uses_the_same_exact_boundary_and_has_bounded_storage() {
        for smoothing in [false, true] {
            let shape = RoundedRectangle::new(100., 80., 20., smoothing);
            let points = shape.outline(0.01);
            assert!(points.len() > 16 && points.len() < 2048);
            for [x, y] in points {
                let dx = 50. - x;
                let dy = 40. - y;
                let distance = dx.hypot(dy);
                let [dx, dy] = [dx / distance * 1e-6, dy / distance * 1e-6];
                assert!(shape.contains(x + dx, y + dy));
                assert!(!shape.contains(x - dx, y - dy));
            }
        }
        assert_eq!(
            RoundedRectangle::new(30., 20., 0., true)
                .outline(0.01)
                .len(),
            4
        );
        let huge = RoundedRectangle::new(30_000., 30_000., 5000., true).outline(0.25 / 64.);
        assert!(
            huge.len() < 40_000,
            "Draft storage follows the contour, not pixel area"
        );
        let crowded = RoundedRectangle::new(20., 40., 10., true);
        assert_eq!(
            crowded.outline(0.01),
            RoundedRectangle::new(20., 40., 10., false).outline(0.01)
        );
    }

    // Evaluate the Swift path's control points with de Casteljau's algorithm,
    // independently of contains()'s cubic coefficients and inverse.
    fn cubic_point(smoothing: f64, t: f64) -> [f64; 2] {
        let reach = 1. + smoothing;
        let angle = smoothing * std::f64::consts::FRAC_PI_4;
        let tangent = (angle / 2.).tan();
        let c = tangent * angle.cos();
        let d = tangent * angle.sin();
        let arc = std::f64::consts::SQRT_2 * ((1. - smoothing) * std::f64::consts::FRAC_PI_4).sin();
        let b = (reach - arc - c - d) / 3.;
        let a = 2. * b;
        let mut points = [
            [reach, 0.],
            [reach - a, 0.],
            [reach - a - b, 0.],
            [reach - a - b - c, d],
        ];
        for count in (1..4).rev() {
            for i in 0..count {
                points[i] = [
                    points[i][0] * (1. - t) + points[i + 1][0] * t,
                    points[i][1] * (1. - t) + points[i + 1][1] * t,
                ];
            }
        }
        points[0]
    }

    #[test]
    fn zero_radius_is_a_rectangle_and_rejects_outside_samples() {
        for smoothing in [false, true] {
            let shape = RoundedRectangle::new(4., 3., 0., smoothing);
            for point in [[0., 0.], [4., 3.], [4., 0.], [2., 1.5]] {
                assert!(shape.contains(point[0], point[1]));
            }
            for point in [[-0.001, 1.], [2., 3.001], [4.001, 1.], [1., -0.001]] {
                assert!(!shape.contains(point[0], point[1]));
            }
            assert!(!shape.contains(f64::NAN, 1.));
            assert!(!shape.contains(1., f64::INFINITY));
        }
    }

    #[test]
    fn invalid_or_empty_boxes_never_contain_samples() {
        for (width, height) in [
            (0., 10.),
            (10., 0.),
            (-1., 10.),
            (f64::NAN, 10.),
            (10., f64::INFINITY),
        ] {
            let shape = RoundedRectangle::new(width, height, 2., true);
            assert!(!shape.contains(0., 0.));
            assert!(!shape.contains(1., 1.));
        }
        assert!(RoundedRectangle::new(3., 3., f64::NAN, true).contains(0., 0.));
        assert!(RoundedRectangle::new(3., 3., -1., true).contains(0., 0.));
    }

    #[test]
    fn smoothing_off_preserves_all_circular_antialias_samples() {
        for width in [1., 2., 5., 20., 31.] {
            for height in [1., 3., 8., 23.] {
                for radius in [0., 0.125, 1., 3., 20., f64::INFINITY] {
                    let shape = RoundedRectangle::new(width, height, radius, false);
                    for y in 0..(height as usize * 4) {
                        for x in 0..(width as usize * 4) {
                            let x = (x as f64 + 0.5) / 4.;
                            let y = (y as f64 + 0.5) / 4.;
                            assert_eq!(shape.contains(x, y), circular(width, height, radius, x, y));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn half_shorter_side_falls_back_to_the_same_round_corner() {
        for (width, height) in [(1_f64, 1.), (1., 5.), (2., 1.), (20., 40.), (23., 10.)] {
            for radius in [width.min(height) / 2., 1000., f64::INFINITY] {
                let shape = RoundedRectangle::new(width, height, radius, true);
                assert!(shape.corner.is_none());
                for y in 0..(height as usize * 4) {
                    for x in 0..(width as usize * 4) {
                        let x = (x as f64 + 0.5) / 4.;
                        let y = (y as f64 + 0.5) / 4.;
                        assert_eq!(shape.contains(x, y), circular(width, height, radius, x, y));
                    }
                }
            }
        }
    }

    #[test]
    fn crowded_corners_reduce_smoothing_without_shrinking_radius() {
        let full = RoundedRectangle::new(40., 60., 10., true);
        assert!((full.corner.unwrap().reach - 1.6).abs() < 1e-14);
        let reduced = RoundedRectangle::new(24., 60., 10., true);
        let rotated = RoundedRectangle::new(60., 24., 10., true);
        assert_eq!(reduced.radius, 10.);
        assert_eq!(rotated.radius, 10.);
        assert!((reduced.corner.unwrap().reach - 1.2).abs() < 1e-14);
        assert_eq!(reduced.corner.unwrap().reach, rotated.corner.unwrap().reach);
        // Each corner's reach is exactly half the short side; it cannot overlap
        // its neighboring corner even though the requested radius remains ten.
        assert!((reduced.radius * reduced.corner.unwrap().reach - 12.).abs() < 1e-13);
        for t in [0.1, 0.25, 0.5, 0.75, 0.95] {
            let [x, y] = cubic_point(0.2, t).map(|v| v * 10.);
            assert!(!reduced.contains(x - 1e-7, y));
            assert!(reduced.contains(x + 1e-7, y));
        }
    }

    #[test]
    fn every_corner_and_the_diagonal_have_identical_geometry() {
        let shape = RoundedRectangle::new(45., 21., 5., true);
        let transposed = RoundedRectangle::new(21., 45., 5., true);
        for y in 0..168 {
            for x in 0..360 {
                let x = (x as f64 + 0.5) / 8.;
                let y = (y as f64 + 0.5) / 8.;
                let inside = shape.contains(x, y);
                assert_eq!(inside, shape.contains(45. - x, y));
                assert_eq!(inside, shape.contains(x, 21. - y));
                assert_eq!(inside, shape.contains(45. - x, 21. - y));
                assert_eq!(inside, transposed.contains(y, x));
            }
        }
    }

    #[test]
    fn membership_matches_both_original_cubics_and_the_circular_arc() {
        let shape = RoundedRectangle::new(100., 80., 20., true);
        for t in [0.01, 0.1, 0.25, 0.5, 0.75, 0.95, 0.999] {
            let [x, y] = cubic_point(0.6, t).map(|v| v * 20.);
            for [x, y] in [[x, y], [y, x]] {
                assert!(
                    !shape.contains(x - 1e-7, y - 1e-7),
                    "outside cubic at t={t}"
                );
                assert!(shape.contains(x + 1e-7, y + 1e-7), "inside cubic at t={t}");
            }
        }
        for angle in [0.6 * std::f64::consts::FRAC_PI_4, 0.5, 0.6, 0.7] {
            let x = 20. * (1. - angle.sin());
            let y = 20. * (1. - angle.cos());
            for [x, y] in [[x, y], [y, x]] {
                assert!(!shape.contains(x - 1e-7, y - 1e-7));
                assert!(shape.contains(x + 1e-7, y + 1e-7));
            }
        }
    }

    #[test]
    fn smooth_corners_have_a_distinct_shape_and_keep_the_arc_radius() {
        let smooth = RoundedRectangle::new(100., 80., 20., true);
        let round = RoundedRectangle::new(100., 80., 20., false);
        assert!(round.contains(22., 0.02));
        assert!(!smooth.contains(22., 0.02));
        assert!(smooth.contains(32., 0.));
        assert!(!smooth.contains(31.99, 0.));
        for angle in [0.5_f64, 0.6, 0.7] {
            let x = 20. * (1. - angle.sin());
            let y = 20. * (1. - angle.cos());
            for offset in [-1e-7, 1e-7] {
                assert_eq!(
                    smooth.contains(x + offset, y),
                    round.contains(x + offset, y)
                );
            }
        }
    }

    #[test]
    fn geometry_remains_continuous_at_cubic_arc_join_and_at_diagonal() {
        let shape = RoundedRectangle::new(100., 80., 20., true);
        let [join_x, join_y] = cubic_point(0.6, 1.).map(|v| v * 20.);
        for offset in [-1e-9, 0., 1e-9] {
            assert!(!shape.contains(join_x - 1e-6, join_y + offset));
            assert!(shape.contains(join_x + 1e-6, join_y + offset));
        }
        let diagonal = 20. * (1. - std::f64::consts::FRAC_1_SQRT_2);
        assert!(!shape.contains(diagonal - 1e-7, diagonal - 1e-7));
        assert!(shape.contains(diagonal + 1e-7, diagonal + 1e-7));
    }

    #[test]
    fn subpixel_and_large_shapes_match_the_same_scaled_path() {
        let base = RoundedRectangle::new(45., 21., 5., true);
        for scale in [0.001, 0.25, 1., 64., 1_000_000.] {
            let scaled = RoundedRectangle::new(45. * scale, 21. * scale, 5. * scale, true);
            for y in 0..42 {
                for x in 0..90 {
                    let x = x as f64 / 2. + 0.13;
                    let y = y as f64 / 2. + 0.17;
                    assert_eq!(base.contains(x, y), scaled.contains(x * scale, y * scale));
                }
            }
        }
        let tiny = RoundedRectangle::new(1., 1., 0.2, true);
        let mut partial_pixels = 0;
        for y in 0..4 {
            for x in 0..4 {
                let mut samples = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        if tiny.contains(
                            (x as f64 + (sx as f64 + 0.5) / 4.) / 4.,
                            (y as f64 + (sy as f64 + 0.5) / 4.) / 4.,
                        ) {
                            samples += 1;
                        }
                    }
                }
                partial_pixels += usize::from(samples > 0 && samples < 16);
            }
        }
        assert!(partial_pixels >= 4);
    }
}
