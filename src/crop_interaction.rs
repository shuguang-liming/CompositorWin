//! Crop frames and snap geometry in document pixels, without GUI or pixel allocations.
use crate::model::{Document, MAX_SIDE, Transform};
use std::collections::HashSet;
use uuid::Uuid;

pub type Frame = [f64; 4];

/// Canvas handle order: top-left, top-right, bottom-right, bottom-left, top, right, bottom, left.
pub const HANDLES: [[f64; 2]; 8] = [
    [0., 0.],
    [1., 0.],
    [1., 1.],
    [0., 1.],
    [0.5, 0.],
    [1., 0.5],
    [0.5, 1.],
    [0., 0.5],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropMode {
    Create,
    Move,
    Resize(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropDrag {
    pub start: [f64; 2],
    pub original: Frame,
    pub mode: CropMode,
}

pub fn snapped(frame: Frame) -> Frame {
    let x = frame[0].min(frame[0] + frame[2]).round();
    let y = frame[1].min(frame[1] + frame[3]).round();
    [
        x,
        y,
        (frame[0].max(frame[0] + frame[2]).round() - x).max(1.),
        (frame[1].max(frame[1] + frame[3]).round() - y).max(1.),
    ]
}

pub fn valid(frame: Frame) -> bool {
    frame.iter().all(|value| value.is_finite())
        && (1. ..=MAX_SIDE as f64).contains(&frame[2])
        && (1. ..=MAX_SIDE as f64).contains(&frame[3])
        && frame[0].abs() <= 1_000_000.
        && frame[1].abs() <= 1_000_000.
}

impl CropDrag {
    pub fn updated(&self, point: [f64; 2], ratio: Option<f64>, symmetric: bool) -> Option<Frame> {
        let frame = snapped(self.draft(point, ratio, symmetric)?);
        valid(frame).then_some(frame)
    }
    fn draft(&self, point: [f64; 2], ratio: Option<f64>, symmetric: bool) -> Option<Frame> {
        if !self.start.iter().chain(point.iter()).all(|v| v.is_finite())
            || ratio.is_some_and(|r| !r.is_finite() || r <= 0.)
        {
            return None;
        }
        let delta = [point[0] - self.start[0], point[1] - self.start[1]];
        let frame = match self.mode {
            CropMode::Create => {
                let [mut dx, mut dy] = delta;
                if let Some(ratio) = ratio {
                    if dx.abs() > dy.abs() * ratio {
                        dy = if dy < 0. {
                            -dx.abs() / ratio
                        } else {
                            dx.abs() / ratio
                        };
                    } else {
                        dx = if dx < 0. {
                            -dy.abs() * ratio
                        } else {
                            dy.abs() * ratio
                        };
                    }
                }
                if symmetric {
                    [
                        self.start[0] - dx.abs(),
                        self.start[1] - dy.abs(),
                        dx.abs() * 2.,
                        dy.abs() * 2.,
                    ]
                } else {
                    [
                        self.start[0].min(self.start[0] + dx),
                        self.start[1].min(self.start[1] + dy),
                        dx.abs(),
                        dy.abs(),
                    ]
                }
            }
            CropMode::Move => [
                self.original[0] + delta[0],
                self.original[1] + delta[1],
                self.original[2],
                self.original[3],
            ],
            CropMode::Resize(index) => {
                if !valid(self.original) {
                    return None;
                }
                let handle = *HANDLES.get(index)?;
                let anchor = if symmetric {
                    [0.5, 0.5]
                } else {
                    [1. - handle[0], 1. - handle[1]]
                };
                let span = if symmetric { 2. } else { 1. };
                let local = [
                    ((handle[0] - anchor[0]) * self.original[2] + delta[0]) * span,
                    ((handle[1] - anchor[1]) * self.original[3] + delta[1]) * span,
                ];
                let signs = handle.map(|value| value * 2. - 1.);
                let raw = std::array::from_fn::<_, 2, _>(|axis| {
                    if signs[axis] == 0. {
                        self.original[axis + 2]
                    } else {
                        local[axis] * signs[axis]
                    }
                });
                let mut size = raw.map(|value| value.abs().max(1.));
                if ratio.is_some() {
                    let [w, h] = [self.original[2], self.original[3]];
                    let factor = if signs[0] == 0. {
                        size[1] / h
                    } else if signs[1] == 0. {
                        size[0] / w
                    } else {
                        ((local[0] * signs[0] * w + local[1] * signs[1] * h) / (w * w + h * h))
                            .max(1. / w.min(h))
                    };
                    size = [w * factor, h * factor];
                }
                let center = std::array::from_fn::<_, 2, _>(|axis| {
                    self.original[axis]
                        + anchor[axis] * self.original[axis + 2]
                        + (0.5 - anchor[axis]) * size[axis] * if raw[axis] < 0. { -1. } else { 1. }
                });
                [
                    center[0] - size[0] / 2.,
                    center[1] - size[1] / 2.,
                    size[0],
                    size[1],
                ]
            }
        };
        Some(frame)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CropSnap {
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    pub tolerance: f64,
}

fn nearest(value: f64, targets: &[f64], tolerance: f64) -> Option<f64> {
    targets
        .iter()
        .copied()
        .filter(|target| target.is_finite() && (target - value).abs() <= tolerance)
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
}

impl CropSnap {
    /// Fixed proportions snap on moves only; other drags snap only their moving edges.
    pub fn apply(
        &self,
        frame: Frame,
        drag: &CropDrag,
        point: [f64; 2],
        ratio: Option<f64>,
        symmetric: bool,
    ) -> Frame {
        if !self.tolerance.is_finite() || self.tolerance <= 0. || !valid(frame) {
            return frame;
        }
        if drag.mode == CropMode::Move {
            let mut result = frame;
            for (axis, targets) in [(0, &self.xs), (1, &self.ys)] {
                let shift = [frame[axis], frame[axis] + frame[axis + 2]]
                    .into_iter()
                    .filter_map(|edge| {
                        nearest(edge, targets, self.tolerance).map(|target| target - edge)
                    })
                    .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                    .unwrap_or(0.);
                result[axis] += shift;
            }
            return result;
        }
        if ratio.is_some() {
            return frame;
        }
        let axes = match drag.mode {
            CropMode::Create => [true, true],
            CropMode::Resize(index) => match HANDLES.get(index) {
                Some(handle) => handle.map(|v| v != 0.5),
                None => return frame,
            },
            CropMode::Move => unreachable!(),
        };
        let mut result = frame;
        for (axis, targets) in [(0, &self.xs), (1, &self.ys)] {
            if !axes[axis] {
                continue;
            }
            let min = result[axis];
            let max = min + result[axis + 2];
            if (point[axis] - min).abs() <= (point[axis] - max).abs() {
                if let Some(target) = nearest(min, targets, self.tolerance).filter(|v| *v < max) {
                    result[axis] = target;
                    result[axis + 2] = max - target;
                }
            } else if let Some(target) = nearest(max, targets, self.tolerance).filter(|v| *v > min)
            {
                result[axis + 2] = target - min;
            }
            if symmetric {
                let center = if drag.mode == CropMode::Create {
                    drag.start[axis]
                } else {
                    drag.original[axis] + drag.original[axis + 2] / 2.
                };
                let half = if point[axis] >= center {
                    result[axis] + result[axis + 2] - center
                } else {
                    center - result[axis]
                };
                if half >= 0.5 {
                    result[axis] = center - half;
                    result[axis + 2] = half * 2.;
                }
            }
        }
        result
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SnapOptions {
    pub canvas: bool,
    pub layers: bool,
    /// Set false when guides are hidden.
    pub guides: bool,
    /// Visible grid's subdivision spacing, at least one pixel. None when the grid is hidden.
    pub grid_spacing: Option<f64>,
    pub include_centers: bool,
}
impl Default for SnapOptions {
    fn default() -> Self {
        Self {
            canvas: true,
            layers: true,
            guides: true,
            grid_spacing: None,
            include_centers: false,
        }
    }
}

pub fn targets(
    document: &Document,
    options: SnapOptions,
    excluding: &HashSet<Uuid>,
    tolerance: f64,
) -> CropSnap {
    let mut snap = CropSnap {
        tolerance,
        ..Default::default()
    };
    if options.canvas {
        snap.xs.extend([0., document.width as f64]);
        snap.ys.extend([0., document.height as f64]);
        if options.include_centers {
            snap.xs.push(document.width as f64 / 2.);
            snap.ys.push(document.height as f64 / 2.);
        }
    }
    if options.layers {
        for layer in &document.layers {
            if layer.image.is_none() || excluding.contains(&layer.id) || !layer.visible {
                continue;
            }
            let mut parent = layer.parent_id;
            let mut visible = true;
            for _ in 0..64 {
                let Some(id) = parent else {
                    break;
                };
                let Some(layer) = document.layer(id) else {
                    visible = false;
                    break;
                };
                if !layer.visible {
                    visible = false;
                    break;
                }
                parent = layer.parent_id;
            }
            if !visible || parent.is_some() {
                continue;
            }
            let corners = layer.transform.corners();
            for (axis, lines) in [(0, &mut snap.xs), (1, &mut snap.ys)] {
                let min = corners
                    .iter()
                    .map(|p| p[axis])
                    .fold(f64::INFINITY, f64::min);
                let max = corners
                    .iter()
                    .map(|p| p[axis])
                    .fold(f64::NEG_INFINITY, f64::max);
                lines.extend([min.round(), max.round()]);
                if options.include_centers {
                    lines.push(((min + max) / 2.).round());
                }
            }
        }
    }
    if options.guides {
        for guide in &document.guides {
            if guide.axis == "vertical" {
                snap.xs.push(guide.position);
            } else if guide.axis == "horizontal" {
                snap.ys.push(guide.position);
            }
        }
    }
    if let Some(spacing) = options.grid_spacing.filter(|v| v.is_finite() && *v >= 1.) {
        for (length, lines) in [
            (document.width, &mut snap.xs),
            (document.height, &mut snap.ys),
        ] {
            lines.extend(
                (0..=(length as f64 / spacing + 0.001).floor() as u32)
                    .map(|index| (index as f64 * spacing).round()),
            );
        }
    }
    for lines in [&mut snap.xs, &mut snap.ys] {
        lines.retain(|v| v.is_finite());
        lines.sort_by(f64::total_cmp);
        lines.dedup();
    }
    snap
}

/// Nudges the pointer, so a proportional resize can snap one edge and preserve its diagonal.
/// `update` is the caller's ordinary resize function. Rotated layers retain the input point.
pub fn snap_resize_point(
    original: Transform,
    handle: usize,
    start: [f64; 2],
    point: [f64; 2],
    proportional: bool,
    snap: &CropSnap,
    update: impl Fn([f64; 2]) -> Transform,
) -> ([f64; 2], [Option<f64>; 2]) {
    if original.rotation != 0. || snap.tolerance <= 0. || !snap.tolerance.is_finite() {
        return (point, [None; 2]);
    }
    let Some(handle) = HANDLES.get(handle) else {
        return (point, [None; 2]);
    };
    let at = std::array::from_fn::<_, 2, _>(|axis| {
        original.origin[axis] + original.size[axis] * handle[axis] + point[axis] - start[axis]
    });
    let edge = |transform: Transform, axis: usize| {
        let min = transform.origin[axis];
        let max = min + transform.size[axis];
        if (min - at[axis]).abs() <= (max - at[axis]).abs() {
            min
        } else {
            max
        }
    };
    let draft = update(point);
    let mut snaps = [None; 2];
    for (axis, targets) in [(0, &snap.xs), (1, &snap.ys)] {
        if handle[axis] != 0.5 {
            snaps[axis] = nearest(edge(draft, axis), targets, snap.tolerance);
        }
    }
    if proportional && let [Some(x), Some(y)] = snaps {
        let farther = if (x - edge(draft, 0)).abs() <= (y - edge(draft, 1)).abs() {
            1
        } else {
            0
        };
        snaps[farther] = None;
    }
    let mut result = point;
    for (axis, target) in snaps.iter_mut().enumerate() {
        let Some(line) = *target else {
            continue;
        };
        let before = edge(update(result), axis);
        let mut nudged = result;
        nudged[axis] += 1.;
        let per_pixel = edge(update(nudged), axis) - before;
        if per_pixel.abs() <= 0.01 {
            *target = None;
            continue;
        }
        result[axis] += (line - before) / per_pixel;
    }
    (result, snaps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Guide, Layer, Raster};
    fn drag(original: Frame, mode: CropMode, start: [f64; 2]) -> CropDrag {
        CropDrag {
            original,
            mode,
            start,
        }
    }
    #[test]
    fn reverse_create_move_and_every_ratio_handle_match_original_crop_tests() {
        let rect = drag([0.; 4], CropMode::Create, [100., 100.])
            .updated([20., 60.], Some(2.), false)
            .unwrap();
        assert_eq!(rect, [20., 60., 80., 40.]);
        assert_eq!(
            drag(rect, CropMode::Move, [40., 70.]).updated([30., 40.], None, false),
            Some([10., 30., 80., 40.])
        );
        for (index, unit) in HANDLES.iter().enumerate() {
            let start = [rect[0] + unit[0] * rect[2], rect[1] + unit[1] * rect[3]];
            let point = [
                start[0] + (unit[0] * 2. - 1.) * 20.,
                start[1] + (unit[1] * 2. - 1.) * 10.,
            ];
            let next = drag(rect, CropMode::Resize(index), start)
                .updated(point, Some(2.), false)
                .unwrap();
            assert!(valid(next));
            assert!((next[2] / next[3] - 2.).abs() < 0.05);
            assert_ne!(next, rect);
        }
    }
    #[test]
    fn create_and_resize_keep_center_with_alt_and_cross_the_anchor() {
        assert_eq!(
            drag([0.; 4], CropMode::Create, [50., 50.]).updated([60., 70.], None, true),
            Some([40., 30., 20., 40.])
        );
        let resize = drag([10., 20., 100., 50.], CropMode::Resize(2), [110., 70.]);
        assert_eq!(
            resize.updated([120., 80.], None, true),
            Some([0., 10., 120., 70.])
        );
        assert_eq!(
            resize.updated([-40., -5.], None, false),
            Some([-40., -5., 50., 25.])
        );
    }
    #[test]
    fn crop_snap_matches_original_move_resize_create_and_fixed_ratio_cases() {
        let snap = CropSnap {
            xs: vec![0., 200., 50., 150.],
            ys: vec![0., 100., 20., 80.],
            tolerance: 6.,
        };
        let rect = [10., 10., 60., 40.];
        let moved = drag(rect, CropMode::Move, [30., 30.]);
        let point = [26., 34.];
        assert_eq!(
            snap.apply(
                moved.updated(point, None, false).unwrap(),
                &moved,
                point,
                None,
                false
            ),
            [0., 20., 60., 40.]
        );
        let resize = drag(rect, CropMode::Resize(2), [70., 50.]);
        for (point, expected) in [
            ([146., 83.], [10., 10., 140., 70.]),
            ([120., 60.], [10., 10., 110., 50.]),
        ] {
            assert_eq!(
                snap.apply(
                    resize.updated(point, None, false).unwrap(),
                    &resize,
                    point,
                    None,
                    false
                ),
                expected
            );
        }
        let ratio = [10., 10., 138., 69.];
        assert_eq!(
            snap.apply(ratio, &resize, [148., 79.], Some(2.), false),
            ratio
        );
        let create = drag([0.; 4], CropMode::Create, [52., 18.]);
        let point = [147., 77.];
        assert_eq!(
            snap.apply(
                create.updated(point, None, false).unwrap(),
                &create,
                point,
                None,
                false
            ),
            [52., 18., 98., 62.]
        );
    }
    #[test]
    fn symmetric_snap_mirrors_the_dragged_edge_about_original_center() {
        let snap = CropSnap {
            xs: vec![130.],
            ys: vec![],
            tolerance: 6.,
        };
        let resize = drag([10., 20., 100., 50.], CropMode::Resize(5), [110., 45.]);
        let point = [127., 45.];
        let frame = resize.updated(point, None, true).unwrap();
        assert_eq!(
            snap.apply(frame, &resize, point, None, true),
            [-10., 20., 140., 50.]
        );
    }
    #[test]
    fn integer_rounding_and_invalid_parameters_are_checked() {
        assert_eq!(snapped([10.4, 20.6, -4.2, 0.1]), [6., 21., 4., 1.]);
        let create = drag([0.; 4], CropMode::Create, [0., 0.]);
        assert!(create.updated([1., 1.], Some(0.), false).is_none());
        assert!(create.updated([f64::NAN, 1.], None, false).is_none());
        assert!(
            create
                .updated([MAX_SIDE as f64 + 1., 1.], None, false)
                .is_none()
        );
        assert!(!valid([1_000_001., 0., 1., 1.]));
    }
    #[test]
    fn targets_use_canvas_rotated_layer_bounds_guides_and_visible_grid() {
        let mut document = Document::new(400, 300);
        let mut layer = Layer::raster("Visible", Raster::solid(100, 60, [255; 4]));
        layer.transform = Transform::new(150., 120., 100., 60.);
        let id = layer.id;
        document.layers.push(layer);
        let options = SnapOptions::default();
        let snap = targets(&document, options, &HashSet::new(), 6.);
        assert_eq!(snap.xs, [0., 150., 250., 400.]);
        assert_eq!(snap.ys, [0., 120., 180., 300.]);
        document.layers.last_mut().unwrap().transform.rotation = 90.;
        document.guides.push(Guide {
            id: Uuid::new_v4(),
            axis: "vertical".into(),
            position: 13.,
        });
        let snap = targets(
            &document,
            SnapOptions {
                grid_spacing: Some(100.),
                ..options
            },
            &HashSet::new(),
            6.,
        );
        assert_eq!(snap.xs, [0., 13., 100., 170., 200., 230., 300., 400.]);
        assert_eq!(snap.ys, [0., 100., 200., 300.]);
        let snap = targets(
            &document,
            SnapOptions {
                guides: false,
                ..options
            },
            &HashSet::from([id]),
            6.,
        );
        assert_eq!(snap.xs, [0., 400.]);
        assert_eq!(snap.ys, [0., 300.]);
    }
    #[test]
    fn hidden_parent_hides_child_snap_targets() {
        let mut document = Document::new(400, 300);
        let mut group = Layer::new("Hidden group", 1, 1);
        group.is_group = true;
        group.visible = false;
        let mut child = Layer::raster("Child", Raster::solid(10, 10, [255; 4]));
        child.parent_id = Some(group.id);
        child.transform = Transform::new(17., 23., 10., 10.);
        document.layers.extend([group, child]);
        let snap = targets(&document, SnapOptions::default(), &HashSet::new(), 6.);
        assert_eq!(snap.xs, [0., 400.]);
        assert_eq!(snap.ys, [0., 300.]);
    }
    #[test]
    fn resize_snap_original_side_proportional_corner_and_rotation_cases() {
        let original = Transform::new(10., 10., 100., 100.);
        let snap = CropSnap {
            xs: vec![0., 150., 300.],
            ys: vec![0., 150., 200.],
            tolerance: 5.,
        };
        let side = drag([10., 10., 100., 100.], CropMode::Resize(5), [110., 60.]);
        let update = |point| {
            let r = side.draft(point, None, false).unwrap();
            Transform::new(r[0], r[1], r[2], r[3])
        };
        let (point, lines) =
            snap_resize_point(original, 5, side.start, [147., 60.], false, &snap, update);
        assert_eq!(point, [150., 60.]);
        assert_eq!(lines, [Some(150.), None]);
        assert_eq!(update(point).size, [140., 100.]);
        assert_eq!(
            snap_resize_point(original, 5, side.start, [130., 60.], false, &snap, update).0,
            [130., 60.]
        );
        let corner = drag([10., 10., 100., 100.], CropMode::Resize(2), [110., 110.]);
        let update = |point| {
            let r = corner.draft(point, Some(1.), false).unwrap();
            Transform::new(r[0], r[1], r[2], r[3])
        };
        let (point, lines) =
            snap_resize_point(original, 2, corner.start, [146., 148.], true, &snap, update);
        let next = update(point);
        assert_eq!(next.origin[0] + next.size[0], 150.);
        assert_eq!(next.size[0], next.size[1]);
        assert!(lines[0].is_some() ^ lines[1].is_some());
        let mut rotated = original;
        rotated.rotation = 20.;
        assert_eq!(
            snap_resize_point(rotated, 5, side.start, [147., 60.], false, &snap, update).0,
            [147., 60.]
        );
    }
}
