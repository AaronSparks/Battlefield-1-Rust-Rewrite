//! Battlefield 1 terrain heights from a TerrainStreamingTree res
//! (res type 0x22fe8ac8) and its chunks.
//!
//! What we know (worked out from the Verdun data, 2026-10-03; see MODLOG.md):
//! - The tree file starts with a packed header (no alignment). It holds the
//!   height range 512.0 (f32 at 0x2f) and the terrain box: min x at 0x6c,
//!   min y 0x70, min z 0x74, max x 0x78, max y 0x7c, max z 0x80 (f32 each).
//!   Verdun: x and z from -2048 to 2048.
//! - At 0x89 is the root height tile: 261 x 261 u16 samples, row-major with
//!   rows along z. A tile is 256 cells plus a border of 2 samples on each side.
//!   Height in metres = sample * 512 / 65536.
//! - The file ends with a table of 23-byte records (3 flag bytes, u32 size,
//!   16-byte chunk id). Chunks that start with a smooth 261 x 261 u16 grid are
//!   detailed height tiles.
//! - We do not decode the tree's node positions yet. Instead each detailed
//!   tile is located by matching its samples against the root tile at every
//!   level and position: on Verdun every tile matches exactly at one place.

use anyhow::{ensure, Context, Result};

pub mod layers;
pub mod scatter;

pub const TILE: usize = 261;
const BORDER: usize = 2;
const CELLS: usize = 256;
const ROOT_OFFSET: usize = 0x89;
const RECORD: usize = 23;
/// A height sample of 0 marks a hole in the terrain (seen at the Verdun bunker entrances).
pub const HOLE: u16 = 0;
/// Size of a colour layer: 260 x 260 pixels in BC3 (65 x 65 blocks of 16 bytes).
pub const COLOUR_BYTES: usize = 65 * 65 * 16;
pub const COLOUR_SIZE: usize = 260;
/// Block A after the heights in a height node (8,847 bytes, probably a min/max table).
pub const BLOCK_A_BYTES: usize = 8847;
/// Where the colour starts in a height node chunk.
pub const COLOUR_IN_HEIGHT_NODE: usize = TILE * TILE * 2 + BLOCK_A_BYTES;

/// A colour tile of one tree node.
#[derive(Debug, Clone)]
pub struct ColourTile {
    /// Index of the chunk record it came from.
    pub record: usize,
    pub level: u32,
    pub ix: u32,
    pub iz: u32,
    /// BC3 blocks, 260 x 260 pixels.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Tile {
    /// Index of the chunk record this tile came from (None for the root).
    pub record: Option<usize>,
    /// 0 = root (whole terrain), level L covers 1 / 2^L of the width.
    pub level: u32,
    pub ix: u32,
    pub iz: u32,
    pub samples: Vec<u16>,
}

/// One record of the chunk table: 3 flag bytes, the chunk size, the chunk id.
#[derive(Debug, Clone, Copy)]
pub struct Record {
    pub flags: [u8; 3],
    pub size: u32,
    pub chunk_id: [u8; 16],
}

#[derive(Debug, Clone)]
pub struct Terrain {
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Metres per sample step.
    pub height_scale: f32,
    /// Sorted from finest (highest level) to coarsest.
    pub tiles: Vec<Tile>,
    /// The chunk table at the end of the tree file, in file order.
    pub records: Vec<Record>,
    /// Colour tiles (260 x 260 BC3, 256 cells + 2 border), finest first.
    pub colours: Vec<ColourTile>,
    /// Material layer masks (None if the mask section could not be read).
    pub layers: Option<layers::LayerMasks>,
}

#[derive(Debug, Default, Clone)]
pub struct LoadStats {
    pub records: usize,
    pub height_tiles: usize,
    pub not_located: usize,
    pub unreadable: usize,
    /// Height tiles whose position from the tree order equals the matched position.
    pub tree_position_agrees: usize,
    /// Height tiles where the two disagree (should be 0).
    pub tree_position_disagrees: usize,
    /// Height nodes whose first layer is not a colour image (they use the parent's colour).
    pub nodes_without_colour: usize,
    /// Material layer masks: counts and checks, or why they were not read.
    pub masks: Option<layers::MaskStats>,
    pub mask_error: Option<String>,
}

