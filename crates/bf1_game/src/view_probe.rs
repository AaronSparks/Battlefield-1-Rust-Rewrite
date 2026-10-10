//! Test tool ("log, don't look"): what the camera sees in parts of the screen.
//!
//! With `BF1_PROBE_RECTS=x0,y0,x1,y1;...` (fractions of the window, like
//! `bf1_probe image-stats --rect`) and `BF1_SCREENSHOT_AT=<seconds>`, a grid of
//! rays (`BF1_PROBE_GRID` per side, default 6) goes from the camera through
//! each region when the screenshot is taken. Each ray reports the first thing
//! it hits: the terrain or a mesh (by name; "cut-out" marks leaf cards, whose
//! clear pixels a ray cannot see). The log lists the share of rays per thing
//! and the mean distance. When the screenshot arrives, `log_colours` also
//! gives the mean screenshot colour of each thing (pixels where its rays hit).

use crate::camera::FlyCam;
use crate::load::Loaded;
use crate::log::GameLog;
use crate::player::Ground;
use bevy::prelude::*;
use std::collections::BTreeMap;

/// Meshes closer than this to the camera start are tested (metres).
const RADIUS: f32 = 60.0;
/// Longest ray (metres).
const RANGE: f32 = 300.0;

/// Triangles of one placed part, with their bounding box.
struct Group {
    name: u32,
    min: Vec3,
    max: Vec3,
    tris: Vec<[Vec3; 3]>,
}

#[derive(Resource)]
pub struct ViewProbe {
    rects: Vec<[f32; 4]>,
    grid: usize,
    names: Vec<String>,
    groups: Vec<Group>,
}

/// What each probe ray hit: (thing, screen x and y as fractions).
#[derive(Resource, Default)]
pub struct ProbeHits(pub Vec<(String, f32, f32)>);

fn rects_from_env() -> Option<Vec<[f32; 4]>> {
    let v = std::env::var("BF1_PROBE_RECTS").ok()?;
    let rects: Vec<[f32; 4]> = v
        .split(';')
        .filter_map(|r| {
            let n: Vec<f32> = r.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (n.len() == 4).then(|| [n[0], n[1], n[2], n[3]])
        })
        .collect();
    (!rects.is_empty()).then_some(rects)
}

/// Collect the triangles near the camera start (`BF1_CAMERA`, else the battle
/// area centre); only if `BF1_PROBE_RECTS` is set.
pub fn build(level: &Loaded, log: &GameLog) -> Option<ViewProbe> {
    let rects = rects_from_env()?;
    let grid = std::env::var("BF1_PROBE_GRID").ok().and_then(|v| v.parse().ok()).unwrap_or(6usize).clamp(1, 200);
    let cam: Vec<f32> = std::env::var("BF1_CAMERA").ok().map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default();
    let start = if cam.len() >= 3 { Vec3::new(cam[0], cam[1], cam[2]) } else { Vec3::from(level.centre) };
    let mut names = Vec::new();
    let mut groups = Vec::new();
    let mut name_index: BTreeMap<String, u32> = BTreeMap::new();
    for (mesh_index, matrix) in &level.placements {
        let m = Mat4::from_cols_array(matrix);
        let mesh = &level.meshes[*mesh_index];
        // Bounding sphere test first (mesh space radius, scaled).
        let radius = mesh.parts.iter().flat_map(|p| p.positions.iter()).map(|v| Vec3::from(*v).length()).fold(0.0f32, f32::max);
        let scale = m.x_axis.truncate().length().max(m.y_axis.truncate().length()).max(m.z_axis.truncate().length());
        if m.transform_point3(Vec3::ZERO).distance(start) > RADIUS + radius * scale {
            continue;
        }
        for (pi, p) in mesh.parts.iter().enumerate() {
            let cut = p.texture.is_some_and(|t| level.textures[t].cutout);
            let short = mesh.name.rsplit('/').next().unwrap_or(&mesh.name);
            let tex = p.texture.map(|t| level.textures[t].name.rsplit('/').next().unwrap_or("").to_string()).unwrap_or_else(|| "no texture".into());
            let label = format!("{short} part {pi} ({tex}{})", if cut { ", cut-out" } else { "" });
            let id = *name_index.entry(label.clone()).or_insert_with(|| {
                names.push(label);
                names.len() as u32 - 1
            });
            let tris: Vec<[Vec3; 3]> = p
                .indices
                .chunks_exact(3)
                .map(|t| {
                    let v = |i: u32| m.transform_point3(Vec3::from(p.positions[i as usize]));
                    [v(t[0]), v(t[1]), v(t[2])]
                })
                .collect();
            let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for v in tris.iter().flatten() {
                min = min.min(*v);
                max = max.max(*v);
            }
            if !tris.is_empty() {
                groups.push(Group { name: id, min, max, tris });
            }
        }
    }
    let count: usize = groups.iter().map(|g| g.tris.len()).sum();
    log.line(format!("view probe: {} regions, grid {grid}, {count} triangles in {} placed parts ({} names) within {RADIUS} m", rects.len(), groups.len(), names.len()));
    Some(ViewProbe { rects, grid, names, groups })
}

