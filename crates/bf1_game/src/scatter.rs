//! Terrain mesh scattering: grass, ivy, foliage and debris on the ground
//! (DESIGN.md "scattered grass and debris"). The level gives, per ground
//! layer, scatter types with a mesh, a density (per square metre at full
//! layer weight), a scale range and a draw distance (see `load::ScatterType`).
//!
//! The ground near the camera is cut into tiles (8 m; larger for types with a
//! long draw distance, see `tile_size`). Each tile's random
//! points depend only on the tile and the type, so a tile looks the same
//! every time it appears. A point is kept with the probability of its
//! layer's mask weight there; it gets the terrain height, a random turn and a
//! random scale. Tiles appear within the type's draw distance and go away
//! beyond it (a hard edge; the game fades them out). Grass blades without a
//! texture take the ground colour at the tile (the mean colour of the ground
//! textures there, by layer weight) times their tint.
//!
//! Kinds whose mesh is one flat card (backdrop trees) turn about the
//! vertical axis to face the camera (DESIGN.md "backdrop trees on the far
//! hills").
//!
//! Test switches: `BF1_SCATTER=off`, `BF1_SCATTER_DENSITY=<factor>`,
//! `BF1_SCATTER_DISTANCE=<factor>`, `BF1_BACKDROP_DISTANCE=<metres>`.

use crate::camera::FlyCam;
use crate::log::GameLog;
use crate::player::Ground;
use bevy::prelude::*;
use fb_terrain::layers::MAX_LAYERS;
use std::collections::HashMap;

/// Tile side (metres): 8 m, larger for types with a long draw distance
/// (about 12 tiles across the range, at most 256 m).
fn tile_size(distance: f32) -> f32 {
    (distance / 12.0).clamp(8.0, 256.0)
}
/// Cards turn to the camera again after it moves this far (metres).
const CARD_TURN_STEP: f32 = 10.0;
/// Most new tiles per frame (spreads the work when the camera moves).
const TILES_PER_FRAME: usize = 48;

pub struct ScatterKind {
    pub label: String,
    pub layer: usize,
    pub density: f32,
    pub min_scale: [f32; 2],
    pub max_scale: [f32; 2],
    pub distance: f32,
    /// The mesh's drawn parts with their materials.
    pub parts: Vec<(Handle<Mesh>, ScatterMaterial)>,
    /// Ground-coloured blades: tint (linear).
    pub ground_tint: Option<[f32; 3]>,
    /// One flat card: the turn of its face about the vertical axis in mesh
    /// space (radians). Such instances face the camera.
    pub card_facing: Option<f32>,
}

/// Material of a scatter part: plain, or shiny leaves (`foliage.rs`).
#[derive(Clone)]
pub enum ScatterMaterial {
    Standard(Handle<StandardMaterial>),
    Foliage(Handle<crate::foliage::FoliageMaterial>),
}

/// A scatter card that faces the camera; the value is `card_facing`.
#[derive(Component)]
pub struct FacingCard(f32);

/// Turn about the vertical axis that points a card's face at the camera.
fn card_yaw(cam: Vec3, at: Vec3, facing: f32) -> f32 {
    (cam.x - at.x).atan2(cam.z - at.z) - facing
}

#[derive(Resource)]
pub struct Scatter {
    pub kinds: Vec<ScatterKind>,
    /// Mean linear colour of each ground layer's texture.
    pub layer_colour: Vec<Option<[f32; 3]>>,
    density_factor: f32,
    distance_factor: f32,
    /// See `carry_on`.
    carry: bool,
    /// Scatter instances cast shadows (test switch `BF1_SCATTER_SHADOWS=off`).
    shadows: bool,
    /// Spawned tiles: (kind, tile x, tile z) -> (parent entity if any
    /// instances, instances).
    tiles: HashMap<(usize, i32, i32), (Option<Entity>, usize)>,
    /// Camera position at the last full update, and whether tiles were missing.
    last_update: Option<Vec3>,
    pending: bool,
    /// Ground-coloured materials by quantised colour.
    tinted: HashMap<(usize, [u8; 3]), Handle<StandardMaterial>>,
}