fn f32_at(d: &[u8], at: usize) -> Result<f32> {
    Ok(f32::from_le_bytes(d.get(at..at + 4).context("terrain header too short")?.try_into().unwrap()))
}

fn read_grid(d: &[u8], at: usize) -> Option<Vec<u16>> {
    let bytes = d.get(at..at + TILE * TILE * 2)?;
    Some(bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect())
}

/// True if a grid looks like heights: neighbouring samples differ little.
fn is_smooth(g: &[u16]) -> bool {
    let d: f64 = (0..TILE - 1).map(|k| (g[k] as f64 - g[k + 1] as f64).abs()).sum::<f64>() / (TILE - 1) as f64;
    d < 1500.0
}

impl Terrain {
    /// Parse the tree file. `known_chunk` says whether 16 bytes are a known
    /// chunk id; `read_chunk` returns a chunk's data.
    pub fn load(
        tree: &[u8],
        known_chunk: impl Fn(&[u8; 16]) -> bool,
        read_chunk: impl Fn(&[u8; 16]) -> Result<Vec<u8>>,
    ) -> Result<(Terrain, LoadStats)> {
        let height_range = f32_at(tree, 0x2f)?;
        ensure!(height_range > 0.0 && height_range < 100_000.0, "unexpected height range {height_range}");
        let min = [f32_at(tree, 0x6c)?, f32_at(tree, 0x70)?, f32_at(tree, 0x74)?];
        let max = [f32_at(tree, 0x78)?, f32_at(tree, 0x7c)?, f32_at(tree, 0x80)?];
        ensure!(max[0] > min[0] && max[2] > min[2], "bad terrain box {min:?} {max:?}");
        let root = read_grid(tree, ROOT_OFFSET).context("root tile past end of file")?;
        let mut terrain = Terrain { min, max, height_scale: height_range / 65536.0, tiles: vec![], records: vec![], colours: vec![], layers: None };
        let mut stats = LoadStats::default();

        let first = (0..tree.len().saturating_sub(16))
            .find(|&at| {
                let id: [u8; 16] = tree[at..at + 16].try_into().unwrap();
                id != [0; 16] && known_chunk(&id)
            })
            .context("no chunk table in terrain tree")?;
        let table = first.checked_sub(7).context("chunk table too early")?;
        stats.records = (tree.len() - table) / RECORD;
        // Per record: matched height position, and the colour layer bytes.
        let mut located: Vec<Option<(u32, u32, u32)>> = vec![None; stats.records];
        let mut colour_data: Vec<Option<Vec<u8>>> = vec![None; stats.records];
        // Per record: the stored layer masks (height nodes: after heights and block A;
        // the root's chunk: masks only), without the final 67,600-byte layer.
        let mut mask_data: Vec<Option<Vec<u8>>> = vec![None; stats.records];

        for i in 0..stats.records {
            let r = &tree[table + i * RECORD..table + (i + 1) * RECORD];
            let id: [u8; 16] = r[7..23].try_into().unwrap();
            terrain.records.push(Record { flags: [r[0], r[1], r[2]], size: u32::from_le_bytes(r[3..7].try_into().unwrap()), chunk_id: id });
            let Ok(chunk) = read_chunk(&id) else {
                stats.unreadable += 1;
                continue;
            };
            if chunk.len() == COLOUR_BYTES {
                colour_data[i] = Some(chunk.clone());
            } else if chunk.len() >= COLOUR_IN_HEIGHT_NODE + COLOUR_BYTES {
                // Height node layout: heights | block A | N masks (66 x 66) | final 67,600-byte
                // layer. The final layer is BC3 colour in some nodes and a BC5 normal map in
                // others; nodes without colour use the parent node's colour.
                let layer = &chunk[chunk.len() - COLOUR_BYTES..];
                if is_colour_layer(layer) {
                    colour_data[i] = Some(layer.to_vec());
                } else {
                    stats.nodes_without_colour += 1;
                }
            }
            let height_node = match read_grid(&chunk, 0).filter(|g| is_smooth(g)) {
                Some(grid) => {
                    stats.height_tiles += 1;
                    match locate(&root, &grid) {
                        Some((level, ix, iz)) => {
                            located[i] = Some((level, ix, iz));
                            terrain.tiles.push(Tile { record: Some(i), level, ix, iz, samples: grid });
                            true
                        }
                        None => {
                            stats.not_located += 1;
                            false
                        }
                    }
                }
                None => false,
            };
            // Layer masks: after heights and block A in a located height node. A chunk that
            // is not a height node but holds whole masks plus a final layer is the root's
            // (masks only; its first bytes can look like smooth heights, masks are smooth too).
            if height_node && chunk.len() >= COLOUR_IN_HEIGHT_NODE + COLOUR_BYTES {
                mask_data[i] = Some(chunk[COLOUR_IN_HEIGHT_NODE..chunk.len() - COLOUR_BYTES].to_vec());
            } else if !height_node && chunk.len() > COLOUR_BYTES && (chunk.len() - COLOUR_BYTES) % layers::MASK_BYTES == 0 {
                mask_data[i] = Some(chunk[..chunk.len() - COLOUR_BYTES].to_vec());
            }
        }
        // Rebuild node positions from the depth-first order and check them.
        let positions = tree_positions(&terrain.records, &located, &mut stats);
        for (i, data) in colour_data.into_iter().enumerate() {
            if let (Some(data), Some((level, ix, iz))) = (data, positions[i]) {
                terrain.colours.push(ColourTile { record: i, level, ix, iz, data });
            }
        }
        terrain.colours.sort_by(|a, b| b.level.cmp(&a.level));

        // Material layers: the mask section of the tree file and each node's masks.
        // The root's masks are in the one record that is not a located height node
        // but holds whole masks plus a final layer.
        let root_masks: Vec<usize> = (0..stats.records).filter(|&i| positions[i].is_none() && located[i].is_none() && mask_data[i].is_some()).collect();
        let width = max[0] - min[0];
        let after_block_a = ROOT_OFFSET + TILE * TILE * 2 + BLOCK_A_BYTES;
        match layers::parse_mask_section(tree, after_block_a, [min[0], min[2]], width) {
            Ok((records, mut mstats)) => {
                let by_pos: std::collections::HashMap<(u32, u32, u32), usize> =
                    positions.iter().enumerate().filter_map(|(i, p)| p.map(|p| (p, i))).collect();
                let lm = layers::LayerMasks::build(
                    &records,
                    [min[0], min[2]],
                    width,
                    |level, ix, iz| {
                        if level == 0 {
                            (root_masks.len() == 1).then(|| mask_data[root_masks[0]].clone()).flatten()
                        } else {
                            by_pos.get(&(level, ix, iz)).and_then(|&i| mask_data[i].clone())
                        }
                    },
                    &mut mstats,
                );
                terrain.layers = Some(lm);
                stats.masks = Some(mstats);
            }
            Err(e) => stats.mask_error = Some(format!("{e:#}")),
        }
        terrain.tiles.push(Tile { record: None, level: 0, ix: 0, iz: 0, samples: root });
        terrain.tiles.sort_by(|a, b| b.level.cmp(&a.level));
        Ok((terrain, stats))
    }

