//! Terrain material layers: which layer covers which patch, and the stored
//! 66 x 66 masks (worked out from the Verdun data; see STATUS.md, update 9).
//!
//! - The tree file, after the root heights and the root's block A, has one
//!   byte, then a 30-byte record per height node (bounding box + flags), then
//!   the mask section: u8 1, u32 section size, u32 66 (mask size), u32 4
//!   (patches per node side), ..., the terrain rectangle at +29, u32 record
//!   count at +49. The first record starts at +68.
//! - A record (30 bytes): u8 layer, rect (x0, z0, x1, z1 as f32), u8 ?,
//!   u8 full, 11 bytes (an 85-bit quadtree we do not use). After some records
//!   come extra bytes (usually 9).
//! - A record is one layer in one patch. A node has 4 x 4 patches, so the rect
//!   size gives the node level (the root's patches are a quarter of the map).
//!   `full` = 1: the layer covers the patch, no mask. `full` = 0: a 66 x 66
//!   mask is stored in the node's chunk, in record order. Mask rows run along
//!   z, columns along x; column 64 equals column 0 of the next patch in x.

use anyhow::{ensure, Context, Result};
use std::collections::HashMap;

/// Mask width and height (64 cells + shared edge + 1).
pub const MASK_SIZE: usize = 66;
pub const MASK_BYTES: usize = MASK_SIZE * MASK_SIZE;
/// Patches per node side.
pub const PATCHES: u32 = 4;
/// Layer numbers are below this.
pub const MAX_LAYERS: usize = 32;

/// One record of the mask section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskRecord {
    pub layer: u8,
    /// x0, z0, x1, z1 in world units.
    pub rect: [f32; 4],
    /// The layer covers the whole patch (no stored mask).
    pub full: bool,
    /// The byte before `full` (meaning not known).
    pub aux: u8,
    /// The 11 bytes after `full`: probably an 85-bit quadtree (this patch,
    /// then 4, 16 and 64 cells below it). Kept for study (DESIGN.md "layers
    /// carried from coarser levels").
    pub tree: [u8; 11],
}

