//! Indoor light with a light probe grid (DESIGN.md "indoor light with a
//! light probe grid"). The game stores precomputed light in probes; we
//! compute our own: in boxes around buildings and bunkers, each 1 m cell
//! stores how much of the sky it can see from 6 directions. Bevy's
//! irradiance volumes add that light to the plain sky light, so a cell holds
//! (visible share - 1) x the plain sky light: 0 outdoors, negative indoors.
//!
//! Steps: 1. find the covered areas and boxes; 2. bake the visibility;
//! 3. Bevy volumes (`apply_volumes`).

use crate::load::Loaded;
use crate::log::GameLog;
use bevy::math::{Mat4, Vec3};

/// Side of the square area searched for covered places (metres), around
/// the battle area centre (the same square as the fine ground weights).
const AREA: f32 = 1024.0;
/// Coverage map cell (metres).
const COLUMN: f32 = 1.0;
/// Tile size for grouping covered columns into boxes, and the largest box
/// side (metres). Bevy uses the 8 boxes nearest to the camera.
const TILE: f32 = 16.0;
const CHUNK: f32 = 64.0;
/// Geometry this far above the terrain covers a column (roofs, ceilings).
const COVER_MIN: f32 = 2.2;
const COVER_MAX: f32 = 30.0;
/// Geometry this far below the terrain is underground (bunkers); props that
/// only sit a little in the ground do not count.
const UNDER_MIN: f32 = 2.5;

/// Meshes that do not make a place "indoors" and are left out of the light
/// probe occluders: vegetation (trees let most sky through their crowns) and
/// the far backdrop.
fn is_open_mesh(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["tree", "bush", "hedge", "ivy", "vegetation", "branch", "root", "stump", "backdrop", "foliage", "grass", "fern", "shrub", "log_"].iter().any(|k| n.contains(k))
}

/// World-space triangles of the solid parts of the placed meshes (no leaf
/// cards, no decals or other non-blocking meshes), inside a rectangle.
pub fn solid_triangles(level: &Loaded, min: [f32; 2], max: [f32; 2]) -> Vec<[Vec3; 3]> {
    let mut out = Vec::new();
    for (mesh_index, matrix) in &level.placements {
        let mesh = &level.meshes[*mesh_index];
        if crate::collision::is_non_blocking(&mesh.name) || is_open_mesh(&mesh.name) {
            continue;
        }
        let m = Mat4::from_cols_array(matrix);
        let o = m.transform_point3(Vec3::ZERO);
        // Placements far outside (generous: large meshes reach far from their origin).
        if o.x < min[0] - 200.0 || o.x > max[0] + 200.0 || o.z < min[1] - 200.0 || o.z > max[1] + 200.0 {
            continue;
        }
        for p in &mesh.parts {
            if p.texture.is_some_and(|t| level.textures[t].cutout) || p.decal.is_some() {
                continue;
            }
            for t in p.indices.chunks_exact(3) {
                let v = |i: u32| m.transform_point3(Vec3::from(p.positions[i as usize]));
                let tri = [v(t[0]), v(t[1]), v(t[2])];
                let (lo, hi) = (tri[0].min(tri[1]).min(tri[2]), tri[0].max(tri[1]).max(tri[2]));
                if hi.x < min[0] || lo.x > max[0] || hi.z < min[1] || lo.z > max[1] {
                    continue;
                }
                out.push(tri);
            }
        }
    }
    out
}

/// A probe box: world min and max corners.
#[derive(Debug, Clone, Copy)]
pub struct ProbeBox {
    pub min: Vec3,
    pub max: Vec3,
}

/// Covered or underground columns (1 m) of the searched square.
pub struct Coverage {
    pub x0: f32,
    pub z0: f32,
    pub n: usize,
    pub marked: Vec<bool>,
}

impl Coverage {
    /// Is the column at (x, z), or one within `margin` columns, marked?
    pub fn near(&self, x: f32, z: f32, margin: isize) -> bool {
        let (ix, iz) = (((x - self.x0) / COLUMN).floor() as isize, ((z - self.z0) / COLUMN).floor() as isize);
        for dz in -margin..=margin {
            for dx in -margin..=margin {
                let (a, b) = (ix + dx, iz + dz);
                if a >= 0 && b >= 0 && (a as usize) < self.n && (b as usize) < self.n && self.marked[b as usize * self.n + a as usize] {
                    return true;
                }
            }
        }
        false
    }
}

