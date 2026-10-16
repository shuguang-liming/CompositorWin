use crate::model::Document;
use anyhow::{Result, ensure};
use std::{collections::HashSet, sync::Arc};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Retained pixel buffers in the current documents, deduplicated across tabs and layers.
/// Counts Vec capacity, including sparse raster tiles and every selection region.
/// GUI/GPU caches, metadata and in-flight codec allocations are outside this measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceUsage {
    pub documents: usize,
    pub unique_allocations: usize,
    pub raster_bytes: u64,
    pub selection_bytes: u64,
    pub current_bytes: u64,
}

pub fn workspace_usage<'a>(documents: impl IntoIterator<Item = &'a Document>) -> WorkspaceUsage {
    fn allocation(buffer: &Arc<Vec<u8>>, seen: &mut HashSet<usize>) -> u64 {
        if buffer.capacity() == 0 || !seen.insert(Arc::as_ptr(buffer) as usize) {
            return 0;
        }
        buffer.capacity() as u64
    }

    let mut seen = HashSet::new();
    let mut usage = WorkspaceUsage::default();
    for document in documents {
        usage.documents += 1;
        for layer in &document.layers {
            for raster in [
                layer.image.as_ref(),
                layer.mask.as_ref().map(|mask| &mask.raster),
            ]
            .into_iter()
            .flatten()
            {
                usage.raster_bytes = usage
                    .raster_bytes
                    .saturating_add(allocation(&raster.pixels, &mut seen));
                for tile in raster.tiles.values() {
                    usage.raster_bytes = usage
                        .raster_bytes
                        .saturating_add(allocation(tile, &mut seen));
                }
            }
        }
        if let Some(selection) = &document.selection {
            for coverage in selection.allocations() {
                usage.selection_bytes = usage
                    .selection_bytes
                    .saturating_add(allocation(coverage, &mut seen));
            }
        }
    }
    usage.unique_allocations = seen.len();
    usage.current_bytes = usage.raster_bytes.saturating_add(usage.selection_bytes);
    usage
}

pub fn physical_memory_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // Windows fills this sized value synchronously and retains no pointer.
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 && status.ullTotalPhys > 0 {
            return Some(status.ullTotalPhys);
        }
    }
    None
}

fn budget_for_memory(physical_bytes: Option<u64>) -> u64 {
    physical_bytes
        .map(|bytes| bytes / 3)
        .unwrap_or(512 * MIB)
        .clamp(256 * MIB, 6 * GIB)
}

/// One third of installed RAM, bounded to 256 MiB..6 GiB; 512 MiB if detection fails.
/// The remainder covers the OS, GPU/preview caches and temporary decoding/rendering buffers.
pub fn workspace_budget_bytes() -> u64 {
    #[cfg(test)]
    if let Some(budget) = TEST_WORKSPACE_BUDGET.with(std::cell::Cell::get) {
        return budget;
    }
    budget_for_memory(physical_memory_bytes())
}

#[cfg(test)]
thread_local! {
    static TEST_WORKSPACE_BUDGET: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_test_workspace_budget<T>(budget: u64, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_WORKSPACE_BUDGET.with(|value| value.set(self.0));
        }
    }
    let _restore = Restore(TEST_WORKSPACE_BUDGET.with(|value| value.replace(Some(budget))));
    operation()
}

/// `retained_history_bytes` is an upper bound supplied by the session histories.
/// It must exclude buffers already counted in each history's current document.
pub fn ensure_workspace_budget<'a>(
    documents: impl IntoIterator<Item = &'a Document>,
    retained_history_bytes: u64,
) -> Result<WorkspaceUsage> {
    check_workspace_budget(documents, retained_history_bytes, workspace_budget_bytes())
}

