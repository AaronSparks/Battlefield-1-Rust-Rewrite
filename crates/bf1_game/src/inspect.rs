//! Inspect key (DESIGN.md "position overlay and inspect key"): I casts a ray
//! from the screen centre and reports what it hits, with the camera position
//! as a ready-to-use `BF1_CAMERA` line. The result goes to the log and to the
//! overlay. `BF1_INSPECT_AT=<seconds>` runs it once without a key (tests).
//!
//! Data: each unique mesh's parts in mesh space, and per placement its
//! matrix, inverse and world box. The ray is tested against the boxes first,
//! then against the triangles in mesh space (no world copies).

use crate::camera::FlyCam;
use crate::load::Loaded;
use crate::log::GameLog;
use crate::player::Ground;
use bevy::prelude::*;
use fb_terrain::layers::MAX_LAYERS;

/// Longest inspect ray (metres).
const RANGE: f32 = 500.0;

struct InspectPart {
    label: String,
    cutout: bool,
    positions: Vec<Vec3>,
    tris: Vec<[u32; 3]>,
}

struct InspectPlacement {
    mesh: usize,
    inverse: Mat4,
    min: Vec3,
    max: Vec3,
}

#[derive(Resource)]
pub struct InspectIndex {
    meshes: Vec<Vec<InspectPart>>,
    placements: Vec<InspectPlacement>,
    /// Ground texture name of each terrain layer.
    layer_names: Vec<Option<String>>,
}

/// The last inspect result, for the overlay.
#[derive(Resource, Default)]
pub struct InspectResult {
    pub text: String,
    /// Time (seconds since start) when it was made.
    pub at: f32,
}

fn short(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Collect the mesh parts and placements (before the level's meshes go to the GPU).
pub fn build(level: &Loaded) -> InspectIndex {
    let meshes = level
        .meshes
        .iter()
        .map(|m| {
            m.parts
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let tex = match (p.texture, p.colour, p.terrain_coloured) {
                        (Some(t), _, _) => short(&level.textures[t].name).to_string(),
                        (None, Some(c), _) => format!("plain colour {c:.2?}"),
                        (None, None, true) => "terrain colour".into(),
                        _ => format!("no texture: {}", p.missing.as_deref().unwrap_or("?")),
                    };
                    let cutout = p.texture.is_some_and(|t| level.textures[t].cutout);
                    let mut extra = Vec::new();
                    if cutout {
                        extra.push("cut-out".to_string());
                    }
                    if let Some(t) = &p.tint {
                        extra.push(format!("{} tint {:.2?}", t.kind, t.colour));
                    }
                    if let Some(d) = &p.detail {
                        extra.push(format!("detail {}", short(&level.textures[d.texture].name)));
                    }
                    let extra = if extra.is_empty() { String::new() } else { format!("; {}", extra.join("; ")) };
                    InspectPart {
                        label: format!("{} part {i} ({tex}{extra})", short(&m.name)),
                        cutout,
                        positions: p.positions.iter().map(|v| Vec3::from(*v)).collect(),
                        tris: p.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect(),
                    }
                })
                .collect()
        })
        .collect();
    let placements = level
        .placements
        .iter()
        .filter_map(|(mesh, matrix)| {
            let m = Mat4::from_cols_array(matrix);
            let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for p in &level.meshes[*mesh].parts {
                for v in &p.positions {
                    let w = m.transform_point3(Vec3::from(*v));
                    min = min.min(w);
                    max = max.max(w);
                }
            }
            (min.x <= max.x).then(|| InspectPlacement { mesh: *mesh, inverse: m.inverse(), min, max })
        })
        .collect();
    let mut layer_names = vec![None; MAX_LAYERS];
    for g in &level.ground {
        for &l in &g.layers {
            if let Some(slot) = layer_names.get_mut(l as usize) {
                *slot = Some(short(&g.name).to_string());
            }
        }
    }
    InspectIndex { meshes, placements, layer_names }
}