/// Step 1: covered columns -> tiles -> boxes. Logs what it finds.
pub fn find_boxes(level: &Loaded, tris: &[[Vec3; 3]], log: &GameLog) -> (Vec<ProbeBox>, Coverage) {
    let t0 = std::time::Instant::now();
    let c = Vec3::from(level.centre);
    let (x0, z0) = (c.x - AREA / 2.0, c.z - AREA / 2.0);
    let n = (AREA / COLUMN) as usize;
    let height = |x: f32, z: f32| level.terrain.height_at(x, z);
    // Per column: covered (roof-like geometry above) or underground geometry.
    let mut covered = vec![false; n * n];
    let mut under = vec![false; n * n];
    // Per column: lowest and highest solid geometry (for box heights).
    let mut lo_y = vec![f32::MAX; n * n];
    let mut hi_y = vec![f32::MIN; n * n];
    for tri in tris {
        let normal = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
        let roof_like = normal.y.abs() > 0.3;
        // Sample points on the triangle about every half column.
        let edge = (tri[1] - tri[0]).length().max((tri[2] - tri[0]).length()).max((tri[2] - tri[1]).length());
        let steps = ((edge / (COLUMN * 0.5)).ceil() as usize).clamp(1, 400);
        for i in 0..=steps {
            for j in 0..=(steps - i) {
                let (a, b) = (i as f32 / steps as f32, j as f32 / steps as f32);
                let p = tri[0] + (tri[1] - tri[0]) * a + (tri[2] - tri[0]) * b;
                let (ix, iz) = (((p.x - x0) / COLUMN) as isize, ((p.z - z0) / COLUMN) as isize);
                if ix < 0 || iz < 0 || ix >= n as isize || iz >= n as isize {
                    continue;
                }
                let k = iz as usize * n + ix as usize;
                let Some(h) = height(p.x, p.z) else {
                    // A terrain hole: bunker entrance.
                    under[k] = true;
                    lo_y[k] = lo_y[k].min(p.y);
                    hi_y[k] = hi_y[k].max(p.y);
                    continue;
                };
                let above = p.y - h;
                if roof_like && (COVER_MIN..COVER_MAX).contains(&above) {
                    covered[k] = true;
                }
                if above < -UNDER_MIN {
                    under[k] = true;
                }
                if above > -40.0 && above < COVER_MAX {
                    lo_y[k] = lo_y[k].min(p.y);
                    hi_y[k] = hi_y[k].max(p.y);
                }
            }
        }
    }
    // Tiles with any covered or underground column.
    let per_tile = (TILE / COLUMN) as usize;
    let nt = n / per_tile;
    let mut tile = vec![false; nt * nt];
    for iz in 0..n {
        for ix in 0..n {
            if covered[iz * n + ix] || under[iz * n + ix] {
                tile[(iz / per_tile) * nt + ix / per_tile] = true;
            }
        }
    }
    // Boxes: one per 64 m chunk of tiles (chunks do not overlap, and Bevy
    // averages overlapping volumes), over the chunk's marked tiles. Heights
    // from the covered or underground columns only: their geometry and
    // terrain.
    let per_chunk = (CHUNK / TILE) as usize;
    let mut boxes = Vec::new();
    for chz in (0..nt).step_by(per_chunk) {
        for chx in (0..nt).step_by(per_chunk) {
            let (mut tmin, mut tmax) = ((usize::MAX, usize::MAX), (0usize, 0usize));
            for tz in chz..(chz + per_chunk).min(nt) {
                for tx in chx..(chx + per_chunk).min(nt) {
                    if tile[tz * nt + tx] {
                        tmin = (tmin.0.min(tx), tmin.1.min(tz));
                        tmax = (tmax.0.max(tx), tmax.1.max(tz));
                    }
                }
            }
            if tmin.0 == usize::MAX {
                continue;
            }
            let (cx0, cz0) = (tmin.0 * per_tile, tmin.1 * per_tile);
            let (cx1, cz1) = ((tmax.0 + 1) * per_tile, (tmax.1 + 1) * per_tile);
            let (mut ylo, mut yhi) = (f32::MAX, f32::MIN);
            for iz in cz0..cz1 {
                for ix in cx0..cx1 {
                    let k = iz * n + ix;
                    if !(covered[k] || under[k]) {
                        continue;
                    }
                    if lo_y[k] <= hi_y[k] {
                        ylo = ylo.min(lo_y[k]);
                        yhi = yhi.max(hi_y[k]);
                    }
                    if let Some(h) = height(x0 + (ix as f32 + 0.5) * COLUMN, z0 + (iz as f32 + 0.5) * COLUMN) {
                        ylo = ylo.min(h);
                        yhi = yhi.max(h);
                    }
                }
            }
            if ylo > yhi {
                continue;
            }
            boxes.push(ProbeBox {
                min: Vec3::new(x0 + cx0 as f32 * COLUMN, ylo - 1.0, z0 + cz0 as f32 * COLUMN),
                max: Vec3::new(x0 + cx1 as f32 * COLUMN, yhi + 2.0, z0 + cz1 as f32 * COLUMN),
            });
        }
    }
    let cells: f64 = boxes.iter().map(|b| ((b.max - b.min) / COLUMN).ceil().to_array().iter().map(|v| *v as f64).product::<f64>()).sum();
    let covered_count = covered.iter().filter(|c| **c).count();
    let under_count = under.iter().filter(|c| **c).count();
    log.line(format!(
        "light probes, step 1 ({:.2} s): {} solid triangles; covered columns {covered_count}, underground columns {under_count} (of {}); {} boxes, {:.2} million 1 m cells ({:.0} MB as 16-bit float ambient cubes)",
        t0.elapsed().as_secs_f64(),
        tris.len(),
        n * n,
        boxes.len(),
        cells / 1e6,
        cells * 48.0 / 1e6
    ));
    let mut sorted: Vec<&ProbeBox> = boxes.iter().collect();
    sorted.sort_by(|a, b| ((b.max - b.min).element_product()).total_cmp(&(a.max - a.min).element_product()));
    for b in sorted.iter().take(5) {
        let s = b.max - b.min;
        log.line(format!("  box min ({:.0}, {:.0}, {:.0}) size {:.0} x {:.0} x {:.0} m", b.min.x, b.min.y, b.min.z, s.x, s.y, s.z));
    }
    let marked = covered.iter().zip(&under).map(|(a, b)| *a || *b).collect();
    (boxes, Coverage { x0, z0, n, marked })
}