impl Scatter {
    pub fn new(kinds: Vec<ScatterKind>, layer_colour: Vec<Option<[f32; 3]>>) -> Scatter {
        let env = |n: &str| std::env::var(n).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        Scatter {
            kinds,
            layer_colour,
            density_factor: env("BF1_SCATTER_DENSITY"),
            distance_factor: env("BF1_SCATTER_DISTANCE"),
            carry: carry_on(),
            shadows: std::env::var("BF1_SCATTER_SHADOWS").map_or(true, |v| v != "off"),
            tiles: HashMap::new(),
            tinted: HashMap::new(),
            last_update: None,
            pending: true,
        }
    }
}

/// Small deterministic random numbers (SplitMix64).
struct Rng(u64);

impl Rng {
    fn new(kind: usize, tx: i32, tz: i32) -> Rng {
        Rng((kind as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ((tx as u32 as u64) << 32 | tz as u32 as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Whether scatter-only layers take their own mask from the finest patch
/// that lists them (DESIGN.md "layers carried from coarser levels", attempt
/// 2); `BF1_SCATTER_CARRY=off` uses the blended weight for all layers (old).
pub fn carry_on() -> bool {
    std::env::var("BF1_SCATTER_CARRY").map_or(true, |v| v != "off")
}

/// Scatter weight of `layer` at (x, z): for a layer with a ground texture the
/// blended weight (finest patch); for a scatter-only layer its own mask from
/// the finest patch that lists it. None outside the terrain.
pub fn scatter_weight(masks: &fb_terrain::layers::LayerMasks, x: f32, z: f32, layer: usize, textured: bool, carry: bool, w: &mut [f32; MAX_LAYERS]) -> Option<f32> {
    if textured || !carry {
        masks.weights_at(x, z, &[false; MAX_LAYERS], w)?;
        Some(w[layer])
    } else {
        masks.own_weight_at(x, z, layer as u8)
    }
}

/// Instance transforms for one tile of one kind.
#[allow(clippy::too_many_arguments)]
fn tile_points(kind_index: usize, kind: &ScatterKind, tx: i32, tz: i32, tile: f32, ground: &Ground, density_factor: f32, textured: bool, carry: bool) -> Vec<Transform> {
    let Some(masks) = ground.0.layers.as_ref() else { return vec![] };
    let mut rng = Rng::new(kind_index, tx, tz);
    let expected = kind.density * density_factor * tile * tile;
    let n = expected.floor() as usize + usize::from(rng.f() < expected.fract());
    let mut w = [0f32; MAX_LAYERS];
    let mut out = Vec::new();
    for _ in 0..n {
        let (x, z) = ((tx as f32 + rng.f()) * tile, (tz as f32 + rng.f()) * tile);
        let keep = rng.f();
        let (yaw, hs, vs) = (rng.f() * std::f32::consts::TAU, rng.f(), rng.f());
        if scatter_weight(masks, x, z, kind.layer, textured, carry, &mut w).is_none_or(|weight| keep >= weight) {
            continue;
        }
        let Some(y) = ground.0.height_at(x, z) else { continue };
        let h = kind.min_scale[0] + (kind.max_scale[0] - kind.min_scale[0]) * hs;
        let v = kind.min_scale[1] + (kind.max_scale[1] - kind.min_scale[1]) * vs;
        out.push(Transform { translation: Vec3::new(x, y, z), rotation: Quat::from_rotation_y(yaw), scale: Vec3::new(h, v, h) });
    }
    out
}

/// Ground colour at a point: the layer texture colours weighted by the masks.
fn ground_colour(scatter: &Scatter, ground: &Ground, x: f32, z: f32) -> Option<[f32; 3]> {
    let masks = ground.0.layers.as_ref()?;
    let mut w = [0f32; MAX_LAYERS];
    masks.weights_at(x, z, &[false; MAX_LAYERS], &mut w)?;
    let (mut sum, mut total) = ([0f32; 3], 0f32);
    for (l, &wl) in w.iter().enumerate() {
        if let (true, Some(Some(c))) = (wl > 0.0, scatter.layer_colour.get(l)) {
            for k in 0..3 {
                sum[k] += c[k] * wl;
            }
            total += wl;
        }
    }
    (total > 0.0).then(|| sum.map(|v| v / total))
}

/// Spawn and remove tiles around the camera; log the counts every 5 s.
#[allow(clippy::too_many_arguments)]
pub fn update_scatter(
    mut commands: Commands,
    scatter: Option<ResMut<Scatter>>,
    ground: Option<Res<Ground>>,
    cams: Query<&GlobalTransform, With<FlyCam>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    time: Res<Time>,
    log: Res<GameLog>,
    mut next_log: Local<f32>,
) {
    let (Some(mut scatter), Some(ground)) = (scatter, ground) else { return };
    let Ok(cam) = cams.single() else { return };
    let p = cam.translation();
    let scatter = &mut *scatter;
    // Work only when the camera moved or tiles are still missing.
    let moved = scatter.last_update.is_none_or(|q| q.distance(p) > 1.0);
    if !moved && !scatter.pending {
        log_counts(scatter, &time, &log, &mut next_log);
        return;
    }
    scatter.last_update = Some(p);
    scatter.pending = false;
    let mut budget = TILES_PER_FRAME;
    for k in 0..scatter.kinds.len() {
        let range = scatter.kinds[k].distance * scatter.distance_factor;
        let tile = tile_size(range);
        let half_diagonal = tile * std::f32::consts::FRAC_1_SQRT_2;
        // Remove tiles that are now too far (a little hysteresis).
        let far: Vec<(usize, i32, i32)> = scatter
            .tiles
            .keys()
            .filter(|(kk, tx, tz)| *kk == k && Vec2::new((*tx as f32 + 0.5) * tile - p.x, (*tz as f32 + 0.5) * tile - p.z).length() > range + half_diagonal + tile)
            .copied()
            .collect();
        for key in far {
            if let Some((Some(e), _)) = scatter.tiles.remove(&key) {
                commands.entity(e).despawn();
            }
        }
        // Add the missing tiles within range, nearest first.
        let (t0x, t1x) = (((p.x - range) / tile).floor() as i32, ((p.x + range) / tile).floor() as i32);
        let (t0z, t1z) = (((p.z - range) / tile).floor() as i32, ((p.z + range) / tile).floor() as i32);
        let mut wanted: Vec<(f32, i32, i32)> = Vec::new();
        for tz in t0z..=t1z {
            for tx in t0x..=t1x {
                let d = Vec2::new((tx as f32 + 0.5) * tile - p.x, (tz as f32 + 0.5) * tile - p.z).length();
                if d <= range + half_diagonal && !scatter.tiles.contains_key(&(k, tx, tz)) {
                    wanted.push((d, tx, tz));
                }
            }
        }
        // No tiles that lie fully outside the terrain.
        if let Some(m) = ground.0.layers.as_ref() {
            let (lo, hi) = (Vec2::from(m.min), Vec2::from(m.min) + m.width);
            wanted.retain(|&(_, tx, tz)| {
                let (a, b) = (Vec2::new(tx as f32, tz as f32) * tile, Vec2::new(tx as f32 + 1.0, tz as f32 + 1.0) * tile);
                b.x > lo.x && b.y > lo.y && a.x < hi.x && a.y < hi.y
            });
        }
        wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, tx, tz) in wanted {
            if budget == 0 {
                scatter.pending = true;
                break;
            }
            budget -= 1;
            let textured = scatter.layer_colour.get(scatter.kinds[k].layer).is_some_and(Option::is_some);
            let points = tile_points(k, &scatter.kinds[k], tx, tz, tile, &ground, scatter.density_factor, textured, scatter.carry);
            if points.is_empty() {
                scatter.tiles.insert((k, tx, tz), (None, 0));
                continue;
            }
            // Ground-coloured blades: one material per tile colour (quantised).
            let tint_material = match scatter.kinds[k].ground_tint {
                Some(tint) if !points.is_empty() => {
                    let c = ground_colour(scatter, &ground, (tx as f32 + 0.5) * tile, (tz as f32 + 0.5) * tile).unwrap_or([0.1, 0.1, 0.05]);
                    let c: [f32; 3] = std::array::from_fn(|i| c[i] * tint[i]);
                    let key = (k, c.map(|v| (v.clamp(0.0, 1.0) * 63.0).round() as u8));
                    Some(
                        scatter
                            .tinted
                            .entry(key)
                            .or_insert_with(|| {
                                materials.add(StandardMaterial {
                                    base_color: Color::linear_rgb(c[0], c[1], c[2]),
                                    perceptual_roughness: 0.9,
                                    cull_mode: None,
                                    double_sided: true,
                                    ..default()
                                })
                            })
                            .clone(),
                    )
                }
                _ => None,
            };
            let parent = commands.spawn((Transform::IDENTITY, Visibility::default())).id();
            let kind = &scatter.kinds[k];
            commands.entity(parent).with_children(|c| {
                for t in &points {
                    for (mesh, material) in &kind.parts {
                        let material = tint_material.clone().map(ScatterMaterial::Standard).unwrap_or_else(|| material.clone());
                        let t = match kind.card_facing {
                            Some(f) => Transform { rotation: Quat::from_rotation_y(card_yaw(p, t.translation, f)), ..*t },
                            None => *t,
                        };
                        let mut e = c.spawn((Mesh3d(mesh.clone()), t));
                        match material {
                            ScatterMaterial::Standard(h) => e.insert(MeshMaterial3d(h)),
                            ScatterMaterial::Foliage(h) => e.insert(MeshMaterial3d(h)),
                        };
                        if let Some(f) = kind.card_facing {
                            e.insert(FacingCard(f));
                        }
                        if !scatter.shadows {
                            e.insert(bevy::light::NotShadowCaster);
                        }
                    }
                }
            });
            scatter.tiles.insert((k, tx, tz), (Some(parent), points.len()));
        }
    }
    log_counts(scatter, &time, &log, &mut next_log);
}

/// Turn the cards to the camera when it has moved `CARD_TURN_STEP` (new
/// cards are turned when they appear).
pub fn face_cards(cams: Query<&GlobalTransform, With<FlyCam>>, mut cards: Query<(&mut Transform, &FacingCard)>, mut last: Local<Option<Vec3>>) {
    let Ok(cam) = cams.single() else { return };
    let p = cam.translation();
    if last.is_some_and(|q| q.distance(p) < CARD_TURN_STEP) {
        return;
    }
    *last = Some(p);
    for (mut t, card) in &mut cards {
        t.rotation = Quat::from_rotation_y(card_yaw(p, t.translation, card.0));
    }
}

/// Every 5 s: tiles and instances per kind.
fn log_counts(scatter: &Scatter, time: &Time, log: &GameLog, next_log: &mut f32) {
    if time.elapsed_secs() >= *next_log {
        *next_log = time.elapsed_secs() + 5.0;
        let mut per_kind = vec![0usize; scatter.kinds.len()];
        for ((k, _, _), (_, n)) in &scatter.tiles {
            per_kind[*k] += n;
        }
        let total: usize = per_kind.iter().sum();
        let list: Vec<String> = per_kind.iter().enumerate().filter(|(_, n)| **n > 0).map(|(k, n)| format!("{} {n}", scatter.kinds[k].label)).collect();
        let with_entity = scatter.tiles.values().filter(|t| t.0.is_some()).count();
        log.line(format!("scatter: {} tiles ({with_entity} not empty), {total} instances ({})", scatter.tiles.len(), list.join(", ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_numbers_repeat_per_tile_and_stay_in_range() {
        let (mut a, mut b, mut c) = (Rng::new(3, 10, -4), Rng::new(3, 10, -4), Rng::new(3, 10, -5));
        let (va, vb, vc): (Vec<f32>, Vec<f32>, Vec<f32>) = ((0..100).map(|_| a.f()).collect(), (0..100).map(|_| b.f()).collect(), (0..100).map(|_| c.f()).collect());
        assert_eq!(va, vb);
        assert_ne!(va, vc);
        assert!(va.iter().all(|v| (0.0..1.0).contains(v)));
        let mean = va.iter().sum::<f32>() / 100.0;
        assert!((0.35..0.65).contains(&mean), "{mean}");
    }

    #[test]
    fn cards_face_the_camera() {
        // A card whose face points along mesh +X, turned toward cameras on
        // all four sides: its face must point at the camera.
        let facing = std::f32::consts::FRAC_PI_2;
        let at = Vec3::new(10.0, 0.0, -5.0);
        for cam in [Vec3::new(10.0, 2.0, 50.0), Vec3::new(-40.0, 2.0, -5.0), Vec3::new(10.0, 2.0, -90.0), Vec3::new(60.0, 2.0, -5.0)] {
            let face = Quat::from_rotation_y(card_yaw(cam, at, facing)) * Vec3::X;
            let to_cam = Vec3::new(cam.x - at.x, 0.0, cam.z - at.z).normalize();
            assert!(face.dot(to_cam) > 0.999, "{cam}: {face}");
        }
    }
}