/// Distance along the ray to a triangle (both sides), Moller-Trumbore.
fn hit_triangle(o: Vec3, d: Vec3, t: &[Vec3; 3]) -> Option<f32> {
    let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - t[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = e2.dot(q) * inv;
    (dist > 0.0).then_some(dist)
}

/// Entry distance of the ray into a box, if it hits it (slab test).
fn hit_box(o: Vec3, inv_d: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let t1 = (min - o) * inv_d;
    let t2 = (max - o) * inv_d;
    let near = t1.min(t2).max_element();
    let far = t1.max(t2).min_element();
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

/// Distance along the ray to the terrain (steps of 5 cm growing with the
/// distance, then halving), up to `range` metres.
pub(crate) fn hit_terrain(ground: &Ground, o: Vec3, d: Vec3, range: f32) -> Option<f32> {
    let below = |t: f32| {
        let p = o + d * t;
        ground.height(p).map(|h| p.y < h)
    };
    let mut prev = 0.0;
    let mut t = 0.05;
    while t < range {
        if below(t) == Some(true) {
            let (mut lo, mut hi) = (prev, t);
            for _ in 0..12 {
                let mid = 0.5 * (lo + hi);
                if below(mid) == Some(true) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            return Some(hi);
        }
        prev = t;
        t += 0.05 + t * 0.01;
    }
    None
}

/// Log what each region shows, once, at `BF1_SCREENSHOT_AT`.
pub fn log_view(mut commands: Commands, probe: Option<Res<ViewProbe>>, ground: Option<Res<Ground>>, cams: Query<(&Camera, &GlobalTransform), With<FlyCam>>, time: Res<Time>, log: Res<GameLog>, mut done: Local<bool>) {
    let (Some(probe), Some(ground)) = (probe, ground) else { return };
    let Some(at) = std::env::var("BF1_SCREENSHOT_AT").ok().and_then(|s| s.parse::<f32>().ok()) else { return };
    if *done || time.elapsed_secs() < at {
        return;
    }
    *done = true;
    let Ok((camera, gt)) = cams.single() else { return };
    let Some(size) = camera.logical_viewport_size() else { return };
    let started = std::time::Instant::now();
    let grid = probe.grid;
    let mut hits = ProbeHits::default();
    for r in &probe.rects {
        // Per thing: (rays, distance sum).
        let mut seen: BTreeMap<String, (usize, f32)> = BTreeMap::new();
        for j in 0..grid {
            for i in 0..grid {
                let fx = r[0] + (r[2] - r[0]) * (i as f32 + 0.5) / grid as f32;
                let fy = r[1] + (r[3] - r[1]) * (j as f32 + 0.5) / grid as f32;
                let Ok(ray) = camera.viewport_to_world(gt, Vec2::new(fx * size.x, fy * size.y)) else { continue };
                let (o, d) = (ray.origin, *ray.direction);
                let inv_d = d.recip();
                let mut best: Option<(f32, u32)> = hit_terrain(&ground, o, d, RANGE).map(|t| (t, u32::MAX));
                for g in &probe.groups {
                    let Some(enter) = hit_box(o, inv_d, g.min, g.max) else { continue };
                    if best.is_some_and(|b| enter > b.0) {
                        continue;
                    }
                    for tri in &g.tris {
                        if let Some(t) = hit_triangle(o, d, tri) {
                            if best.is_none_or(|b| t < b.0) {
                                best = Some((t, g.name));
                            }
                        }
                    }
                }
                let (t, what) = match best {
                    Some((t, u32::MAX)) => (t, "terrain".to_string()),
                    Some((t, id)) => (t, probe.names[id as usize].clone()),
                    None => (RANGE, "nothing (sky or far)".into()),
                };
                hits.0.push((what.clone(), fx, fy));
                let e = seen.entry(what).or_default();
                e.0 += 1;
                e.1 += t;
            }
        }
        let mut list: Vec<(String, (usize, f32))> = seen.into_iter().collect();
        list.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        let total = (grid * grid) as f32;
        let parts: Vec<String> = list.iter().take(12).map(|(n, (c, d))| format!("{n} {:.0}% at {:.1} m", 100.0 * *c as f32 / total, d / *c as f32)).collect();
        log.line(format!("view probe {r:?}: {}", parts.join(", ")));
    }
    log.line(format!("view probe: {} rays in {:.2} s", hits.0.len(), started.elapsed().as_secs_f64()));
    commands.insert_resource(hits);
}

/// Mean screenshot colour (sRGB bytes, red-green-blue) of each thing the probe
/// rays hit, from the pixels at the ray positions. `rgba` is the screenshot in
/// its own channel order; `bgr` says the first byte is blue.
pub fn log_colours(hits: &ProbeHits, w: usize, h: usize, data: &[u8], bgr: bool, log: &GameLog) {
    let mut samples: BTreeMap<&str, Vec<[f64; 3]>> = BTreeMap::new();
    for (what, fx, fy) in &hits.0 {
        let x = ((fx * w as f32) as usize).min(w - 1);
        let y = ((fy * h as f32) as usize).min(h - 1);
        let i = (y * w + x) * 4;
        let Some(px) = data.get(i..i + 3) else { continue };
        let rgb = if bgr { [px[2], px[1], px[0]] } else { [px[0], px[1], px[2]] };
        samples.entry(what.as_str()).or_default().push(rgb.map(|v| v as f64));
    }
    let mut list: Vec<_> = samples.into_iter().collect();
    list.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let mean = |v: &[[f64; 3]]| {
        let n = v.len().max(1) as f64;
        let mut m = [0f64; 3];
        for p in v {
            for c in 0..3 {
                m[c] += p[c] / n;
            }
        }
        format!("[{:.0}, {:.0}, {:.0}]", m[0], m[1], m[2])
    };
    for (what, mut v) in list.into_iter().take(25) {
        // Brightest quarter by luminance (the sunlit side), as in `bf1_probe image-stats`.
        let all = mean(&v);
        v.sort_by(|a, b| (0.2126 * a[0] + 0.7152 * a[1] + 0.0722 * a[2]).total_cmp(&(0.2126 * b[0] + 0.7152 * b[1] + 0.0722 * b[2])));
        let q = (v.len() / 4).max(1);
        log.line(format!("view colour: {what}: {} rays, mean {all}, bright quarter {}, dark quarter {}", v.len(), mean(&v[v.len() - q..]), mean(&v[..q])));
    }
}