// ---------------------------------------------------------------------------
// Step 2: the bake.

/// Occupancy voxel size (metres) and voxels per block side.
const VOXEL: f32 = 0.5;
const BLOCK: i32 = 16;
/// Probe cell size (metres), rays per direction, ray length and step.
const CELL: f32 = 1.0;
const RAYS: usize = 12;
const RAY_LENGTH: f32 = 40.0;
const RAY_STEP: f32 = 0.4;

/// Solid geometry as 0.5 m voxels, in sparse 8 m blocks.
pub struct Occupancy {
    blocks: std::collections::HashMap<(i32, i32, i32), Box<[u64; 64]>>,
    /// Highest solid point (rays above it are in the open).
    pub top: f32,
}

impl Occupancy {
    fn key(p: Vec3) -> ((i32, i32, i32), usize) {
        let v = (p / VOXEL).floor();
        let (x, y, z) = (v.x as i32, v.y as i32, v.z as i32);
        let b = (x.div_euclid(BLOCK), y.div_euclid(BLOCK), z.div_euclid(BLOCK));
        let (lx, ly, lz) = (x.rem_euclid(BLOCK), y.rem_euclid(BLOCK), z.rem_euclid(BLOCK));
        (b, ((ly * BLOCK + lz) * BLOCK + lx) as usize)
    }

    fn set(&mut self, p: Vec3) {
        let (b, bit) = Self::key(p);
        self.blocks.entry(b).or_insert_with(|| Box::new([0u64; 64]))[bit / 64] |= 1 << (bit % 64);
    }