pub fn check_workspace_budget<'a>(
    documents: impl IntoIterator<Item = &'a Document>,
    retained_history_bytes: u64,
    budget_bytes: u64,
) -> Result<WorkspaceUsage> {
    let usage = workspace_usage(documents);
    let total_bytes = usage.current_bytes.saturating_add(retained_history_bytes);
    ensure!(
        total_bytes <= budget_bytes,
        "打开的项目占用 {} 字节的像素与选区内存，加上 {} 字节的撤销历史 \
         （共 {} 字节），超出工作区内存上限 {} 字节。\
         请关闭部分项目或减小图像尺寸后再执行此操作。",
        usage.current_bytes,
        retained_history_bytes,
        total_bytes,
        budget_bytes,
    );
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Layer, Mask, Raster, Selection, SelectionRegion, TILE_SIZE};

    #[test]
    fn shared_rasters_and_tiles_are_counted_once_across_tabs_and_masks() {
        let mut raster = Raster::from_rgba(16, 16, vec![128; 16 * 16 * 4]);
        raster.set(0, 0, [255; 4]);
        let expected = raster.pixels.capacity() + raster.tiles.values().next().unwrap().capacity();
        let raster = Arc::new(raster);
        let mut first = Document::new(16, 16);
        first.layers[0].image = Some(raster.clone());
        first.layers[0].mask = Some(Mask {
            raster: raster.clone(),
            enabled: true,
            linked: true,
            placement: None,
        });
        let mut second = first.clone();
        second
            .layers
            .push(Layer::raster("Shared", (*raster).clone()));
        let usage = workspace_usage([&first, &second]);
        assert_eq!(usage.documents, 2);
        assert_eq!(usage.unique_allocations, 2);
        assert_eq!(usage.raster_bytes, expected as u64);
        assert_eq!(usage.current_bytes, expected as u64);
    }

    #[test]
    fn sparse_large_canvas_counts_only_touched_tiles_and_uniform_storage() {
        let mut first = Document::new(30_000, 6_000);
        let mut raster = Raster::blank(first.width, first.height);
        raster.set(29_999, 5_999, [255; 4]);
        first.layers[0].image = Some(Arc::new(raster));
        let mut second = Document::new(30_000, 6_000);
        second.layers[0].image = Some(Arc::new(Raster::solid(30_000, 6_000, [255; 4])));
        let usage = workspace_usage([&first, &second]);
        assert_eq!(usage.current_bytes, (TILE_SIZE * TILE_SIZE * 4 + 4) as u64);
        assert_eq!(usage.unique_allocations, 2);
        assert!(check_workspace_budget([&first, &second], 0, MIB).is_ok());
    }

    #[test]
    fn distinct_document_storage_can_exceed_workspace_limit() {
        let mut first = Document::new(32, 32);
        first.layers[0].image = Some(Arc::new(Raster::from_rgba(32, 32, vec![0; 4096])));
        let mut second = Document::new(32, 32);
        second.layers[0].image = Some(Arc::new(Raster::from_rgba(32, 32, vec![1; 4096])));
        assert!(check_workspace_budget([&first], 0, 6_000).is_ok());
        assert!(check_workspace_budget([&second], 0, 6_000).is_ok());
        let error = check_workspace_budget([&first, &second], 0, 6_000).unwrap_err();
        assert!(error.to_string().contains("共 8192 字节"));
        let shared = first.clone();
        assert!(check_workspace_budget([&first, &shared], 0, 6_000).is_ok());
    }

    #[test]
    fn history_is_included_at_the_budget_boundary_without_overflow() {
        let mut document = Document::new(32, 32);
        document.layers[0].image = Some(Arc::new(Raster::from_rgba(32, 32, vec![0; 4096])));
        assert!(check_workspace_budget([&document], 1904, 6000).is_ok());
        assert!(check_workspace_budget([&document], 1905, 6000).is_err());
        assert!(check_workspace_budget([&document], u64::MAX, 6000).is_err());
    }

    #[test]
    fn retained_vec_capacity_is_counted_even_when_payload_is_small() {
        let mut bytes = Vec::with_capacity(8192);
        bytes.extend_from_slice(&[0, 0, 0, 255]);
        let capacity = bytes.capacity();
        let mut document = Document::new(32, 32);
        document.layers[0].image = Some(Arc::new(Raster {
            width: 32,
            height: 32,
            pixels: Arc::new(bytes),
            tiles: Default::default(),
        }));
        assert_eq!(workspace_usage([&document]).current_bytes, capacity as u64);
        assert!(check_workspace_budget([&document], 0, 4096).is_err());
    }

    #[test]
    fn selection_regions_are_counted_once_including_off_canvas_storage() {
        let first = Arc::new(vec![255; 4]);
        let second = Arc::new(vec![128; 9]);
        let mut document = Document::new(30_000, 6_000);
        document.selection = Some(Selection::from_regions(
            document.width,
            document.height,
            vec![
                SelectionRegion {
                    origin: [0, 0],
                    size: [2, 2],
                    coverage: first.clone(),
                },
                SelectionRegion {
                    origin: [100_000, -100_000],
                    size: [3, 3],
                    coverage: second,
                },
                SelectionRegion {
                    origin: [1_000_000, 1_000_000],
                    size: [2, 2],
                    coverage: first,
                },
            ],
        ));
        let other = document.clone();
        let usage = workspace_usage([&document, &other]);
        assert_eq!(usage.unique_allocations, 2);
        assert_eq!(usage.selection_bytes, 13);
        assert_eq!(usage.current_bytes, 13);
    }

    #[test]
    fn feathered_selection_counts_the_retained_outline_once_across_tabs() {
        let hard = Selection::from_coverage(3, 3, vec![255; 9]);
        let soft = crate::selection::feather(&hard, 2.);
        let soft_bytes = soft
            .regions()
            .map(|(_, _, coverage)| coverage.capacity())
            .sum::<usize>();
        assert!(soft_bytes > hard.coverage.capacity());
        let expected = soft_bytes + hard.coverage.capacity();
        let mut first = Document::new(3, 3);
        first.selection = Some(soft);
        let mut second = first.clone();
        second.selection = Some(hard);
        let third = first.clone();
        let usage = workspace_usage([&first, &second, &third]);
        assert_eq!(usage.unique_allocations, 2);
        assert_eq!(usage.selection_bytes, expected as u64);
        assert_eq!(usage.current_bytes, expected as u64);
        assert!(check_workspace_budget([&first, &second, &third], 0, expected as u64).is_ok());
        assert!(check_workspace_budget([&first], 0, expected as u64 - 1).is_err());
    }

    #[test]
    fn memory_policy_leaves_space_for_os_and_transient_buffers() {
        assert_eq!(budget_for_memory(Some(3 * GIB)), GIB);
        assert_eq!(budget_for_memory(Some(12 * GIB)), 4 * GIB);
        assert_eq!(budget_for_memory(Some(64 * GIB)), 6 * GIB);
        assert_eq!(budget_for_memory(Some(512 * MIB)), 256 * MIB);
        assert_eq!(budget_for_memory(None), 512 * MIB);
        assert!((256 * MIB..=6 * GIB).contains(&workspace_budget_bytes()));
    }
}