/// A layer of a patch, in paint order.
#[derive(Debug, Clone, Copy)]
pub struct PatchLayer {
    pub layer: u8,
    /// Index of the stored mask in `LayerMasks::masks` (None: covers the patch).
    pub mask: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct LayerMasks {
    /// World x and z of the terrain's corner, and its width.
    pub min: [f32; 2],
    pub width: f32,
    /// Layers of each patch, by (level, patch x, patch z) in the level's patch grid
    /// (4 x 2^level patches per side).
    pub patches: HashMap<(u32, u32, u32), Vec<PatchLayer>>,
    /// All stored masks, MASK_BYTES each.
    pub masks: Vec<u8>,
    /// Finest level that has records.
    pub finest: u32,
    /// All records (also those whose masks could not be used), for study.
    pub records: Vec<MaskRecord>,
}

#[derive(Debug, Clone, Default)]
pub struct MaskStats {
    pub header_count: usize,
    pub records: usize,
    /// Records with a stored mask.
    pub stored: usize,
    /// Nodes whose stored-record count equals the masks in their chunk.
    pub nodes_matched: usize,
    /// Nodes where it differs (their masks are not used), with details for the log.
    pub nodes_mismatched: usize,
    pub mismatches: Vec<String>,
    /// Nodes with records but no chunk.
    pub nodes_missing: usize,
    /// Gaps between records other than 0 or 9 extra bytes.
    pub odd_gaps: usize,
    /// Edge check (same layer, neighbouring patches, edge not constant).
    pub edge_x_equal: usize,
    pub edge_x_different: usize,
    pub edge_z_equal: usize,
    pub edge_z_different: usize,
    /// Patches whose layers are not in ascending order (0 means the stored
    /// order is the layer order).
    pub patches_not_ascending: usize,
}

fn f32_at(d: &[u8], at: usize) -> Option<f32> {
    Some(f32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// A square rect aligned to its own size, inside the terrain, with a size
/// that is the terrain width / (4 x 2^level).
fn rect_at(d: &[u8], at: usize, min: [f32; 2], width: f32) -> Option<[f32; 4]> {
    let r = [f32_at(d, at)?, f32_at(d, at + 4)?, f32_at(d, at + 8)?, f32_at(d, at + 12)?];
    let s = r[2] - r[0];
    if !r.iter().all(|v| v.is_finite()) || s <= 0.0 || (r[3] - r[1] - s).abs() > 1e-3 {
        return None;
    }
    let (fx, fz) = ((r[0] - min[0]) / s, (r[1] - min[1]) / s);
    if fx < 0.0 || fz < 0.0 || fx.fract() != 0.0 || fz.fract() != 0.0 || r[2] > min[0] + width + 1e-3 || r[3] > min[1] + width + 1e-3 {
        return None;
    }
    let per_side = width / s;
    let level = (per_side / PATCHES as f32).log2();
    (level >= 0.0 && level.fract() == 0.0 && level < 16.0).then_some(r)
}

/// Find and parse the mask section. `after_block_a` is the offset right after
/// the root's block A; `min` and `width` describe the terrain square.
pub fn parse_mask_section(tree: &[u8], after_block_a: usize, min: [f32; 2], width: f32) -> Result<(Vec<MaskRecord>, MaskStats)> {
    // Skip one byte, then the node records (24-byte box + 6 bytes) while they look like boxes.
    let mut at = after_block_a + 1;
    loop {
        let b: Option<Vec<f32>> = (0..6).map(|k| f32_at(tree, at + 4 * k)).collect();
        let Some(b) = b else { break };
        let w = b[3] - b[0];
        if !(w > 0.0 && (b[5] - b[2] - w).abs() < 0.01 && b[0] >= min[0] - 1.0 && b[3] <= min[0] + width + 1.0) {
            break;
        }
        at += 30;
    }
    let s = at;
    ensure!(tree.get(s) == Some(&1), "mask section: expected 1 at {s}");
    let size = u32_at(tree, s + 1).context("mask section size")? as usize;
    ensure!(u32_at(tree, s + 5) == Some(MASK_SIZE as u32), "mask section: mask size is not {MASK_SIZE}");
    ensure!(u32_at(tree, s + 9) == Some(PATCHES), "mask section: patches per side is not {PATCHES}");
    let end = s + 1 + size;
    ensure!(end <= tree.len(), "mask section past the end of the file");
    let mut stats = MaskStats { header_count: u32_at(tree, s + 49).context("mask record count")? as usize, ..Default::default() };

    let mut out = Vec::new();
    let mut p = s + 68;
    // The last record may end a little after the stated section size (2 bytes on
    // Verdun), so only its start must be inside the section.
    while out.len() < stats.header_count && p < end && p + 19 <= tree.len() {
        let rect = rect_at(tree, p + 1, min, width).with_context(|| format!("mask record {} at {p}: no rect", out.len()))?;
        let mut bits = [0u8; 11];
        if let Some(b) = tree.get(p + 19..p + 30) {
            bits.copy_from_slice(b);
        }
        out.push(MaskRecord { layer: tree[p], rect, full: tree[p + 18] != 0, aux: tree[p + 17], tree: bits });
        let q = p + 30;
        if out.len() == stats.header_count || q >= end {
            break;
        }
        if rect_at(tree, q + 1, min, width).is_some() {
            p = q;
        } else if rect_at(tree, q + 10, min, width).is_some() {
            p = q + 9;
        } else {
            let k = (1..80)
                .find(|&k| tree.get(q + k).is_some_and(|&l| (l as usize) < MAX_LAYERS) && rect_at(tree, q + k + 1, min, width).is_some())
                .with_context(|| format!("mask records lost after {}", out.len()))?;
            stats.odd_gaps += 1;
            p = q + k;
        }
    }
    stats.records = out.len();
    stats.stored = out.iter().filter(|r| !r.full).count();
    ensure!(out.iter().all(|r| (r.layer as usize) < MAX_LAYERS), "layer number out of range");
    Ok((out, stats))
}

impl LayerMasks {
    /// Patch size at a level.
    pub fn patch_size(&self, level: u32) -> f32 {
        self.width / (PATCHES << level) as f32
    }

    /// Level and patch position of a record.
    pub fn patch_of(&self, r: &MaskRecord) -> (u32, u32, u32) {
        let s = r.rect[2] - r.rect[0];
        let per_side = (self.width / s).round() as u32;
        let level = (per_side / PATCHES).trailing_zeros();
        (level, ((r.rect[0] - self.min[0]) / s).round() as u32, ((r.rect[1] - self.min[1]) / s).round() as u32)
    }

    /// Build from the records and the masks of each node. `node_masks(level, ix, iz)`
    /// returns the node's mask bytes (everything after heights and block A, without
    /// the final layer), if the node has a chunk.
    pub fn build(records: &[MaskRecord], min: [f32; 2], width: f32, node_masks: impl Fn(u32, u32, u32) -> Option<Vec<u8>>, stats: &mut MaskStats) -> LayerMasks {
        let mut lm = LayerMasks { min, width, records: records.to_vec(), ..Default::default() };
        // Records of each node, in order.
        let mut by_node: Vec<((u32, u32, u32), Vec<(u32, u32, MaskRecord)>)> = Vec::new();
        let mut index: HashMap<(u32, u32, u32), usize> = HashMap::new();
        for r in records {
            let (level, px, pz) = lm.patch_of(r);
            let node = (level, px / PATCHES, pz / PATCHES);
            let i = *index.entry(node).or_insert_with(|| {
                by_node.push((node, Vec::new()));
                by_node.len() - 1
            });
            by_node[i].1.push((px, pz, *r));
        }
        for (node, recs) in by_node {
            let stored = recs.iter().filter(|r| !r.2.full).count();
            let data = if stored > 0 { node_masks(node.0, node.1, node.2) } else { Some(Vec::new()) };
            let usable = match &data {
                None => {
                    stats.nodes_missing += 1;
                    false
                }
                Some(d) if d.len() == stored * MASK_BYTES => {
                    if stored > 0 {
                        stats.nodes_matched += 1;
                    }
                    true
                }
                Some(d) => {
                    stats.nodes_mismatched += 1;
                    stats.mismatches.push(format!(
                        "node {node:?}: {stored} stored records, chunk has {} bytes = {:.2} masks",
                        d.len(),
                        d.len() as f64 / MASK_BYTES as f64
                    ));
                    false
                }
            };
            let mut next = 0usize;
            for (px, pz, r) in recs {
                let mask = if r.full {
                    None
                } else if usable {
                    let d = data.as_ref().unwrap();
                    lm.masks.extend_from_slice(&d[next * MASK_BYTES..(next + 1) * MASK_BYTES]);
                    next += 1;
                    Some((lm.masks.len() / MASK_BYTES - 1) as u32)
                } else {
                    continue;
                };
                lm.finest = lm.finest.max(node.0);
                lm.patches.entry((node.0, px, pz)).or_default().push(PatchLayer { layer: r.layer, mask });
            }
        }
        lm.edge_check(stats);
        stats.patches_not_ascending = lm.patches.values().filter(|l| l.windows(2).any(|w| w[0].layer >= w[1].layer)).count();
        lm
    }

    pub fn mask(&self, i: u32) -> &[u8] {
        &self.masks[i as usize * MASK_BYTES..(i as usize + 1) * MASK_BYTES]
    }

    /// Compare shared edges of the same layer in neighbouring patches (x: column
    /// 64 against column 0; z: row 64 against row 0). Only edges that are not
    /// constant count.
    fn edge_check(&self, stats: &mut MaskStats) {
        for (&(level, px, pz), layers) in &self.patches {
            for pl in layers {
                let Some(a) = pl.mask else { continue };
                let a = self.mask(a);
                for (dx, dz) in [(1u32, 0u32), (0, 1)] {
                    let Some(nb) = self.patches.get(&(level, px + dx, pz + dz)) else { continue };
                    let Some(b) = nb.iter().find(|n| n.layer == pl.layer).and_then(|n| n.mask) else { continue };
                    let b = self.mask(b);
                    let (ea, eb): (Vec<u8>, Vec<u8>) = if dx == 1 {
                        ((0..65).map(|y| a[y * MASK_SIZE + 64]).collect(), (0..65).map(|y| b[y * MASK_SIZE]).collect())
                    } else {
                        ((0..65).map(|x| a[64 * MASK_SIZE + x]).collect(), (0..65).map(|x| b[x]).collect())
                    };
                    if ea.iter().all(|&v| v == ea[0]) && ea == eb {
                        continue;
                    }
                    match (dx == 1, ea == eb) {
                        (true, true) => stats.edge_x_equal += 1,
                        (true, false) => stats.edge_x_different += 1,
                        (false, true) => stats.edge_z_equal += 1,
                        (false, false) => stats.edge_z_different += 1,
                    }
                }
            }
        }
    }

    /// Layer weights at world (x, z) from the finest patch that has records:
    /// start empty, then paint each layer on top in stored order
    /// (`w = w * (1 - m)`, then `w[layer] += m`; a full layer has m = 1).
    /// Layers with `skip[layer]` set are not painted. Returns the level used,
    /// or None if no patch covers the point.
    pub fn weights_at(&self, x: f32, z: f32, skip: &[bool; MAX_LAYERS], out: &mut [f32; MAX_LAYERS]) -> Option<u32> {
        *out = [0.0; MAX_LAYERS];
        let (fx, fz) = ((x - self.min[0]) / self.width, (z - self.min[1]) / self.width);
        if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fz) {
            return None;
        }
        for level in (0..=self.finest).rev() {
            let n = (PATCHES << level) as f32;
            let (gx, gz) = ((fx * n).min(n - 1e-4), (fz * n).min(n - 1e-4));
            let Some(layers) = self.patches.get(&(level, gx as u32, gz as u32)) else { continue };
            // Mask coordinates 0..64 inside the patch.
            let (u, v) = (gx.fract() * 64.0, gz.fract() * 64.0);
            for pl in layers {
                if skip[pl.layer as usize] {
                    continue;
                }
                let m = match pl.mask {
                    None => 1.0,
                    Some(i) => sample(self.mask(i), u, v) / 255.0,
                };
                for w in out.iter_mut() {
                    *w *= 1.0 - m;
                }
                out[pl.layer as usize] += m;
            }
            return Some(level);
        }
        None
    }
}

impl LayerMasks {
    /// One layer's own mask value at (x, z), not mixed with the layers painted
    /// after it (0 if the patch does not have the layer); None if no patch
    /// covers the point. For scatter-only layers (DESIGN.md "trees on the far
    /// hills").
    /// One layer's own mask at (x, z) in the patch of one quadtree level
    /// (None if that level has no patch there).
    pub fn raw_weight_at_level(&self, x: f32, z: f32, layer: u8, level: u32) -> Option<f32> {
        let (fx, fz) = ((x - self.min[0]) / self.width, (z - self.min[1]) / self.width);
        if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fz) {
            return None;
        }
        let n = (PATCHES << level) as f32;
        let (gx, gz) = ((fx * n).min(n - 1e-4), (fz * n).min(n - 1e-4));
        let layers = self.patches.get(&(level, gx as u32, gz as u32))?;
        let (u, v) = (gx.fract() * 64.0, gz.fract() * 64.0);
        Some(match layers.iter().find(|pl| pl.layer == layer) {
            Some(pl) => match pl.mask {
                None => 1.0,
                Some(i) => sample(self.mask(i), u, v) / 255.0,
            },
            None => 0.0,
        })
    }

    /// One layer's own mask at (x, z) from the finest patch that lists the
    /// layer (0 if none does; None outside the terrain). For scatter-only
    /// layers, which a finer patch may not list even where a coarser one has
    /// them (DESIGN.md "layers carried from coarser levels", attempt 2).
    pub fn own_weight_at(&self, x: f32, z: f32, layer: u8) -> Option<f32> {
        let (fx, fz) = ((x - self.min[0]) / self.width, (z - self.min[1]) / self.width);
        if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fz) {
            return None;
        }
        for level in (0..=self.finest).rev() {
            if let Some(m) = self.raw_weight_at_level(x, z, layer, level).filter(|_| self.lists(x, z, layer, level)) {
                return Some(m);
            }
        }
        Some(0.0)
    }