    /// Ground height at world (x, z), from the finest tile that covers it.
    /// None outside the terrain, or in a hole (a sample of 0 next to the point).
    pub fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let u = (x - self.min[0]) / (self.max[0] - self.min[0]);
        let v = (z - self.min[2]) / (self.max[2] - self.min[2]);
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return None;
        }
        for t in &self.tiles {
            let n = (1u32 << t.level) as f32;
            let (lu, lv) = (u * n - t.ix as f32, v * n - t.iz as f32);
            if !(0.0..=1.0).contains(&lu) || !(0.0..=1.0).contains(&lv) {
                continue;
            }
            let fc = lu * CELLS as f32 + BORDER as f32;
            let fr = lv * CELLS as f32 + BORDER as f32;
            let (c0, r0) = ((fc.floor() as usize).min(TILE - 2), (fr.floor() as usize).min(TILE - 2));
            let (tc, tr) = (fc - c0 as f32, fr - r0 as f32);
            if [t.samples[r0 * TILE + c0], t.samples[r0 * TILE + c0 + 1], t.samples[(r0 + 1) * TILE + c0], t.samples[(r0 + 1) * TILE + c0 + 1]].contains(&HOLE) {
                return None;
            }
            let s = |r: usize, c: usize| t.samples[r * TILE + c] as f32;
            let h = (s(r0, c0) * (1.0 - tc) + s(r0, c0 + 1) * tc) * (1.0 - tr)
                + (s(r0 + 1, c0) * (1.0 - tc) + s(r0 + 1, c0 + 1) * tc) * tr;
            return Some(h * self.height_scale);
        }
        None
    }

    /// Finest level covering world (x, z), or None outside the terrain.
    pub fn level_at(&self, x: f32, z: f32) -> Option<u32> {
        let u = (x - self.min[0]) / (self.max[0] - self.min[0]);
        let v = (z - self.min[2]) / (self.max[2] - self.min[2]);
        self.tiles
            .iter()
            .find(|t| {
                let n = (1u32 << t.level) as f32;
                let (lu, lv) = (u * n - t.ix as f32, v * n - t.iz as f32);
                (0.0..=1.0).contains(&lu) && (0.0..=1.0).contains(&lv)
            })
            .map(|t| t.level)
    }
}