    pub fn get(&self, p: Vec3) -> bool {
        let (b, bit) = Self::key(p);
        self.blocks.get(&b).is_some_and(|blk| blk[bit / 64] & (1 << (bit % 64)) != 0)
    }

    /// Mark the voxels that the triangles pass through (points every 0.3 m).
    pub fn build(tris: &[[Vec3; 3]]) -> Occupancy {
        let mut o = Occupancy { blocks: Default::default(), top: f32::MIN };
        for tri in tris {
            let edge = (tri[1] - tri[0]).length().max((tri[2] - tri[0]).length()).max((tri[2] - tri[1]).length());
            let steps = ((edge / 0.3).ceil() as usize).clamp(1, 600);
            for i in 0..=steps {
                for j in 0..=(steps - i) {
                    let (a, b) = (i as f32 / steps as f32, j as f32 / steps as f32);
                    o.set(tri[0] + (tri[1] - tri[0]) * a + (tri[2] - tri[0]) * b);
                }
            }
            o.top = o.top.max(tri[0].y).max(tri[1].y).max(tri[2].y);
        }
        o
    }

    pub fn blocks(&self) -> usize {
        self.blocks.len()
    }
}

/// Terrain heights on a 1 m grid (NaN in holes), bilinear lookup.
pub struct HeightGrid {
    x0: f32,
    z0: f32,
    n: usize,
    h: Vec<f32>,
}

impl HeightGrid {
    pub fn build(terrain: &fb_terrain::Terrain, x0: f32, z0: f32, n: usize) -> HeightGrid {
        let h = (0..n * n).map(|k| terrain.height_at(x0 + (k % n) as f32, z0 + (k / n) as f32).unwrap_or(f32::NAN)).collect();
        HeightGrid { x0, z0, n, h }
    }

    /// Height at (x, z); NaN in a hole; minus infinity outside the grid
    /// (treated as open ground far below).
    pub fn at(&self, x: f32, z: f32) -> f32 {
        let (fx, fz) = (x - self.x0, z - self.z0);
        if fx < 0.0 || fz < 0.0 || fx >= (self.n - 1) as f32 || fz >= (self.n - 1) as f32 {
            return f32::NEG_INFINITY;
        }
        let (ix, iz) = (fx as usize, fz as usize);
        let (tx, tz) = (fx - ix as f32, fz - iz as f32);
        let g = |a: usize, b: usize| self.h[b * self.n + a];
        let top = g(ix, iz) * (1.0 - tx) + g(ix + 1, iz) * tx;
        let bottom = g(ix, iz + 1) * (1.0 - tx) + g(ix + 1, iz + 1) * tx;
        top * (1.0 - tz) + bottom * tz
    }
}

/// 12 directions around +Z, cosine-weighted (Fibonacci spiral on the disc).
fn hemisphere() -> [Vec3; RAYS] {
    std::array::from_fn(|i| {
        let r = ((i as f32 + 0.5) / RAYS as f32).sqrt();
        let a = i as f32 * 2.399_963;
        let (x, y) = (r * a.cos(), r * a.sin());
        Vec3::new(x, y, (1.0 - x * x - y * y).max(0.0).sqrt())
    })
}

/// The 6 face directions in Bevy's order: +X, -X, +Y, -Y, +Z, -Z.
const FACES: [Vec3; 6] = [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z];

/// Does a ray from `o` reach open sky? A mesh blocks it only where that place
/// is covered (a roof or ceiling above, or underground); outdoor objects and
/// the terrain hit from above count as open (outdoors they reflect light,
/// which the plain sky light already includes). From below (bunkers), the
/// terrain blocks.
fn ray_open(occ: &Occupancy, hg: &HeightGrid, coverage: &Coverage, o: Vec3, d: Vec3) -> bool {
    let start_h = hg.at(o.x, o.z);
    let underground = !start_h.is_nan() && o.y < start_h;
    let mut t = RAY_STEP;
    while t < RAY_LENGTH {
        let p = o + d * t;
        if occ.get(p) {
            return !(underground || coverage.near(p.x, p.z, 0));
        }
        let h = hg.at(p.x, p.z);
        if !h.is_nan() {
            if underground && p.y > h {
                return false;
            }
            if !underground && p.y < h {
                return true;
            }
        }
        if !underground && p.y > occ.top + 1.0 {
            return true;
        }
        t += RAY_STEP;
    }
    !underground
}