/// Distance along the ray to a triangle (both sides), Moller-Trumbore. The
/// direction need not be unit length; the result is in its units.
fn hit_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let (e1, e2) = (b - a, c - a);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// Entry distance of the ray into a box, if it hits it.
fn hit_box(o: Vec3, inv_d: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let t1 = (min - o) * inv_d;
    let t2 = (max - o) * inv_d;
    let near = t1.min(t2).max_element();
    let far = t1.max(t2).min_element();
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

impl InspectIndex {
    /// Mesh hits along the ray, nearest first: (distance, label, cut-out).
    /// Only the nearest hit per part.
    fn mesh_hits(&self, o: Vec3, d: Vec3, max: f32) -> Vec<(f32, &str, bool)> {
        let inv_d = d.recip();
        let mut hits: Vec<(f32, &str, bool)> = Vec::new();
        for pl in &self.placements {
            if hit_box(o, inv_d, pl.min, pl.max).is_none_or(|t| t > max) {
                continue;
            }
            // Ray in mesh space; the parameter t stays in world metres.
            let (lo, ld) = (pl.inverse.transform_point3(o), pl.inverse.transform_vector3(d));
            for part in &self.meshes[pl.mesh] {
                let best = part
                    .tris
                    .iter()
                    .filter_map(|t| hit_triangle(lo, ld, part.positions[t[0] as usize], part.positions[t[1] as usize], part.positions[t[2] as usize]))
                    .fold(f32::MAX, f32::min);
                if best <= max {
                    hits.push((best, part.label.as_str(), part.cutout));
                }
            }
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        hits
    }
}

/// Terrain layers at a point, strongest first, as text.
fn ground_layers(ground: &Ground, names: &[Option<String>], x: f32, z: f32) -> String {
    let Some(masks) = ground.0.layers.as_ref() else { return "no layer masks".into() };
    let mut w = [0f32; MAX_LAYERS];
    if masks.weights_at(x, z, &[false; MAX_LAYERS], &mut w).is_none() {
        return "outside the layer masks".into();
    }
    let mut list: Vec<(usize, f32)> = w.iter().copied().enumerate().filter(|(_, v)| *v > 0.01).collect();
    list.sort_by(|a, b| b.1.total_cmp(&a.1));
    list.iter()
        .take(4)
        .map(|(l, v)| format!("layer {l} {} {:.0}%", names.get(*l).cloned().flatten().unwrap_or_else(|| "(mesh scattering only)".into()), v * 100.0))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The camera as a `BF1_CAMERA` value.
pub fn camera_line(p: Vec3, cam: &FlyCam) -> String {
    format!("BF1_CAMERA=\"{:.1},{:.1},{:.1},{:.0},{:.0}\"", p.x, p.y, p.z, cam.yaw.to_degrees(), cam.pitch.to_degrees())
}

/// Inspect what is in the screen centre: on key I, or once at `BF1_INSPECT_AT`.
#[allow(clippy::too_many_arguments)]
pub fn inspect_key(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    index: Option<Res<InspectIndex>>,
    ground: Option<Res<Ground>>,
    cams: Query<(&GlobalTransform, &FlyCam)>,
    log: Res<GameLog>,
    mut result: ResMut<InspectResult>,
    mut auto_done: Local<bool>,
) {
    let auto = !*auto_done && std::env::var("BF1_INSPECT_AT").ok().and_then(|v| v.parse::<f32>().ok()).is_some_and(|t| time.elapsed_secs() >= t);
    if !keys.just_pressed(KeyCode::KeyI) && !auto {
        return;
    }
    if auto {
        *auto_done = true;
    }
    let (Some(index), Some(ground)) = (index, ground) else { return };
    let Ok((gt, cam)) = cams.single() else { return };
    let (o, d) = (gt.translation(), *gt.forward());
    let terrain = crate::view_probe::hit_terrain(&ground, o, d, RANGE);
    let max = terrain.unwrap_or(RANGE);
    let hits = index.mesh_hits(o, d, max);
    let describe_terrain = |t: f32| {
        let p = o + d * t;
        format!("terrain at {t:.1} m, point ({:.1}, {:.1}, {:.1}); ground: {}", p.x, p.y, p.z, ground_layers(&ground, &index.layer_names, p.x, p.z))
    };
    let mut what = match hits.first() {
        Some((t, label, _)) => {
            let p = o + d * *t;
            format!("{label} at {t:.1} m, point ({:.1}, {:.1}, {:.1})", p.x, p.y, p.z)
        }
        None => match terrain {
            Some(t) => describe_terrain(t),
            None => format!("nothing within {RANGE:.0} m (sky)"),
        },
    };
    // A leaf card first: also say what solid thing is behind it.
    if hits.first().is_some_and(|h| h.2) {
        let behind = match hits.iter().find(|h| !h.2) {
            Some((t, label, _)) => format!("{label} at {t:.1} m"),
            None => terrain.map(describe_terrain).unwrap_or_else(|| "sky".into()),
        };
        what = format!("{what}; behind it: {behind}");
    }
    let text = format!("{} | {what}", camera_line(o, cam));
    log.line(format!("inspect: {text}"));
    *result = InspectResult { text, at: time.elapsed_secs() };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangle_and_box_hits() {
        let (a, b, c) = (Vec3::new(-1.0, -1.0, 5.0), Vec3::new(1.0, -1.0, 5.0), Vec3::new(0.0, 1.0, 5.0));
        // Straight ahead along +Z hits at 5; a scaled direction gives a scaled parameter.
        assert!((hit_triangle(Vec3::ZERO, Vec3::Z, a, b, c).unwrap() - 5.0).abs() < 1e-5);
        assert!((hit_triangle(Vec3::ZERO, Vec3::Z * 2.0, a, b, c).unwrap() - 2.5).abs() < 1e-5);
        assert!(hit_triangle(Vec3::ZERO, Vec3::NEG_Z, a, b, c).is_none());
        let inv = Vec3::new(1.0, 1.0, 1.0) / Vec3::new(1e-9, 1e-9, 1.0);
        assert!(hit_box(Vec3::ZERO, inv, Vec3::new(-1.0, -1.0, 3.0), Vec3::new(1.0, 1.0, 4.0)).is_some());
        assert!(hit_box(Vec3::ZERO, inv, Vec3::new(2.0, 2.0, 3.0), Vec3::new(3.0, 3.0, 4.0)).is_none());
    }

    #[test]
    fn mesh_space_ray_keeps_world_distance() {
        // A placement scaled 2x and moved to z = 10: the triangle at local z = 1 is at world z = 12.
        let m = Mat4::from_scale_rotation_translation(Vec3::splat(2.0), Quat::IDENTITY, Vec3::new(0.0, 0.0, 10.0));
        let inv = m.inverse();
        let (lo, ld) = (inv.transform_point3(Vec3::ZERO), inv.transform_vector3(Vec3::Z));
        let t = hit_triangle(lo, ld, Vec3::new(-1.0, -1.0, 1.0), Vec3::new(1.0, -1.0, 1.0), Vec3::new(0.0, 1.0, 1.0)).unwrap();
        assert!((t - 12.0).abs() < 1e-4, "{t}");
    }
}