/// Find (level, ix, iz) where a detailed tile matches the root tile exactly.
/// Level L tile spans 256 / 2^L root cells; every 2^L-th tile sample lies on a root sample.
fn locate(root: &[u16], tile: &[u16]) -> Option<(u32, u32, u32)> {
    let mut best: Option<(u64, u32, u32, u32)> = None;
    for level in 1..=7u32 {
        let step = 1usize << level;
        let span = CELLS / step;
        if span < 2 {
            break;
        }
        for iz in 0..(1u32 << level) {
            for ix in 0..(1u32 << level) {
                let mut diff = 0u64;
                for k in 0..=span {
                    for j in 0..=span {
                        let t = tile[(BORDER + k * step) * TILE + BORDER + j * step] as i64;
                        let r = root[(BORDER + iz as usize * span + k) * TILE + BORDER + ix as usize * span + j] as i64;
                        diff += (t - r).unsigned_abs();
                    }
                }
                if best.is_none_or(|b| diff < b.0) {
                    best = Some((diff, level, ix, iz));
                }
                if diff == 0 {
                    return Some((level, ix, iz));
                }
            }
        }
    }
    // Accept only an exact match; anything else is not located.
    best.filter(|b| b.0 == 0).map(|b| (b.1, b.2, b.3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_finds_exact_child() {
        // Root: a ramp. A level-1 tile at (1, 0) holds the same heights at double resolution.
        let mut root = vec![0u16; TILE * TILE];
        for r in 0..TILE {
            for c in 0..TILE {
                root[r * TILE + c] = (r * 7 + c * 3) as u16;
            }
        }
        let mut child = vec![0u16; TILE * TILE];
        for r in 0..TILE {
            for c in 0..TILE {
                // Child sample (r, c) lies at root position (2 + (r - 2) / 2, 2 + 128 + (c - 2) / 2).
                let rr = 2.0 + (r as f32 - 2.0) / 2.0;
                let rc = 2.0 + 128.0 + (c as f32 - 2.0) / 2.0;
                child[r * TILE + c] = (rr * 7.0 + rc * 3.0).max(0.0) as u16;
            }
        }
        assert_eq!(locate(&root, &child), Some((1, 1, 0)));
    }
}

/// Child order inside a node: (0,0), (1,0), (1,1), (0,1) as (x, z) offsets.
const CHILD_ORDER: [(u32, u32); 4] = [(0, 0), (1, 0), (1, 1), (0, 1)];

/// Rebuild each record's node position from the depth-first order of the
/// chunk table. Rules (seen on Verdun): a node's children follow it directly,
/// in CHILD_ORDER. Records without a height tile are leaves and are the next
/// children of the most recent node. A record with flags `.. .. ff` before
/// the first node belongs to the root and gets no position.
/// Height tiles are checked against their matched positions.
fn tree_positions(records: &[Record], located: &[Option<(u32, u32, u32)>], stats: &mut LoadStats) -> Vec<Option<(u32, u32, u32)>> {
    struct Frame {
        level: u32,
        ix: u32,
        iz: u32,
        next: usize,
    }
    let mut out = vec![None; records.len()];
    let mut stack = vec![Frame { level: 0, ix: 0, iz: 0, next: 0 }];
    for (i, r) in records.iter().enumerate() {
        if r.flags[2] == 0xff {
            continue;
        }
        match located[i] {
            Some((level, ix, iz)) => {
                while stack.len() > 1 && stack.last().is_some_and(|f| f.level >= level) {
                    stack.pop();
                }
                let top = stack.last_mut().unwrap();
                let expected = CHILD_ORDER.get(top.next).map(|(dx, dz)| (top.level + 1, top.ix * 2 + dx, top.iz * 2 + dz));
                if expected == Some((level, ix, iz)) {
                    stats.tree_position_agrees += 1;
                } else {
                    stats.tree_position_disagrees += 1;
                }
                top.next += 1;
                out[i] = Some((level, ix, iz));
                stack.push(Frame { level, ix, iz, next: 0 });
            }
            None => {
                while stack.len() > 1 && stack.last().is_some_and(|f| f.next >= 4) {
                    stack.pop();
                }
                let top = stack.last_mut().unwrap();
                if let Some((dx, dz)) = CHILD_ORDER.get(top.next) {
                    out[i] = Some((top.level + 1, top.ix * 2 + dx, top.iz * 2 + dz));
                }
                top.next += 1;
            }
        }
    }
    out
}

impl Terrain {
    /// The finest colour tile covering world (x, z), and the texture
    /// coordinate (0..1) of the point inside it.
    pub fn colour_at(&self, x: f32, z: f32) -> Option<(usize, [f32; 2])> {
        let u = (x - self.min[0]) / (self.max[0] - self.min[0]);
        let v = (z - self.min[2]) / (self.max[2] - self.min[2]);
        self.colours.iter().enumerate().find_map(|(i, t)| {
            let n = (1u32 << t.level) as f32;
            let (lu, lv) = (u * n - t.ix as f32, v * n - t.iz as f32);
            if (0.0..=1.0).contains(&lu) && (0.0..=1.0).contains(&lv) {
                let s = COLOUR_SIZE as f32;
                Some((i, [(2.0 + lu * 256.0) / s, (2.0 + lv * 256.0) / s]))
            } else {
                None
            }
        })
    }
}

impl Terrain {
    /// True if (x, z) is inside the terrain but in a hole.
    pub fn is_hole(&self, x: f32, z: f32) -> bool {
        self.level_at(x, z).is_some() && self.height_at(x, z).is_none()
    }

    /// World (x, z) of every hole sample in the finest tiles (for checks and logs).
    /// A sample counts only where its tile is the finest tile at that point.
    pub fn hole_positions(&self) -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        let (sx, sz) = (self.max[0] - self.min[0], self.max[2] - self.min[2]);
        for t in &self.tiles {
            let n = (1u32 << t.level) as f32;
            for r in BORDER..TILE - BORDER {
                for c in BORDER..TILE - BORDER {
                    if t.samples[r * TILE + c] != HOLE {
                        continue;
                    }
                    let x = self.min[0] + (t.ix as f32 + (c - BORDER) as f32 / CELLS as f32) / n * sx;
                    let z = self.min[2] + (t.iz as f32 + (r - BORDER) as f32 / CELLS as f32) / n * sz;
                    if self.level_at(x, z) == Some(t.level) {
                        out.push([x, z]);
                    }
                }
            }
        }
        out
    }
}

impl Terrain {
    /// Texture coordinate of world (x, z) inside colour tile `tile`.
    pub fn colour_uv(&self, tile: usize, x: f32, z: f32) -> [f32; 2] {
        let t = &self.colours[tile];
        let u = (x - self.min[0]) / (self.max[0] - self.min[0]);
        let v = (z - self.min[2]) / (self.max[2] - self.min[2]);
        let n = (1u32 << t.level) as f32;
        let (lu, lv) = (u * n - t.ix as f32, v * n - t.iz as f32);
        let s = COLOUR_SIZE as f32;
        [(2.0 + lu * 256.0) / s, (2.0 + lv * 256.0) / s]
    }
}

/// Roughness of a BC3 image: mean difference of the first colour endpoint
/// (RGB565) between horizontally neighbouring blocks. Real images are smooth
/// (low values); data read from the wrong place looks like noise (high).
pub fn bc3_roughness(data: &[u8]) -> f32 {
    const BLOCKS: usize = 65;
    let c0 = |b: usize| -> [i32; 3] {
        let v = u16::from_le_bytes([data[b * 16 + 8], data[b * 16 + 9]]) as i32;
        [(v >> 11) & 31, (v >> 5) & 63, v & 31]
    };
    let mut sum = 0i64;
    let mut n = 0i64;
    for y in 0..BLOCKS {
        for x in 0..BLOCKS - 1 {
            let (a, b) = (c0(y * BLOCKS + x), c0(y * BLOCKS + x + 1));
            sum += ((a[0] - b[0]).abs() * 2 + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() * 2) as i64;
            n += 1;
        }
    }
    sum as f32 / n as f32
}


/// Mean colour (0..1) of the first colour endpoints of a BC3 image.
pub fn bc3_mean_colour(data: &[u8]) -> [f32; 3] {
    let blocks = data.len() / 16;
    let mut sum = [0f64; 3];
    for b in 0..blocks {
        let v = u16::from_le_bytes([data[b * 16 + 8], data[b * 16 + 9]]) as u32;
        sum[0] += ((v >> 11) & 31) as f64 / 31.0;
        sum[1] += ((v >> 5) & 63) as f64 / 63.0;
        sum[2] += (v & 31) as f64 / 31.0;
    }
    sum.map(|s| (s / blocks as f64) as f32)
}

/// True if a mean colour looks like ground (earth, grass, mud, stone, snow),
/// not like a normal map (blue/purple) or a single-channel mask (red).
pub fn looks_like_ground(c: [f32; 3]) -> bool {
    let [r, g, b] = c;
    b <= r.max(g) + 0.06 && r <= g + 0.2
}

/// Strict limit: real colour tiles checked by eye are about 7 (record 25);
/// data read as the wrong thing is about 50. See STATUS.md.
const MAX_COLOUR_ROUGHNESS: f32 = 12.0;

/// True if a 67,600-byte layer in a height node is a ground colour image:
/// smooth, ground-coloured, and not almost black. Strict on purpose: a node
/// that fails uses its parent's colour (lower resolution, but never wrong).
pub fn is_colour_layer(layer: &[u8]) -> bool {
    let m = bc3_mean_colour(layer);
    bc3_roughness(layer) < MAX_COLOUR_ROUGHNESS && looks_like_ground(m) && (m[0] + m[1] + m[2]) / 3.0 > 0.08
}

/// Structure ratio of a BC3 image: endpoint difference between neighbouring
/// blocks divided by the difference between far-apart blocks. A real image
/// is far below 1 (neighbours are alike); noise is about 1.
pub fn bc3_structure_ratio(data: &[u8]) -> f32 {
    const BLOCKS: usize = 65;
    let n = BLOCKS * BLOCKS;
    let c0 = |b: usize| -> [i32; 3] {
        let v = u16::from_le_bytes([data[b * 16 + 8], data[b * 16 + 9]]) as i32;
        [(v >> 11) & 31, (v >> 5) & 63, v & 31]
    };
    let d = |a: [i32; 3], b: [i32; 3]| ((a[0] - b[0]).abs() * 2 + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() * 2) as f64;
    let (mut near, mut far) = (0f64, 0f64);
    for y in 0..BLOCKS {
        for x in 0..BLOCKS - 1 {
            let i = y * BLOCKS + x;
            near += d(c0(i), c0(i + 1));
            // A block far away (about half the image away, in a fixed pattern).
            far += d(c0(i), c0((i * 7 + n / 2) % n));
        }
    }
    if far == 0.0 {
        return 1.0;
    }
    (near / far) as f32
}

/// Decode a 260 x 260 BC3 layer to RGB.
pub fn decode_colour(layer: &[u8]) -> Vec<[u8; 3]> {
    let mut px = vec![0u32; COLOUR_SIZE * COLOUR_SIZE];
    let _ = texture2ddecoder::decode_bc3(layer, COLOUR_SIZE, COLOUR_SIZE, &mut px);
    px.iter().map(|p| { let b = p.to_le_bytes(); [b[2], b[1], b[0]] }).collect()
}

/// True for the marker colours in colour tiles: strong red (the game's "finer
/// data here" marker) and strong purple or blue. Real ground never has these
/// (0% in the L3 tiles of the battle area, 81% of the L2 battle area is red).
pub fn is_marker(p: [u8; 3]) -> bool {
    let (r, g, b) = (p[0] as u32, p[1] as u32, p[2] as u32);
    (r >= 2 * g && r >= 2 * b && r > 40) || (3 * b > 4 * g && b > 30)
}

/// Replace marker pixels of a decoded tile (`COLOUR_SIZE` square) with `fill`,
/// then blend them with their neighbours for a few pixels so the edge is soft.
/// Returns the number of marker pixels.
pub fn fill_markers(px: &mut [[u8; 3]], fill: [u8; 3]) -> usize {
    const BLEND_PASSES: usize = 8;
    let s = COLOUR_SIZE;
    let marker: Vec<bool> = px.iter().map(|p| is_marker(*p)).collect();
    let count = marker.iter().filter(|m| **m).count();
    if count == 0 {
        return 0;
    }
    let mut cur: Vec<[f32; 3]> = px.iter().zip(&marker).map(|(p, m)| if *m { fill } else { *p }.map(|c| c as f32)).collect();
    for _ in 0..BLEND_PASSES {
        let prev = cur.clone();
        for y in 0..s {
            for x in 0..s {
                let i = y * s + x;
                if !marker[i] {
                    continue;
                }
                let mut acc = prev[i];
                let mut k = 1.0;
                for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                    if nx < s && ny < s {
                        let q = prev[ny * s + nx];
                        acc = [acc[0] + q[0], acc[1] + q[1], acc[2] + q[2]];
                        k += 1.0;
                    }
                }
                cur[i] = acc.map(|c| c / k);
            }
        }
    }
    for (p, c) in px.iter_mut().zip(&cur) {
        *p = c.map(|v| v.round().clamp(0.0, 255.0) as u8);
    }
    count
}

