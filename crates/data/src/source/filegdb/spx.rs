//! The spatial index ArcGIS keeps beside a feature class: a `.spx` B-tree of grid cells and the
//! features in them, and the `.gdbindexes` file naming a table's indexes.
//!
//! Each feature is indexed under every cell of a square grid its bounding box meets (each point
//! of a multipoint on its own), keyed `(level << 62) | (column << 31) | row` with the grid's
//! cells counted from 2^29 cells left of and below the origin. The B-tree's pages are 4096 bytes;
//! a leaf holds up to 340 keys with their object IDs, and a page above holds the last key under
//! each page below it.

use rustgis_geom::Extent;

const PAGE: usize = 4096;
/// Keys a page holds: (4096 - 12) / (4 + 8).
const PER_PAGE: usize = 340;
/// Where a page's keys start, after its header and its object or page IDs.
const KEYS_AT: usize = 12 + PER_PAGE * 4;
/// Grid cells are counted from this many cells left of and below the origin.
const SHIFT: f64 = (1u64 << 29) as f64;
/// Most cells one feature is indexed under (its box is gridded no finer).
const MAX_CELLS: u64 = 1 << 16;

/// The cell size for a feature class: for points, the side of a square holding one point on
/// average over the extent; for lines and polygons, the largest feature's width or height, so a
/// feature meets at most four cells.
pub(super) fn grid_size(points: bool, extent: &Extent, boxes: &[Extent], point_count: usize) -> f64 {
    let size = if points {
        let (w, h) = (extent.width(), extent.height());
        let n = point_count.max(1) as f64;
        if w * h > 0.0 { (w * h / n).sqrt() } else { w.max(h) / n }
    } else {
        boxes.iter().map(|b| b.width().max(b.height())).fold(0.0, f64::max)
    };
    if size.is_finite() && size > 0.0 { size } else { 1.0 }
}

/// The keys of every cell `b` meets on a grid of `size`; none for a box the grid can't key.
pub(super) fn keys(b: &Extent, size: f64, out: &mut Vec<i64>) {
    let cell = |v: f64| -> Option<i64> {
        let c = (v / size + SHIFT).floor();
        (c.abs() < (1u64 << 30) as f64).then_some(c as i64)
    };
    let (Some(x0), Some(y0), Some(x1), Some(y1)) = (cell(b.min_x), cell(b.min_y), cell(b.max_x), cell(b.max_y)) else { return };
    if x1 < x0 || y1 < y0 || ((x1 - x0 + 1) as u64).saturating_mul((y1 - y0 + 1) as u64) > MAX_CELLS {
        return;
    }
    for x in x0..=x1 {
        for y in y0..=y1 {
            out.push((x << 31) | y);
        }
    }
}

