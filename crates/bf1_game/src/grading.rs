//! Colour grading with the level's own tables (step 1 of the "match the
//! reference look" plan in docs/DESIGN.md).
//!
//! Bevy's TonyMcMapface tone mapping reads a 48^3 table (scene light ->
//! display colour). Cell c of an axis stands for the scene light value
//! e / (1 - e), e = c / 47 (Bevy samples at x / (x + 1)). We keep a copy of
//! that table and write a combined one into the same image, so the existing
//! tone mapping pass does the grading too; no extra render pass. The table
//! is rebuilt when the weather changes.
//!
//! Modes (`BF1_GRADING`; docs/DESIGN.md "colour grading fix"):
//! - `display`: tone map, then the weather table, then the plain cube, both
//!   on sRGB screen colours (the first way; crushes dark tones);
//! - `weather`, `plain`: one of them, on screen colours;
//! - `pq`: the weather table on scene light in the PQ encoding (scene value
//!   1.0 = `BF1_GRADING_NITS` nits), then tone map, then the plain cube on
//!   screen colours;
//! - `pq-only`: the same without the plain cube (the default, at 100 nits:
//!   it fits the user's sunny and fire rain screenshots best; MODLOG
//!   2026-10-07);
//! - `off`.

use crate::log::GameLog;
use crate::scene::Env;
use bevy::core_pipeline::tonemapping::TonemappingLuts;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;

#[derive(Resource, Default)]
pub struct GradingState {
    /// Bevy's own table (display-linear RGB), kept to rebuild from.
    original: Option<Vec<[f32; 3]>>,
    /// Weather index whose grading is in the table.
    applied: Option<usize>,
    /// Set when the table cannot be used (format we cannot write).
    failed: bool,
}

pub fn apply_grading(env: Res<Env>, luts: Option<Res<TonemappingLuts>>, mut images: ResMut<Assets<Image>>, mut state: ResMut<GradingState>, log: Res<GameLog>) {
    if state.failed || state.applied == Some(env.current) || env.list.is_empty() {
        return;
    }
    let Some(luts) = luts else { return };
    let Some(mut image) = images.get_mut(&luts.tony_mc_mapface) else { return };
    let format = image.texture_descriptor.format;
    let size = image.texture_descriptor.size;
    let n = (size.width * size.height * size.depth_or_array_layers) as usize;
    let Some(data) = image.data.as_mut() else {
        log.line("grading: tone mapping table has no data in main memory; grading off");
        state.failed = true;
        return;
    };
    if state.original.is_none() {
        let decoded: Option<Vec<[f32; 3]>> = match format {
            TextureFormat::Rgb9e5Ufloat if data.len() >= n * 4 => Some((0..n).map(|i| rgb9e5_decode(u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()))).collect()),
            _ => None,
        };
        let Some(decoded) = decoded else {
            log.line(format!("grading: tone mapping table format {format:?} ({} bytes) not supported; grading off", data.len()));
            state.failed = true;
            return;
        };
        log.line(format!("grading: tone mapping table {}x{}x{} {format:?}", size.width, size.height, size.depth_or_array_layers));
        state.original = Some(decoded);
    }
    let mode = std::env::var("BF1_GRADING").unwrap_or_else(|_| DEFAULT_MODE.into()).to_lowercase();
    let nits = std::env::var("BF1_GRADING_NITS").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(DEFAULT_NITS);
    let e = &env.list[env.current];
    let (weather, plain) = (e.grading[0].as_ref(), e.grading[1].as_ref());
    let pq_mode = mode.starts_with("pq");
    // Tables used on screen colours, in order.
    let screen: Vec<&crate::environment::GradingTable> = match mode.as_str() {
        "off" | "pq-only" => vec![],
        "weather" => weather.into_iter().collect(),
        "plain" | "pq" => plain.into_iter().collect(),
        _ => weather.into_iter().chain(plain).collect(),
    };
    let original = state.original.as_ref().unwrap();
    let d = size.width as usize;
    let mut probe = Vec::new();
    for (i, &lin) in original.iter().enumerate() {
        let lin = match (pq_mode, weather) {
            (true, Some(w)) => {
                // Scene light of this cell, graded in PQ, then tone mapped by Bevy's own table.
                let cell = [i % d, (i / d) % d, i / (d * d)];
                let x = cell.map(|c| cell_light(c, d));
                let graded = w.apply(x.map(|v| pq_encode(v * nits / 10_000.0))).map(|q| pq_decode(q) * 10_000.0 / nits);
                sample_table(original, d, graded)
            }
            _ => lin,
        };
        let mut c = lin.map(linear_to_srgb);
        for t in &screen {
            c = t.apply(c);
        }
        let out = c.map(srgb_to_linear);
        data[i * 4..i * 4 + 4].copy_from_slice(&rgb9e5_encode(out).to_le_bytes());
        // Grey diagonal of the 48^3 table, for the log.
        if i % (d * d + d + 1) == 0 && probe.len() < 8 {
            let before = original[i];
            probe.push(format!("{:?}->{:?}", before.map(|v| (linear_to_srgb(v) * 255.0).round() as u8), c.map(|v| (v * 255.0).round() as u8)));
        }
    }
    log.line(format!(
        "grading for {} (BF1_GRADING={mode}{}): weather table {}, {} table(s) on screen colours; grey steps (sRGB bytes, tone map only -> graded): {}",
        e.name,
        if pq_mode { format!(", {nits} nits per scene unit") } else { String::new() },
        if pq_mode && weather.is_some() { "on scene light (PQ)" } else { "not on scene light" },
        screen.len(),
        probe.join(" ")
    ));
    state.applied = Some(env.current);
}