/// A baked probe box: resolution and per cell the visible share of the sky
/// for the 6 face directions (0..255).
pub struct BakedBox {
    pub min: Vec3,
    pub max: Vec3,
    pub res: [usize; 3],
    pub vis: Vec<[u8; 6]>,
}

/// Step 2: visibility of every probe cell near covered columns; other cells
/// are open. Cells inside solid voxels take the mean of their neighbours.
pub fn bake(boxes: &[ProbeBox], coverage: &Coverage, occ: &Occupancy, hg: &HeightGrid, log: &GameLog) -> Vec<BakedBox> {
    let t0 = std::time::Instant::now();
    let dirs = hemisphere();
    let basis = |n: Vec3| {
        let up = if n.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let t = up.cross(n).normalize();
        (t, n.cross(t))
    };
    let face_dirs: Vec<[Vec3; RAYS]> = FACES
        .iter()
        .map(|&n| {
            let (t, b) = basis(n);
            dirs.map(|d| (t * d.x + b * d.y + n * d.z).normalize())
        })
        .collect();
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(32);
    let (mut computed, mut solid, mut total) = (0usize, 0usize, 0usize);
    let mut hist = [0usize; 5];
    let mut out = Vec::new();
    for b in boxes {
        let size = b.max - b.min;
        let res = [(size.x / CELL).ceil().max(1.0) as usize, (size.y / CELL).ceil().max(1.0) as usize, (size.z / CELL).ceil().max(1.0) as usize];
        let cell = Vec3::new(size.x / res[0] as f32, size.y / res[1] as f32, size.z / res[2] as f32);
        let count = res[0] * res[1] * res[2];
        // Per cell: Some(visibility) or None (inside solid), and whether rays were cast.
        let results: Vec<(Option<[u8; 6]>, bool)> = std::thread::scope(|sc| {
            let chunk = count.div_ceil(threads);
            let jobs: Vec<_> = (0..threads)
                .map(|ti| {
                    let face_dirs = &face_dirs;
                    sc.spawn(move || {
                        let mut part = Vec::new();
                        for idx in ti * chunk..((ti + 1) * chunk).min(count) {
                            let (x, y, z) = (idx % res[0], (idx / res[0]) % res[1], idx / (res[0] * res[1]));
                            let p = b.min + Vec3::new((x as f32 + 0.5) * cell.x, (y as f32 + 0.5) * cell.y, (z as f32 + 0.5) * cell.z);
                            if !coverage.near(p.x, p.z, 2) {
                                part.push((Some([255u8; 6]), false));
                                continue;
                            }
                            if occ.get(p) {
                                part.push((None, false));
                                continue;
                            }
                            let v: [u8; 6] = std::array::from_fn(|f| {
                                let open = face_dirs[f].iter().filter(|d| ray_open(occ, hg, coverage, p, **d)).count();
                                (open * 255 / RAYS) as u8
                            });
                            part.push((Some(v), true));
                        }
                        part
                    })
                })
                .collect();
            jobs.into_iter().flat_map(|j| j.join().expect("probe bake thread")).collect()
        });
        total += count;
        computed += results.iter().filter(|r| r.1).count();
        // Fill cells inside solid geometry from their valid neighbours (2 passes).
        let mut vis: Vec<Option<[u8; 6]>> = results.into_iter().map(|r| r.0).collect();
        for _ in 0..2 {
            let prev = vis.clone();
            for idx in 0..count {
                if prev[idx].is_some() {
                    continue;
                }
                let (x, y, z) = ((idx % res[0]) as isize, ((idx / res[0]) % res[1]) as isize, (idx / (res[0] * res[1])) as isize);
                let (mut sum, mut k) = ([0u32; 6], 0u32);
                for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
                    let (a, b2, c) = (x + dx, y + dy, z + dz);
                    if a < 0 || b2 < 0 || c < 0 || a >= res[0] as isize || b2 >= res[1] as isize || c >= res[2] as isize {
                        continue;
                    }
                    if let Some(v) = prev[(c as usize * res[1] + b2 as usize) * res[0] + a as usize] {
                        for f in 0..6 {
                            sum[f] += v[f] as u32;
                        }
                        k += 1;
                    }
                }
                if k > 0 {
                    vis[idx] = Some(sum.map(|v| (v / k) as u8));
                }
            }
        }
        let vis: Vec<[u8; 6]> = vis
            .into_iter()
            .map(|v| {
                if v.is_none() {
                    solid += 1;
                }
                v.unwrap_or([255; 6])
            })
            .collect();
        for v in &vis {
            let mean = v.iter().map(|&x| x as usize).sum::<usize>() / 6;
            hist[(mean * 5 / 256).min(4)] += 1;
        }
        out.push(BakedBox { min: b.min, max: b.max, res, vis });
    }
    log.line(format!(
        "light probes, step 2 ({:.2} s, {threads} threads): {total} cells, {computed} computed ({RAYS} rays per direction), {solid} left inside solid; mean visibility 0-20% {}, 20-40% {}, 40-60% {}, 60-80% {}, 80-100% {} cells",
        t0.elapsed().as_secs_f64(),
        hist[0],
        hist[1],
        hist[2],
        hist[3],
        hist[4]
    ));
    out
}

