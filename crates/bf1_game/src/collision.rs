//! Simple collision for walking: the placed meshes near the player, baked into
//! world-space triangles on a grid, plus sphere-against-triangle push-out.
//!
//! The game has separate collision meshes (Havok); we do not decode those
//! yet, so we use the visible meshes (LOD 0) and leave out meshes that never
//! block the player (decals, scattered debris, cloth, effects).

use bevy::math::{Mat4, Vec3};
use bevy::prelude::Resource;
use std::collections::HashMap;

/// Radius of the area baked around the player (metres), and how far the
/// player may move before it is rebuilt.
pub const BAKE_RADIUS: f32 = 32.0;
pub const REBAKE_DISTANCE: f32 = 10.0;
const CELL: f32 = 4.0;

pub struct Placement {
    pub mesh: usize,
    pub transform: Mat4,
    pub min: Vec3,
    pub max: Vec3,
}

/// All collision data: unique mesh triangles (local space) and placements.
#[derive(Resource, Default)]
pub struct CollisionWorld {
    /// Per unique mesh: vertices and triangle indices (Bevy coordinates, local space).
    pub meshes: Vec<(Vec<Vec3>, Vec<[u32; 3]>)>,
    /// Name of each unique mesh (for logs).
    pub names: Vec<String>,
    pub placements: Vec<Placement>,
    /// The baked area around the player.
    pub local: LocalArea,
}

#[derive(Default)]
pub struct LocalArea {
    pub centre: Option<Vec3>,
    pub triangles: Vec<[Vec3; 3]>,
    /// Unique mesh index of each triangle.
    pub owner: Vec<u32>,
    grid: HashMap<(i32, i32, i32), Vec<u32>>,
}

/// True for meshes that never block the player.
pub fn is_non_blocking(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["decal", "meshscatter", "cloth", "/fx/", "fx/", "grass", "debrisbranch", "smoke", "fire"].iter().any(|k| n.contains(k))
}