    /// Whether the patch of `level` at (x, z) has a record of `layer`.
    fn lists(&self, x: f32, z: f32, layer: u8, level: u32) -> bool {
        let (fx, fz) = ((x - self.min[0]) / self.width, (z - self.min[1]) / self.width);
        let n = (PATCHES << level) as f32;
        let (gx, gz) = ((fx * n).min(n - 1e-4), (fz * n).min(n - 1e-4));
        self.patches.get(&(level, gx as u32, gz as u32)).is_some_and(|v| v.iter().any(|pl| pl.layer == layer))
    }

    pub fn raw_weight_at(&self, x: f32, z: f32, layer: u8) -> Option<f32> {
        let (fx, fz) = ((x - self.min[0]) / self.width, (z - self.min[1]) / self.width);
        if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fz) {
            return None;
        }
        for level in (0..=self.finest).rev() {
            let n = (PATCHES << level) as f32;
            let (gx, gz) = ((fx * n).min(n - 1e-4), (fz * n).min(n - 1e-4));
            let Some(layers) = self.patches.get(&(level, gx as u32, gz as u32)) else { continue };
            let (u, v) = (gx.fract() * 64.0, gz.fract() * 64.0);
            return Some(match layers.iter().find(|pl| pl.layer == layer) {
                Some(pl) => match pl.mask {
                    None => 1.0,
                    Some(i) => sample(self.mask(i), u, v) / 255.0,
                },
                None => 0.0,
            });
        }
        None
    }
}

