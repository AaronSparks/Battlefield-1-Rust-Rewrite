//! Build the terrain as square patches of triangles from `Terrain::height_at`.
//! Patches near the battle area use a fine grid; patches with holes use the
//! finest grid (0.5 m) and leave out triangles that touch a hole; the rest use
//! a coarse grid. Each patch is coloured by the finest colour tile covering it.
//!
//! Where two patches with different grids meet, the finer edge follows the
//! true height between the coarse points, so thin cracks open. Such edges get
//! a skirt: a strip that hangs down from the edge (deeper than the measured
//! gap, plus a margin) and fills the crack. Edges between equal grids share
//! their points exactly and need no skirt.

use crate::convert;
use fb_terrain::Terrain;

pub struct PatchData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Index into `Terrain::colours`.
    pub colour: Option<usize>,
}

#[derive(Default, Debug)]
pub struct BuildStats {
    pub patches_with_holes: usize,
    pub triangles_left_out: usize,
    /// Patch edges next to a patch with a different grid (each side counts).
    pub seam_edges: usize,
    /// Largest height gap found on such an edge, and where (Frostbite x, z).
    pub max_gap: f32,
    pub max_gap_at: [f32; 2],
    pub skirt_triangles: usize,
    pub max_skirt_depth: f32,
}

/// Skirts reach this far below the measured gap.
const SKIRT_MARGIN: f32 = 0.25;

/// Patches covering the whole terrain. `centre` is in Frostbite x, z.
pub fn build(terrain: &Terrain, centre: [f32; 2], inner_half: f32, patch: f32, fine: f32, coarse: f32) -> (Vec<PatchData>, BuildStats) {
    let mut stats = BuildStats::default();
    let nx = ((terrain.max[0] - terrain.min[0]) / patch - 0.01).ceil() as usize;
    let nz = ((terrain.max[2] - terrain.min[2]) / patch - 0.01).ceil() as usize;
    let corner = |ix: usize, iz: usize| (terrain.min[0] + ix as f32 * patch, terrain.min[2] + iz as f32 * patch);
    // First the grid spacing of every patch, so each patch knows its neighbours.
    let mut spacing = vec![0f32; nx * nz];
    for iz in 0..nz {
        for ix in 0..nx {
            let (px, pz) = corner(ix, iz);
            let near = (px + patch > centre[0] - inner_half)
                && (px < centre[0] + inner_half)
                && (pz + patch > centre[1] - inner_half)
                && (pz < centre[1] + inner_half);
            // Look for holes on a 0.5 m grid (only near the battle area, where the fine tiles are).
            let has_hole = near && {
                let n = (patch / 0.5) as usize;
                (0..=n).any(|j| (0..=n).any(|i| terrain.is_hole(px + i as f32 * 0.5, pz + j as f32 * 0.5)))
            };
            if has_hole {
                stats.patches_with_holes += 1;
            }
            spacing[iz * nx + ix] = if has_hole { 0.5 } else if near { fine } else { coarse };
        }
    }
    let mut out = Vec::new();
    for iz in 0..nz {
        for ix in 0..nx {
            let (px, pz) = corner(ix, iz);
            let at = |x: isize, z: isize| (x >= 0 && z >= 0 && (x as usize) < nx && (z as usize) < nz).then(|| spacing[z as usize * nx + x as usize]);
            let (x, z) = (ix as isize, iz as isize);
            // Neighbour spacing per edge: z = pz, z = pz + patch, x = px, x = px + patch.
            let neighbours = [at(x, z - 1), at(x, z + 1), at(x - 1, z), at(x + 1, z)];
            out.push(build_patch(terrain, px, pz, patch, spacing[iz * nx + ix], neighbours, &mut stats));
        }
    }
    (out, stats)
}