impl CollisionWorld {
    pub fn add_mesh(&mut self, name: &str, vertices: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> usize {
        self.names.push(name.to_string());
        self.meshes.push((vertices, triangles));
        self.meshes.len() - 1
    }

    pub fn add_placement(&mut self, mesh: usize, transform: Mat4) {
        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &self.meshes[mesh].0 {
            let w = transform.transform_point3(*v);
            min = min.min(w);
            max = max.max(w);
        }
        if min.x <= max.x {
            self.placements.push(Placement { mesh, transform, min, max });
        }
    }

    /// Rebuild the baked area if the player moved far enough. Returns the
    /// number of triangles if it was rebuilt.
    pub fn update_area(&mut self, at: Vec3) -> Option<usize> {
        if self.local.centre.is_some_and(|c| c.distance(at) < REBAKE_DISTANCE) {
            return None;
        }
        let (lo, hi) = (at - Vec3::splat(BAKE_RADIUS), at + Vec3::splat(BAKE_RADIUS));
        let mut area = LocalArea { centre: Some(at), triangles: Vec::new(), owner: Vec::new(), grid: HashMap::new() };
        for p in &self.placements {
            if p.max.x < lo.x || p.min.x > hi.x || p.max.y < lo.y || p.min.y > hi.y || p.max.z < lo.z || p.min.z > hi.z {
                continue;
            }
            let (verts, tris) = &self.meshes[p.mesh];
            let world: Vec<Vec3> = verts.iter().map(|v| p.transform.transform_point3(*v)).collect();
            for t in tris {
                let tri = [world[t[0] as usize], world[t[1] as usize], world[t[2] as usize]];
                let (tmin, tmax) = (tri[0].min(tri[1]).min(tri[2]), tri[0].max(tri[1]).max(tri[2]));
                if tmax.x < lo.x || tmin.x > hi.x || tmax.y < lo.y || tmin.y > hi.y || tmax.z < lo.z || tmin.z > hi.z {
                    continue;
                }
                let index = area.triangles.len() as u32;
                area.triangles.push(tri);
                area.owner.push(p.mesh as u32);
                let (c0, c1) = (cell_of(tmin), cell_of(tmax));
                // Very large triangles span many cells; cap the work.
                if (c1.0 - c0.0 + 1) * (c1.1 - c0.1 + 1) * (c1.2 - c0.2 + 1) > 4096 {
                    continue;
                }
                for x in c0.0..=c1.0 {
                    for y in c0.1..=c1.1 {
                        for z in c0.2..=c1.2 {
                            area.grid.entry((x, y, z)).or_default().push(index);
                        }
                    }
                }
            }
        }
        let n = area.triangles.len();
        self.local = area;
        Some(n)
    }

    /// Push a sphere out of nearby triangles. Returns the total push and the
    /// most upward push direction (to detect ground).
    pub fn push_sphere(&self, centre: Vec3, radius: f32) -> (Vec3, Option<Vec3>) {
        let (c0, c1) = (cell_of(centre - Vec3::splat(radius)), cell_of(centre + Vec3::splat(radius)));
        let mut push = Vec3::ZERO;
        let mut best_up: Option<Vec3> = None;
        let mut seen = std::collections::HashSet::new();
        for x in c0.0..=c1.0 {
            for y in c0.1..=c1.1 {
                for z in c0.2..=c1.2 {
                    for &i in self.local.grid.get(&(x, y, z)).into_iter().flatten() {
                        if !seen.insert(i) {
                            continue;
                        }
                        let t = &self.local.triangles[i as usize];
                        let p = centre + push;
                        let q = closest_point_on_triangle(p, t[0], t[1], t[2]);
                        let d = p - q;
                        let dist = d.length();
                        if dist < radius {
                            let n = if dist > 1e-5 { d / dist } else { (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero() };
                            push += n * (radius - dist);
                            if best_up.is_none_or(|b| n.y > b.y) {
                                best_up = Some(n);
                            }
                        }
                    }
                }
            }
        }
        (push, best_up)
    }
}

fn cell_of(p: Vec3) -> (i32, i32, i32) {
    ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32, (p.z / CELL).floor() as i32)
}

/// Closest point on triangle (a, b, c) to point p (Ericson, Real-Time Collision Detection, 5.1.5).
pub fn closest_point_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_point_cases() {
        let (a, b, c) = (Vec3::ZERO, Vec3::X, Vec3::Z);
        // Above the inside: straight down.
        assert!((closest_point_on_triangle(Vec3::new(0.2, 1.0, 0.2), a, b, c) - Vec3::new(0.2, 0.0, 0.2)).length() < 1e-6);
        // Beyond a corner.
        assert!((closest_point_on_triangle(Vec3::new(-1.0, 0.0, -1.0), a, b, c) - a).length() < 1e-6);
        // Beyond an edge.
        assert!((closest_point_on_triangle(Vec3::new(0.5, 0.0, -1.0), a, b, c) - Vec3::new(0.5, 0.0, 0.0)).length() < 1e-6);
    }

    #[test]
    fn sphere_on_floor_is_pushed_up() {
        let mut w = CollisionWorld::default();
        let m = w.add_mesh("floor", vec![Vec3::new(-10.0, 0.0, -10.0), Vec3::new(10.0, 0.0, -10.0), Vec3::new(0.0, 0.0, 10.0)], vec![[0, 2, 1]]);
        w.add_placement(m, Mat4::IDENTITY);
        w.update_area(Vec3::ZERO);
        let (push, up) = w.push_sphere(Vec3::new(0.0, 0.2, 0.0), 0.35);
        assert!((push - Vec3::new(0.0, 0.15, 0.0)).length() < 1e-4);
        assert!(up.unwrap().y > 0.99);
    }
}

impl CollisionWorld {
    /// Names of meshes with triangles closer than `radius` to a point (for logs).
    pub fn touching(&self, centre: Vec3, radius: f32) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        for (i, t) in self.local.triangles.iter().enumerate() {
            if (closest_point_on_triangle(centre, t[0], t[1], t[2]) - centre).length() < radius {
                out.insert(self.names[self.local.owner[i] as usize].clone());
            }
        }
        out.into_iter().collect()
    }
}