/// The `.spx` for (key, object ID) pairs.
pub(super) fn spx(mut pairs: Vec<(i64, u32)>) -> Vec<u8> {
    pairs.sort_unstable();
    pairs.dedup();
    let n = pairs.len();
    let leaves = n.div_ceil(PER_PAGE).max(1);
    // Pages per level above the leaves, top level last: each page points at up to 341 below it,
    // sharing its last with the next page's first.
    let mut levels: Vec<usize> = Vec::new();
    if leaves > 1 {
        let mut below = leaves;
        while below > PER_PAGE + 1 {
            below = (below - 1).div_ceil(PER_PAGE);
            levels.push(below);
        }
    }
    let depth = if leaves == 1 { 1 } else { levels.len() + 2 };
    let mut out = Vec::with_capacity((1 + levels.iter().sum::<usize>() + leaves) * PAGE + 22);
    // The last key under the `i`-th page of a level whose pages each cover `span` keys.
    let last_key = |i: usize, span: usize| pairs.get(((i + 1) * span).min(n).saturating_sub(1)).map_or(0, |p| p.0);
    if leaves == 1 {
        leaf(&mut out, 0, &pairs);
    } else {
        // The page IDs (from 1: the root) where each level starts, top down, then the leaves.
        let mut first = vec![2usize];
        for count in levels.iter().rev() {
            first.push(first.last().copied().unwrap_or(2) + count);
        }
        let top_pages = levels.last().copied().unwrap_or(leaves);
        // Keys under each page of the top level.
        let span_of = |level_from_leaves: usize| PER_PAGE.pow(level_from_leaves as u32 + 1);
        let top_span = span_of(levels.len());
        let mut page = header(0, if top_pages == 1 { 1 } else { top_pages - 1 });
        for i in 0..top_pages {
            page.extend(((first[0] + i) as u32).to_le_bytes());
        }
        page.resize(KEYS_AT, 0);
        for i in 0..top_pages.saturating_sub(1).max(1) {
            page.extend(last_key(i, top_span).to_le_bytes());
        }
        page.resize(PAGE, 0);
        out.extend(page);
        // The levels between the root and the leaves, top down.
        for (k, count) in levels.iter().rev().enumerate() {
            let below = if k + 1 < levels.len() { levels[levels.len() - 2 - k] } else { leaves };
            let below_first = first[k + 1];
            let span = span_of(levels.len() - 1 - k);
            for i in 0..*count {
                let last = i + 1 == *count;
                let items = if last { below - i * PER_PAGE - 1 } else { PER_PAGE };
                let next = if last { 0 } else { first[k] + i + 1 };
                let mut page = header(next as u32, items);
                for j in 0..=items {
                    page.extend(((below_first + i * PER_PAGE + j) as u32).to_le_bytes());
                }
                page.resize(KEYS_AT, 0);
                for j in 0..items {
                    page.extend(last_key(i * PER_PAGE + j, span).to_le_bytes());
                }
                page.resize(PAGE, 0);
                out.extend(page);
            }
        }
        let leaf_first = first.last().copied().unwrap_or(2);
        for (i, chunk) in pairs.chunks(PER_PAGE).enumerate() {
            let next = if i + 1 < leaves { leaf_first + i + 1 } else { 0 };
            leaf(&mut out, next as u32, chunk);
        }
    }
    // The trailer: key size, a flag, then the depth and the number of keys.
    out.extend([8, 0x40]);
    for v in [1u32, depth as u32, n as u32, 0, 1] {
        out.extend(v.to_le_bytes());
    }
    out
}

fn header(next: u32, count: usize) -> Vec<u8> {
    let mut p = Vec::with_capacity(PAGE);
    p.extend(next.to_le_bytes());
    p.extend((count as u32).to_le_bytes());
    p
}

fn leaf(out: &mut Vec<u8>, next: u32, pairs: &[(i64, u32)]) {
    let mut page = header(next, pairs.len());
    page.extend(0u32.to_le_bytes());
    for (_, oid) in pairs {
        page.extend(oid.to_le_bytes());
    }
    page.resize(KEYS_AT, 0);
    for (key, _) in pairs {
        page.extend(key.to_le_bytes());
    }
    page.resize(PAGE, 0);
    out.extend(page);
}