// ---------------------------------------------------------------------------
// Step 3: Bevy irradiance volumes.

/// f32 to IEEE half (round to nearest; clamps to the half range).
fn f16_bits(v: f32) -> u16 {
    let sign = ((v.to_bits() >> 16) & 0x8000) as u16;
    let a = v.abs();
    if a.is_nan() {
        return 0x7e00;
    }
    if a >= 65504.0 {
        return sign | 0x7bff;
    }
    if a < 6.103_515_6e-5 {
        // Subnormal half: steps of 2^-24.
        return sign | (a / 5.960_464_5e-8).round() as u16;
    }
    let e = ((a.to_bits() >> 23) & 0xff) as i32 - 127;
    let m = a.to_bits() & 0x7f_ffff;
    let mut h = (((e + 15) as u32) << 10) | (m >> 13);
    // Round to nearest (ties up).
    if m & 0x1000 != 0 {
        h += 1;
    }
    sign | h as u16
}

/// Share of the plain sky light that stays where the sky is fully hidden:
/// light that comes in through openings and bounces inside (the game's global
/// illumination; we do not compute it). A fit to the user's screenshots;
/// `BF1_INDOOR_FILL` overrides it for tests.
pub const INDOOR_FILL: f32 = 0.3;

/// Bevy's plain sky light reaches a diffuse surface scaled by
/// `F_AB(1.0, NdotV).x` (bevy_pbr `ambient.wgsl`, polynomial version): 0.452
/// for any angle. Irradiance volume light is not scaled, so the correction
/// is scaled by this to remove the right amount.
const AMBIENT_DIFFUSE: f32 = 0.452;

fn indoor_fill() -> f32 {
    std::env::var("BF1_INDOOR_FILL").ok().and_then(|v| v.parse().ok()).unwrap_or(INDOOR_FILL)
}

/// The volume texture for one box. Per face, the light should be
/// vis x A + (1 - vis) x fill x W, where A is the plain sky light and W the
/// indoor light: the bounce colour (warm) at A's brightness. The volume adds
/// the difference to A (0 where the sky is fully visible).
pub fn volume_image(b: &BakedBox, ambient: [f32; 3], bounce: [f32; 3]) -> bevy::prelude::Image {
    let fill = indoor_fill();
    let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let warm: [f32; 3] = if lum(bounce) > 0.0 { bounce.map(|c| c * lum(ambient) / lum(bounce)) } else { ambient };
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    let [rx, ry, rz] = b.res;
    let (w, h, d) = (rx, 2 * ry, 3 * rz);
    let mut data = vec![0u8; w * h * d * 8];
    for z in 0..rz {
        for y in 0..ry {
            for x in 0..rx {
                let v = b.vis[(z * ry + y) * rx + x];
                for (f, &vf) in v.iter().enumerate() {
                    let vis = vf as f32 / 255.0;
                    // Bevy's shader: one slice per axis (X, Y, Z); the negative
                    // side is in the second half of the rows.
                    let (axis, negative) = (f / 2, f % 2 == 1);
                    let (s, t, p) = (x, y + if negative { ry } else { 0 }, z + axis * rz);
                    let at = ((p * h + t) * w + s) * 8;
                    for c in 0..3 {
                        let target = vis * ambient[c] + (1.0 - vis) * fill * warm[c];
                        data[at + c * 2..at + c * 2 + 2].copy_from_slice(&f16_bits(AMBIENT_DIFFUSE * (target - ambient[c])).to_le_bytes());
                    }
                    data[at + 6..at + 8].copy_from_slice(&f16_bits(1.0).to_le_bytes());
                }
            }
        }
    }
    bevy::prelude::Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: d as u32 },
        TextureDimension::D3,
        data,
        TextureFormat::Rgba16Float,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    )
}