/// Default mode and PQ scale (see the module comment and MODLOG).
const DEFAULT_MODE: &str = "pq-only";
const DEFAULT_NITS: f32 = 100.0;

/// Scene light value that cell `c` of a `d`-cell axis of Bevy's tone map
/// table stands for (the table is sampled at x / (x + 1)).
fn cell_light(c: usize, d: usize) -> f32 {
    let e = c as f32 / (d - 1) as f32;
    if e >= 1.0 { 1.0e4 } else { e / (1.0 - e) }
}

/// Trilinear lookup in Bevy's tone map table at scene light `x`.
fn sample_table(table: &[[f32; 3]], d: usize, x: [f32; 3]) -> [f32; 3] {
    let pos = x.map(|v| (v.max(0.0) / (v.max(0.0) + 1.0)) * (d - 1) as f32);
    let i0 = pos.map(|p| (p as usize).min(d - 2));
    let f = [pos[0] - i0[0] as f32, pos[1] - i0[1] as f32, pos[2] - i0[2] as f32];
    let mut out = [0f32; 3];
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let w = (if dx == 1 { f[0] } else { 1.0 - f[0] }) * (if dy == 1 { f[1] } else { 1.0 - f[1] }) * (if dz == 1 { f[2] } else { 1.0 - f[2] });
                let v = table[((i0[2] + dz) * d + i0[1] + dy) * d + i0[0] + dx];
                for k in 0..3 {
                    out[k] += w * v[k];
                }
            }
        }
    }
    out
}

// ST 2084 (PQ) constants.
const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

/// PQ code value (0..1) of a light level (1.0 = 10,000 nits).
fn pq_encode(l: f32) -> f32 {
    let lm = l.clamp(0.0, 1.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * lm) / (1.0 + PQ_C3 * lm)).powf(PQ_M2)
}

/// Light level (1.0 = 10,000 nits) of a PQ code value.
fn pq_decode(n: f32) -> f32 {
    let np = n.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    ((np - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * np)).powf(1.0 / PQ_M1)
}

fn linear_to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn srgb_to_linear(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn rgb9e5_decode(w: u32) -> [f32; 3] {
    let e = (w >> 27) as i32;
    let s = 2f32.powi(e - 15 - 9);
    [(w & 0x1ff) as f32 * s, ((w >> 9) & 0x1ff) as f32 * s, ((w >> 18) & 0x1ff) as f32 * s]
}

/// Shared-exponent encoding (9-bit mantissas, 5-bit exponent, bias 15).
fn rgb9e5_encode(c: [f32; 3]) -> u32 {
    const N: i32 = 9;
    const B: i32 = 15;
    let max_value = ((1 << N) - 1) as f32 / (1 << N) as f32 * 2f32.powi(31 - B);
    let c = c.map(|v| if v.is_finite() { v.clamp(0.0, max_value) } else { 0.0 });
    let max_c = c[0].max(c[1]).max(c[2]);
    if max_c <= 0.0 {
        return 0;
    }
    let mut exp = (max_c.log2().floor() as i32).max(-B - 1) + 1 + B;
    let mut denom = 2f32.powi(exp - B - N);
    if (max_c / denom + 0.5).floor() as i32 == (1 << N) {
        denom *= 2.0;
        exp += 1;
    }
    let m = c.map(|v| ((v / denom + 0.5).floor() as u32).min((1 << N) - 1));
    m[0] | (m[1] << 9) | (m[2] << 18) | ((exp as u32) << 27)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb9e5_round_trip() {
        // The three channels share one exponent, so the step size follows the largest channel.
        for c in [[0.0, 0.0, 0.0], [0.18, 0.5, 1.0], [0.001, 0.02, 0.3], [1.0, 1.0, 1.0]] {
            let d = rgb9e5_decode(rgb9e5_encode(c));
            let step = c[0].max(c[1]).max(c[2]) / 256.0;
            for k in 0..3 {
                assert!((d[k] - c[k]).abs() <= step + 1e-6, "{c:?} -> {d:?}");
            }
        }
    }

    #[test]
    fn pq_known_values_and_round_trip() {
        // 100 nits is PQ code 0.508; 10,000 nits is 1.0; 0 is 0.
        assert!((pq_encode(0.01) - 0.508).abs() < 0.002, "{}", pq_encode(0.01));
        assert!((pq_encode(1.0) - 1.0).abs() < 1e-4);
        assert!(pq_encode(0.0) < 1e-6);
        for l in [0.0001, 0.001, 0.01, 0.1, 0.5] {
            assert!((pq_decode(pq_encode(l)) - l).abs() / l < 1e-3, "{l}");
        }
    }

    #[test]
    fn tone_map_cells_round_trip() {
        // Sampling the table at a cell's own light value returns that cell.
        let d = 4;
        let table: Vec<[f32; 3]> = (0..d * d * d).map(|i| [(i % d) as f32, ((i / d) % d) as f32, (i / (d * d)) as f32]).collect();
        for c in 0..d - 1 {
            let x = cell_light(c, d);
            let v = sample_table(&table, d, [x, x, x]);
            assert!(v.iter().all(|&k| (k - c as f32).abs() < 1e-4), "{c}: {v:?}");
        }
    }

    #[test]
    fn srgb_round_trip() {
        for v in [0.0, 0.002, 0.2, 0.5, 1.0] {
            assert!((srgb_to_linear(linear_to_srgb(v)) - v).abs() < 1e-5);
        }
    }
}