/// The `.gdbindexes` naming a table's indexes: its object IDs, and its shapes when it has them.
pub(super) fn gdbindexes(shape: bool) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend((1 + u32::from(shape)).to_le_bytes());
    let mut entry = |name: &str, field: &str, kind: u32, flag: u16| {
        let text = |s: &str, b: &mut Vec<u8>| {
            b.extend((s.encode_utf16().count() as u32).to_le_bytes());
            b.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
        };
        text(name, &mut b);
        b.extend(0u16.to_le_bytes());
        b.extend(kind.to_le_bytes());
        b.extend(flag.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        text(field, &mut b);
        b.extend(0u16.to_le_bytes());
    };
    entry("FDO_OBJECTID", "OBJECTID", 16, 0xffff);
    if shape {
        entry("FDO_SHAPE", "SHAPE", 4, 0);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a `.spx` back: every (key, object ID) pair in leaf order, checking the pages above
    /// point at pages that hold the keys their separators say.
    fn read(b: &[u8]) -> (u32, Vec<(i64, u32)>) {
        let trailer = &b[b.len() - 22..];
        let depth = u32::from_le_bytes(trailer[6..10].try_into().unwrap());
        let n = u32::from_le_bytes(trailer[10..14].try_into().unwrap()) as usize;
        let page = |id: usize| &b[(id - 1) * PAGE..id * PAGE];
        let u32_at = |p: &[u8], at: usize| u32::from_le_bytes(p[at..at + 4].try_into().unwrap());
        let key_at = |p: &[u8], i: usize| i64::from_le_bytes(p[KEYS_AT + i * 8..KEYS_AT + i * 8 + 8].try_into().unwrap());
        // Down the first child at each level to the first leaf, then along the leaves.
        let mut id = 1usize;
        for _ in 1..depth {
            id = u32_at(page(id), 8) as usize;
        }
        let mut out = Vec::new();
        loop {
            let p = page(id);
            let count = u32_at(p, 4) as usize;
            for i in 0..count {
                out.push((key_at(p, i), u32_at(p, 12 + i * 4)));
            }
            match u32_at(p, 0) {
                0 => break,
                next => id = next as usize,
            }
        }
        assert_eq!(out.len(), n);
        // A search from the root, as a reader runs one: at each page above the leaves, the first
        // child whose separator isn't below the key (or the last child); the leaf reached, or the
        // one after it for a key repeated across leaves, holds it.
        let find = |k: i64| -> bool {
            let mut id = 1usize;
            for _ in 1..depth {
                let p = page(id);
                let items = u32_at(p, 4) as usize;
                let i = (0..items).find(|i| key_at(p, *i) >= k).unwrap_or(items);
                id = u32_at(p, 8 + i * 4) as usize;
            }
            let leaf = page(id);
            let has = |p: &[u8]| (0..u32_at(p, 4) as usize).any(|i| key_at(p, i) == k);
            has(leaf) || (u32_at(leaf, 0) != 0 && has(page(u32_at(leaf, 0) as usize)))
        };
        for (k, _) in out.iter().step_by((n / 500).max(1)) {
            assert!(find(*k), "key {k} not found by a search");
        }
        (depth, out)
    }

    #[test]
    fn trees_of_every_depth_read_back_in_order() {
        for n in [0usize, 1, 2, 340, 341, 1000, 340 * 341, 340 * 341 + 1, 200_000] {
            let pairs: Vec<(i64, u32)> = (0..n).map(|i| (((i * 7919) % (n + 13)) as i64, i as u32 + 1)).collect();
            let b = spx(pairs.clone());
            let (depth, back) = read(&b);
            let mut want = pairs;
            want.sort_unstable();
            assert_eq!(back, want, "{n} keys");
            let expected = if n <= 340 {
                1
            } else if n <= 341 * 340 {
                2
            } else {
                3
            };
            assert_eq!(depth, expected, "{n} keys");
            assert_eq!((b.len() - 22) % PAGE, 0);
        }
    }

    #[test]
    fn keys_cover_every_cell_a_box_meets() {
        let mut k = Vec::new();
        keys(&Extent::new(0.5, 0.5, 0.5, 0.5), 1.0, &mut k);
        assert_eq!(k, [((1i64 << 29) << 31) | (1 << 29)]);
        k.clear();
        keys(&Extent::new(-0.5, -0.5, 1.5, 0.5), 1.0, &mut k);
        assert_eq!(k.len(), 3 * 2);
        k.clear();
        // Past what the grid keys, or too many cells: nothing (the feature is still found by a
        // full scan).
        keys(&Extent::new(0.0, 0.0, 1e300, 1.0), 1.0, &mut k);
        keys(&Extent::new(0.0, 0.0, 1e6, 1e6), 1.0, &mut k);
        keys(&Extent::new(f64::NAN, 0.0, 1.0, 1.0), 1.0, &mut k);
        assert!(k.is_empty());
    }

    #[test]
    fn grid_sizes() {
        let e = Extent::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(grid_size(true, &e, &[], 100), 1.0);
        assert_eq!(grid_size(false, &e, &[Extent::new(0.0, 0.0, 2.0, 1.0), Extent::new(5.0, 5.0, 5.5, 8.0)], 2), 3.0);
        assert_eq!(grid_size(true, &Extent::new(1.0, 1.0, 1.0, 1.0), &[], 1), 1.0);
    }

    #[test]
    fn gdbindexes_as_arcgis_writes_them() {
        let b = gdbindexes(true);
        assert_eq!(&b[..8], &[2, 0, 0, 0, 12, 0, 0, 0]);
        assert_eq!(b.len(), 116);
        assert_eq!(gdbindexes(false).len(), 66);
    }
}