/// The baked boxes and their Bevy volumes.
#[derive(bevy::prelude::Resource)]
pub struct ProbeVolumes {
    pub baked: Vec<BakedBox>,
    /// Weather whose sky light is in the textures.
    pub applied: Option<usize>,
    pub images: Vec<bevy::prelude::Handle<bevy::prelude::Image>>,
}

/// Plain sky light (sky + bounce) of a weather, linear RGB in cd/m2: what
/// the volumes subtract where the sky is hidden.
fn plain_sky_light(e: &crate::environment::Environment, ground_albedo: [f32; 3]) -> [f32; 3] {
    let a = crate::scene::ambient_for(Some(e), ground_albedo);
    let c = a.color.to_linear();
    [c.red * a.brightness, c.green * a.brightness, c.blue * a.brightness]
}

/// Spawn the volumes once, and rewrite their textures when the weather
/// changes (they scale with the plain sky light).
pub fn apply_volumes(
    mut commands: bevy::prelude::Commands,
    env: bevy::prelude::Res<crate::scene::Env>,
    volumes: Option<bevy::prelude::ResMut<ProbeVolumes>>,
    mut images: bevy::prelude::ResMut<bevy::prelude::Assets<bevy::prelude::Image>>,
    log: bevy::prelude::Res<GameLog>,
) {
    use bevy::prelude::*;
    let Some(mut volumes) = volumes else { return };
    if env.list.is_empty() || volumes.applied == Some(env.current) {
        return;
    }
    let sky = plain_sky_light(&env.list[env.current], env.ground_albedo);
    let bounce = crate::scene::bounce_light(&env.list[env.current], env.ground_albedo);
    if volumes.images.is_empty() {
        let mut handles = Vec::new();
        for b in &volumes.baked {
            let handle = images.add(volume_image(b, sky, bounce));
            commands.spawn((
                bevy::light::LightProbe::default(),
                bevy::light::IrradianceVolume { voxels: handle.clone(), intensity: 1.0, affects_lightmapped_meshes: true },
                Transform::from_translation((b.min + b.max) * 0.5).with_scale(b.max - b.min),
            ));
            handles.push(handle);
        }
        volumes.images = handles;
    } else {
        for (b, h) in volumes.baked.iter().zip(&volumes.images) {
            if let Some(mut img) = images.get_mut(h) {
                *img = volume_image(b, sky, bounce);
            }
        }
    }
    volumes.applied = Some(env.current);
    log.line(format!("light probes: {} volumes for {} (plain sky light {:.0?} cd/m2)", volumes.baked.len(), env.list[env.current].name, sky));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(f16_bits(0.0), 0);
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(-2.0), 0xc000);
        assert_eq!(f16_bits(65504.0), 0x7bff);
        assert_eq!(f16_bits(-1000.0), 0xe3d0);
        assert_eq!(f16_bits(0.5), 0x3800);
    }

    #[test]
    fn hemisphere_directions_are_unit_and_upward() {
        for d in hemisphere() {
            assert!((d.length() - 1.0).abs() < 1e-4 && d.z > 0.0, "{d:?}");
        }
    }

    #[test]
    fn occupancy_marks_a_triangle() {
        let tri = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 4.0)];
        let o = Occupancy::build(&[tri]);
        assert!(o.get(Vec3::new(1.0, 0.1, 1.0)));
        assert!(!o.get(Vec3::new(1.0, 1.0, 1.0)));
        assert!(!o.get(Vec3::new(3.5, 0.1, 3.5)));
    }
}
