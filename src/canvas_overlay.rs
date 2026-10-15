//! Cached selection boundaries and background layer thumbnails.
use crate::{
    model::{Document, Raster, Selection},
    pixels, preview, selection,
};
use eframe::egui;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
};
use uuid::Uuid;

type Segment = ([f64; 2], [f64; 2]);
#[derive(Default)]
pub struct SelectionOutlineCache {
    source: Option<Selection>,
    max_side: u32,
    lines: Vec<Segment>,
}
impl SelectionOutlineCache {
    /// Pixel-edge boundaries, including holes. Very large masks use a bounded preview grid.
    pub fn lines(&mut self, selection: &Selection) -> &[Segment] {
        self.lines_at(selection, 1000)
    }
    pub fn lines_at(&mut self, selection: &Selection, max_side: u32) -> &[Segment] {
        let selection = selection.outline();
        let max_side = max_side.clamp(1, 8000);
        if self.max_side == max_side
            && self
                .source
                .as_ref()
                .is_some_and(|s| s.same_coverage(selection))
        {
            return &self.lines;
        }
        self.max_side = max_side;
        self.source = Some(selection.clone());
        self.lines.clear();
        if selection.width == 0 || selection.height == 0 {
            return &self.lines;
        }
        for [x0, y0, x1, y1] in selection::region_bounds(selection, 0, false) {
            let span_x = (x1 - x0) as f64;
            let span_y = (y1 - y0) as f64;
            let scale = (max_side as f64 / span_x.max(span_y)).min(1.);
            let width = (span_x * scale).round().max(1.) as usize;
            let height = (span_y * scale).round().max(1.) as usize;
            let sx = span_x / width as f64;
            let sy = span_y / height as f64;
            let mut mask = vec![0; width * height];
            for y in 0..height {
                for x in 0..width {
                    let source_x = x0 + ((x as f64 + 0.5) * sx).floor() as i64;
                    let source_y = y0 + ((y as f64 + 0.5) * sy).floor() as i64;
                    if selection.get_unclipped(source_x, source_y) >= 128 {
                        mask[y * width + x] = 255;
                    }
                }
            }
            if let Ok(paths) = pixels::trace_mask(&mask, width, height) {
                for path in paths {
                    if path.len() < 2 {
                        continue;
                    }
                    let mut start = path[0];
                    let mut previous = path[0];
                    let mut direction = None;
                    // trace_mask returns corner vertices without repeating the first vertex.
                    for &point in path.iter().skip(1).chain(std::iter::once(&path[0])) {
                        let next_direction = [
                            (point[0] - previous[0]).signum(),
                            (point[1] - previous[1]).signum(),
                        ];
                        if direction.is_some_and(|d| d != next_direction) {
                            self.lines.push((
                                [
                                    x0 as f64 + start[0] as f64 * sx,
                                    y0 as f64 + start[1] as f64 * sy,
                                ],
                                [
                                    x0 as f64 + previous[0] as f64 * sx,
                                    y0 as f64 + previous[1] as f64 * sy,
                                ],
                            ));
                            start = previous;
                        }
                        direction = Some(next_direction);
                        previous = point;
                    }
                    self.lines.push((
                        [
                            x0 as f64 + start[0] as f64 * sx,
                            y0 as f64 + start[1] as f64 * sy,
                        ],
                        [
                            x0 as f64 + previous[0] as f64 * sx,
                            y0 as f64 + previous[1] as f64 * sy,
                        ],
                    ));
                }
            }
        }
        &self.lines
    }
}
struct ThumbnailEntry {
    source: Arc<Raster>,
    revision: u64,
    texture: egui::TextureHandle,
}
#[derive(Default)]
struct ThumbnailResults {
    revisions: HashMap<Uuid, u64>,
    ready: HashMap<Uuid, (u64, Raster)>,
}
struct ThumbnailRequest {
    id: Uuid,
    revision: u64,
    source: Arc<Raster>,
    results: Weak<Mutex<ThumbnailResults>>,
    context: egui::Context,
}
impl ThumbnailRequest {
    fn publish(&self, thumbnail: Raster) -> bool {
        let Some(results) = self.results.upgrade() else {
            return false;
        };
        let mut results = results.lock().unwrap_or_else(|e| e.into_inner());
        if results.revisions.get(&self.id) != Some(&self.revision) {
            return false;
        }
        results.ready.insert(self.id, (self.revision, thumbnail));
        drop(results);
        self.context.request_repaint();
        true
    }
}
type ThumbnailKey = (Uuid, Uuid);
#[derive(Default)]
struct ThumbnailQueue {
    pending: HashMap<ThumbnailKey, ThumbnailRequest>,
    order: VecDeque<ThumbnailKey>,
}
impl ThumbnailQueue {
    fn insert(&mut self, key: ThumbnailKey, request: ThumbnailRequest) {
        if self.pending.insert(key, request).is_none() {
            self.order.push_back(key);
        }
    }
    fn pop(&mut self) -> Option<ThumbnailRequest> {
        self.order
            .pop_front()
            .and_then(|key| self.pending.remove(&key))
    }
    fn retain(&mut self, cache: Uuid, ids: &HashSet<Uuid>) {
        self.pending
            .retain(|(owner, id), _| *owner != cache || ids.contains(id));
        self.order.retain(|key| self.pending.contains_key(key));
    }
}
struct ThumbnailScheduler {
    queue: Mutex<ThumbnailQueue>,
    wake: Condvar,
}
impl ThumbnailScheduler {
    fn shared() -> Arc<Self> {
        static SCHEDULER: OnceLock<Arc<ThumbnailScheduler>> = OnceLock::new();
        SCHEDULER
            .get_or_init(|| {
                let scheduler = Arc::new(Self {
                    queue: Mutex::new(ThumbnailQueue::default()),
                    wake: Condvar::new(),
                });
                let worker = scheduler.clone();
                std::thread::spawn(move || worker.run());
                scheduler
            })
            .clone()
    }
    fn submit(&self, cache: Uuid, request: ThumbnailRequest) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((cache, request.id), request);
        self.wake.notify_one();
    }
    fn retain(&self, cache: Uuid, ids: &HashSet<Uuid>) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(cache, ids);
    }
    fn run(&self) {
        loop {
            let request = {
                let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(request) = queue.pop() {
                        break request;
                    }
                    queue = self.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
                }
            };
            let Some(results) = request.results.upgrade() else {
                continue;
            };
            if results
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .revisions
                .get(&request.id)
                != Some(&request.revision)
            {
                continue;
            }
            drop(results);
            let source = &request.source;
            let scale = (64.0 / source.width.max(source.height).max(1) as f64).min(1.0);
            let width = (source.width as f64 * scale).round().max(1.) as u32;
            let height = (source.height as f64 * scale).round().max(1.) as u32;
            let thumbnail = preview::area_resize(source, width, height);
            request.publish(thumbnail);
        }
    }
}
pub struct ThumbnailCache {
    id: Uuid,
    entries: HashMap<Uuid, ThumbnailEntry>,
    results: Arc<Mutex<ThumbnailResults>>,
    scheduler: Option<Arc<ThumbnailScheduler>>,
    revision: u64,
}
impl Default for ThumbnailCache {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            entries: HashMap::new(),
            results: Arc::new(Mutex::new(ThumbnailResults::default())),
            scheduler: None,
            revision: 0,
        }
    }
}
impl ThumbnailCache {
    pub fn retain(&mut self, ids: &HashSet<Uuid>) {
        self.entries.retain(|id, _| ids.contains(id));
        let mut results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        results.revisions.retain(|id, _| ids.contains(id));
        results.ready.retain(|id, _| ids.contains(id));
        drop(results);
        if let Some(scheduler) = &self.scheduler {
            scheduler.retain(self.id, ids);
        }
    }
    pub fn retain_document(&mut self, doc: &Document) {
        if self
            .entries
            .keys()
            .any(|id| doc.layer(*id).is_none_or(|layer| layer.image.is_none()))
        {
            let ids = doc
                .layers
                .iter()
                .filter(|layer| layer.image.is_some())
                .map(|layer| layer.id)
                .collect();
            self.retain(&ids);
        }
    }
    pub fn clear(&mut self) {
        self.retain(&HashSet::new());
    }
    /// Returns immediately; a changed immutable raster is reduced off the UI thread.
    pub fn get(&mut self, ctx: &egui::Context, id: Uuid, image: &Arc<Raster>) -> egui::TextureId {
        let ready =
            std::mem::take(&mut self.results.lock().unwrap_or_else(|e| e.into_inner()).ready);
        for (id, (revision, raster)) in ready {
            if let Some(entry) = self.entries.get_mut(&id)
                && entry.revision == revision
            {
                entry.texture.set(
                    egui::ColorImage::from_rgba_unmultiplied(
                        [raster.width as usize, raster.height as usize],
                        &raster.to_rgba(),
                    ),
                    egui::TextureOptions::LINEAR,
                );
            }
        }
        let unchanged = self
            .entries
            .get(&id)
            .is_some_and(|entry| same_pixels(&entry.source, image));
        if !unchanged {
            self.revision = self.revision.wrapping_add(1);
            let revision = self.revision;
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.source = image.clone();
                entry.revision = revision;
            } else {
                let placeholder =
                    egui::ColorImage::new([2, 2], vec![egui::Color32::TRANSPARENT; 4]);
                let texture = ctx.load_texture(
                    format!("layer-thumbnail-{id}"),
                    placeholder,
                    egui::TextureOptions::LINEAR,
                );
                self.entries.insert(
                    id,
                    ThumbnailEntry {
                        source: image.clone(),
                        revision,
                        texture,
                    },
                );
            }
            self.results
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .revisions
                .insert(id, revision);
            let scheduler = self
                .scheduler
                .get_or_insert_with(ThumbnailScheduler::shared);
            scheduler.submit(
                self.id,
                ThumbnailRequest {
                    id,
                    revision,
                    source: image.clone(),
                    results: Arc::downgrade(&self.results),
                    context: ctx.clone(),
                },
            );
        }
        self.entries[&id].texture.id()
    }
}
impl Drop for ThumbnailCache {
    fn drop(&mut self) {
        self.clear();
    }
}
fn same_pixels(a: &Arc<Raster>, b: &Arc<Raster>) -> bool {
    Arc::ptr_eq(a, b)
        || (a.width == b.width
            && a.height == b.height
            && Arc::ptr_eq(&a.pixels, &b.pixels)
            && a.tiles.len() == b.tiles.len()
            && a.tiles.iter().all(|(key, value)| {
                b.tiles
                    .get(key)
                    .is_some_and(|other| Arc::ptr_eq(value, other))
            }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn thumbnail_request(
        id: Uuid,
        revision: u64,
        source: Arc<Raster>,
        results: &Arc<Mutex<ThumbnailResults>>,
    ) -> ThumbnailRequest {
        ThumbnailRequest {
            id,
            revision,
            source,
            results: Arc::downgrade(results),
            context: egui::Context::default(),
        }
    }
    #[test]
    fn thumbnail_queue_keeps_only_the_latest_pending_image_and_preserves_other_layers() {
        let cache = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let results = Arc::new(Mutex::new(ThumbnailResults::default()));
        let old = Arc::new(Raster::solid(8, 8, [10; 4]));
        let latest = Arc::new(Raster::solid(8, 8, [40; 4]));
        let mut queue = ThumbnailQueue::default();
        queue.insert(
            (cache, first),
            thumbnail_request(first, 1, old.clone(), &results),
        );
        queue.insert(
            (cache, second),
            thumbnail_request(second, 2, latest.clone(), &results),
        );
        for revision in 3..103 {
            queue.insert(
                (cache, first),
                thumbnail_request(first, revision, latest.clone(), &results),
            );
        }
        assert_eq!(queue.pending.len(), 2);
        assert_eq!(queue.order.len(), 2);
        assert_eq!(
            Arc::strong_count(&old),
            1,
            "Superseded source pixels must be released"
        );
        let request = queue.pop().unwrap();
        assert_eq!(request.id, first);
        assert_eq!(request.revision, 102);
        assert!(Arc::ptr_eq(&request.source, &latest));
        assert_eq!(queue.pop().unwrap().id, second);
        assert!(queue.pop().is_none());
    }
    #[test]
    fn deleted_layers_and_closed_caches_release_queued_images() {
        let cache = Uuid::new_v4();
        let other = Uuid::new_v4();
        let removed = Uuid::new_v4();
        let retained = Uuid::new_v4();
        let results = Arc::new(Mutex::new(ThumbnailResults::default()));
        let source = Arc::new(Raster::solid(8, 8, [255; 4]));
        let mut queue = ThumbnailQueue::default();
        queue.insert(
            (cache, removed),
            thumbnail_request(removed, 1, source.clone(), &results),
        );
        queue.insert(
            (cache, retained),
            thumbnail_request(retained, 2, source.clone(), &results),
        );
        queue.insert(
            (other, removed),
            thumbnail_request(removed, 3, source.clone(), &results),
        );
        queue.retain(cache, &HashSet::from([retained]));
        assert_eq!(Arc::strong_count(&source), 3);
        assert_eq!(queue.order.len(), 2);
        queue.retain(cache, &HashSet::new());
        assert_eq!(Arc::strong_count(&source), 2);
        assert_eq!(queue.pop().unwrap().revision, 3);
        assert_eq!(Arc::strong_count(&source), 1);
    }
    #[test]
    fn stale_thumbnail_results_are_discarded_and_current_results_replace_ready_images() {
        let id = Uuid::new_v4();
        let results = Arc::new(Mutex::new(ThumbnailResults::default()));
        let source = Arc::new(Raster::solid(8, 8, [255; 4]));
        let stale = thumbnail_request(id, 1, source.clone(), &results);
        let current = thumbnail_request(id, 2, source, &results);
        results.lock().unwrap().revisions.insert(id, 2);
        assert!(!stale.publish(Raster::solid(1, 1, [10; 4])));
        assert!(current.publish(Raster::solid(1, 1, [20; 4])));
        assert!(current.publish(Raster::solid(1, 1, [30; 4])));
        assert_eq!(results.lock().unwrap().ready.len(), 1);
        assert_eq!(results.lock().unwrap().ready[&id].1.get(0, 0), [30; 4]);
        results.lock().unwrap().revisions.remove(&id);
        assert!(!current.publish(Raster::solid(1, 1, [40; 4])));
        drop(results);
        assert!(!current.publish(Raster::solid(1, 1, [50; 4])));
    }
    #[test]
    fn outline_is_pixel_edge_geometry_and_keeps_holes() {
        let mut mask = Selection::empty(8, 8);
        for y in 1..7 {
            for x in 1..7 {
                mask.set(x, y, 255);
            }
        }
        for y in 3..5 {
            for x in 3..5 {
                mask.set(x, y, 0);
            }
        }
        let mut cache = SelectionOutlineCache::default();
        let lines = cache.lines(&mask).to_vec();
        assert_eq!(lines.len(), 8);
        assert!(lines.contains(&([1., 1.], [7., 1.])));
        assert!(lines.iter().any(|&(a, b)| a == [3., 3.] || b == [3., 3.]));
        let pointer = cache.lines(&mask).as_ptr();
        assert_eq!(pointer, cache.lines(&mask).as_ptr());
        mask.set(0, 0, 255);
        assert_eq!(cache.lines(&mask).len(), 12);
    }
    #[test]
    fn thumbnail_identity_includes_dirty_tile_data() {
        let mut image = Raster::solid(1024, 1024, [255; 4]);
        image.set(1, 1, [0; 4]);
        let a = Arc::new(image);
        let mut b = (*a).clone();
        assert!(same_pixels(&a, &Arc::new(b.clone())));
        b.set(1, 1, [90, 100, 120, 255]);
        assert!(!same_pixels(&a, &Arc::new(b)));
    }
    #[test]
    fn outline_moves_outside_canvas_and_joins_overlapping_grids() {
        let original = selection::marquee(10, 10, [2., 2., 4., 4.], false, false);
        let outside = selection::translated(&original, -4, 0, 10, 10);
        let mut cache = SelectionOutlineCache::default();
        assert_eq!(cache.lines(&original).len(), 4);
        let shifted = cache.lines(&outside);
        assert_eq!(shifted.len(), 4);
        assert!(shifted.iter().any(|&(a, b)| a[0] == -2. || b[0] == -2.));
        let second = selection::marquee(10, 10, [1., 2., 4., 4.], false, false);
        let added = selection::combine(Some(&outside), &second, selection::SelectionMode::Add);
        assert_eq!(cache.lines(&added).len(), 4);
        let removed = selection::combine(Some(&added), &second, selection::SelectionMode::Subtract);
        assert_eq!(cache.lines(&removed).len(), 4);
    }
}