/// Bilinear sample of a 66 x 66 mask at (u, v) in 0..=64.
fn sample(m: &[u8], u: f32, v: f32) -> f32 {
    let (x0, y0) = (u.floor() as usize, v.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(MASK_SIZE - 1), (y0 + 1).min(MASK_SIZE - 1));
    let (fx, fy) = (u - x0 as f32, v - y0 as f32);
    let at = |x: usize, y: usize| m[y * MASK_SIZE + x] as f32;
    let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
    let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny tree: block A end at 0, one node record, the section with two records.
    fn tiny_tree() -> Vec<u8> {
        let mut t = vec![0u8; 1];
        // One node record: box (-8..8, 0..1, -8..8) + 6 bytes.
        for v in [-8.0f32, 0.0, -8.0, 8.0, 1.0, 8.0] {
            t.extend_from_slice(&v.to_le_bytes());
        }
        t.extend_from_slice(&[0; 6]);
        let s = t.len();
        let mut h = vec![0u8; 68];
        h[0] = 1;
        h[5..9].copy_from_slice(&(MASK_SIZE as u32).to_le_bytes());
        h[9..13].copy_from_slice(&PATCHES.to_le_bytes());
        h[49..53].copy_from_slice(&2u32.to_le_bytes());
        t.extend_from_slice(&h);
        // Two records in the root's patch (0, 0) (patch size 16 / 4 = 4): layer 0 full, layer 3 masked.
        for (layer, full) in [(0u8, 1u8), (3, 0)] {
            t.push(layer);
            for v in [-8.0f32, -8.0, -4.0, -4.0] {
                t.extend_from_slice(&v.to_le_bytes());
            }
            t.push(0);
            t.push(full);
            t.extend_from_slice(&[0xff; 11]);
        }
        let size = (t.len() - s - 1) as u32;
        t[s + 1..s + 5].copy_from_slice(&size.to_le_bytes());
        t
    }

    #[test]
    fn own_weight_comes_from_the_finest_patch_that_lists_the_layer() {
        // Level 0 patch (0, 0): layer 2 full, layer 16 at 128/255. Level 1
        // patch (0, 0): only layer 4 at 64/255. The point lies in both.
        let mut lm = LayerMasks { min: [0.0, 0.0], width: 16.0, finest: 1, ..Default::default() };
        lm.masks.extend(std::iter::repeat_n(128u8, MASK_BYTES));
        lm.masks.extend(std::iter::repeat_n(64u8, MASK_BYTES));
        lm.patches.insert((0, 0, 0), vec![PatchLayer { layer: 2, mask: None }, PatchLayer { layer: 16, mask: Some(0) }]);
        lm.patches.insert((1, 0, 0), vec![PatchLayer { layer: 4, mask: Some(1) }]);
        let close = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-5);
        assert!(close(lm.own_weight_at(0.5, 0.5, 16), 128.0 / 255.0));
        assert!(close(lm.own_weight_at(0.5, 0.5, 4), 64.0 / 255.0));
        assert!(close(lm.own_weight_at(0.5, 0.5, 2), 1.0));
        assert!(close(lm.own_weight_at(0.5, 0.5, 7), 0.0));
        assert_eq!(lm.own_weight_at(-1.0, 0.5, 16), None);
        // The blend still uses the finest patch only.
        let mut w = [0f32; MAX_LAYERS];
        lm.weights_at(0.5, 0.5, &[false; MAX_LAYERS], &mut w);
        assert!((w[4] - 64.0 / 255.0).abs() < 1e-5 && w[2] == 0.0 && w[16] == 0.0, "{w:?}");
    }

    #[test]
    fn parses_records() {
        let t = tiny_tree();
        let (recs, st) = parse_mask_section(&t, 0, [-8.0, -8.0], 16.0).unwrap();
        assert_eq!(st.header_count, 2);
        assert_eq!(recs.len(), 2);
        assert_eq!((recs[0].layer, recs[0].full), (0, true));
        assert_eq!(recs[0].tree, [0xff; 11]);
        assert_eq!((recs[1].layer, recs[1].full), (3, false));
    }

    #[test]
    fn paints_layers_in_order() {
        let t = tiny_tree();
        let (recs, mut st) = parse_mask_section(&t, 0, [-8.0, -8.0], 16.0).unwrap();
        // Mask of layer 3: 0 on the left half (columns < 32), 255 on the right.
        let mask: Vec<u8> = (0..MASK_BYTES).map(|i| if i % MASK_SIZE < 32 { 0 } else { 255 }).collect();
        let lm = LayerMasks::build(&recs, [-8.0, -8.0], 16.0, |_, _, _| Some(mask.clone()), &mut st);
        assert_eq!(st.nodes_matched, 1);
        let mut w = [0.0; MAX_LAYERS];
        let skip = [false; MAX_LAYERS];
        // Left part of the patch: only layer 0.
        assert_eq!(lm.weights_at(-7.9, -6.0, &skip, &mut w), Some(0));
        assert!((w[0] - 1.0).abs() < 1e-5 && w[3] < 1e-5);
        // Right part: layer 3 covers layer 0.
        lm.weights_at(-4.2, -6.0, &skip, &mut w);
        assert!(w[0] < 1e-5 && (w[3] - 1.0).abs() < 1e-5);
        // Skipping layer 3 leaves layer 0.
        let mut skip3 = [false; MAX_LAYERS];
        skip3[3] = true;
        lm.weights_at(-4.2, -6.0, &skip3, &mut w);
        assert!((w[0] - 1.0).abs() < 1e-5);
    }
}