impl Terrain {
    /// Mean colour (sRGB bytes) of the non-marker pixels of the finest colour
    /// tiles: the ground of the battle area. Used to fill marker pixels.
    pub fn ground_colour(&self) -> [u8; 3] {
        let Some(finest) = self.colours.iter().map(|c| c.level).max() else { return [102, 89, 73] };
        let (mut sum, mut n) = ([0u64; 3], 0u64);
        for c in self.colours.iter().filter(|c| c.level == finest) {
            for p in decode_colour(&c.data).into_iter().filter(|p| !is_marker(*p)) {
                for k in 0..3 {
                    sum[k] += p[k] as u64;
                }
                n += 1;
            }
        }
        if n == 0 {
            return [102, 89, 73];
        }
        sum.map(|v| (v / n) as u8)
    }
}

/// Pixel structure ratio of a decoded layer: mean difference between
/// neighbouring pixels divided by the mean difference between far-apart
/// pixels. A real image is far below 1; noise is about 1.
pub fn pixel_structure_ratio(px: &[[u8; 3]]) -> f32 {
    let s = COLOUR_SIZE;
    let n = s * s;
    let d = |a: [u8; 3], b: [u8; 3]| (0..3).map(|c| (a[c] as f64 - b[c] as f64).abs()).sum::<f64>();
    let (mut near, mut far) = (0f64, 0f64);
    for y in 0..s {
        for x in 0..s - 1 {
            let i = y * s + x;
            near += d(px[i], px[i + 1]);
            far += d(px[i], px[(i * 7 + n / 2) % n]);
        }
    }
    if far == 0.0 { 1.0 } else { (near / far) as f32 }
}