fn build_patch(terrain: &Terrain, px: f32, pz: f32, patch: f32, spacing: f32, neighbours: [Option<f32>; 4], stats: &mut BuildStats) -> PatchData {
    let n = (patch / spacing).round() as usize + 1;
    let colour = terrain.colour_at(px + patch / 2.0, pz + patch / 2.0).map(|c| c.0);
    let mut d = PatchData { positions: vec![], normals: vec![], uvs: vec![], indices: vec![], colour };
    let mut hole = vec![false; n * n];
    // Height for normals: holes count as the patch's lowest known height.
    let h = |x: f32, z: f32| terrain.height_at(x, z);
    for j in 0..n {
        for i in 0..n {
            let (x, z) = (px + i as f32 * spacing, pz + j as f32 * spacing);
            let y = match h(x, z) {
                Some(y) => y,
                None => {
                    hole[j * n + i] = true;
                    terrain.min[1]
                }
            };
            d.positions.push(convert::point([x, y, z]));
            let e = spacing.max(1.0);
            let hh = |x: f32, z: f32| h(x, z).unwrap_or(y);
            let nx = hh(x - e, z) - hh(x + e, z);
            let nz = hh(x, z - e) - hh(x, z + e);
            let len = (nx * nx + 4.0 * e * e + nz * nz).sqrt();
            d.normals.push(convert::point([nx / len, 2.0 * e / len, nz / len]));
            d.uvs.push(match colour {
                Some(c) => terrain.colour_uv(c, x, z),
                None => [0.0, 0.0],
            });
        }
    }
    // Triangles: (a, b, c) and (b, d, c) per cell. Check the facing once below.
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            let a = j * n + i;
            let (b, c, dd) = (a + 1, a + n, a + n + 1);
            for t in [[a, b, c], [b, dd, c]] {
                if t.iter().any(|&k| hole[k]) {
                    stats.triangles_left_out += 1;
                } else {
                    tris.push(t.map(|k| k as u32));
                }
            }
        }
    }
    // Make triangles face up in Bevy's coordinates (counter-clockwise = front).
    let (p0, p1, p2) = (d.positions[0], d.positions[1], d.positions[n]);
    let (u, v) = ([p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]], [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]]);
    let flip = u[2] * v[0] - u[0] * v[2] < 0.0;
    for t in tris {
        if flip {
            d.indices.extend_from_slice(&[t[0], t[2], t[1]]);
        } else {
            d.indices.extend_from_slice(&t);
        }
    }

    // Skirts on edges next to a patch with a different grid.
    let edges: [(Vec<usize>, [f32; 2]); 4] = [
        ((0..n).collect(), [0.0, 0.0]),
        ((0..n).map(|i| (n - 1) * n + i).collect(), [0.0, patch]),
        ((0..n).map(|j| j * n).collect(), [0.0, 0.0]),
        ((0..n).map(|j| j * n + n - 1).collect(), [patch, 0.0]),
    ];
    for (e, ((vertices, start), other)) in edges.into_iter().zip(neighbours).enumerate() {
        let Some(other) = other.filter(|o| (o - spacing).abs() > 1e-3) else { continue };
        stats.seam_edges += 1;
        // Along x for the first two edges, along z for the others.
        let dir = if e < 2 { [1.0, 0.0] } else { [0.0, 1.0] };
        let (x0, z0) = (px + start[0], pz + start[1]);
        let point = |t: f32| [x0 + dir[0] * t, z0 + dir[1] * t];
        let gap = edge_gap(|t| terrain.height_at(point(t)[0], point(t)[1]), patch, spacing.min(other), spacing.max(other));
        if gap.0 > stats.max_gap {
            stats.max_gap = gap.0;
            stats.max_gap_at = point(gap.1);
        }
        let depth = gap.0 + SKIRT_MARGIN;
        stats.max_skirt_depth = stats.max_skirt_depth.max(depth);
        let base = d.positions.len() as u32;
        for &k in &vertices {
            let p = d.positions[k];
            d.positions.push([p[0], p[1] - depth, p[2]]);
            d.normals.push(d.normals[k]);
            d.uvs.push(d.uvs[k]);
        }
        for w in 0..vertices.len() - 1 {
            if hole[vertices[w]] || hole[vertices[w + 1]] {
                continue;
            }
            let (t0, t1) = (vertices[w] as u32, vertices[w + 1] as u32);
            let (b0, b1) = (base + w as u32, base + w as u32 + 1);
            // Both windings, so the skirt shows from either side of the crack.
            d.indices.extend_from_slice(&[t0, t1, b0, t1, b1, b0, t0, b0, t1, t1, b0, b1]);
            stats.skirt_triangles += 4;
        }
    }
    d
}

/// Largest height difference along an edge of `length` metres between the
/// fine patch's edge (true heights every `fine` metres) and the coarse patch's
/// edge (straight lines between heights every `coarse` metres), and the
/// distance along the edge where it is. `height(t)` is None in holes.
fn edge_gap(height: impl Fn(f32) -> Option<f32>, length: f32, fine: f32, coarse: f32) -> (f32, f32) {
    let mut best = (0.0f32, 0.0f32);
    let steps = (length / fine).round() as usize;
    for s in 0..=steps {
        let t = s as f32 * fine;
        let c0 = ((t / coarse).floor() * coarse).min(length - coarse);
        let f = ((t - c0) / coarse).clamp(0.0, 1.0);
        let (Some(h), Some(a), Some(b)) = (height(t), height(c0), height(c0 + coarse)) else { continue };
        let gap = (h - (a + (b - a) * f)).abs();
        if gap > best.0 {
            best = (gap, t);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight slope has no cracks: the coarse edge's straight lines match.
    #[test]
    fn edge_gap_is_zero_on_a_slope() {
        let (gap, _) = edge_gap(|t| Some(3.0 + 0.25 * t), 64.0, 1.0, 8.0);
        assert!(gap < 1e-4, "gap {gap}");
    }

    /// A bump between two coarse points: the gap is the bump height, at the bump.
    #[test]
    fn edge_gap_finds_a_bump() {
        let (gap, at) = edge_gap(|t| Some(if t == 12.0 { 2.0 } else { 0.0 }), 64.0, 1.0, 8.0);
        assert!((gap - 2.0).abs() < 1e-5 && at == 12.0, "gap {gap} at {at}");
    }

    /// Hole points are skipped.
    #[test]
    fn edge_gap_skips_holes() {
        let (gap, _) = edge_gap(|t| if t == 12.0 { None } else { Some(0.0) }, 64.0, 1.0, 8.0);
        assert_eq!(gap, 0.0);
    }
}
