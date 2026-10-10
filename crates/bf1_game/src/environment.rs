//! The level's lighting setup ("visual environment"): sun, sky, fog, exposure
//! and bloom, read from the VisualEnvironment EBX the level uses.

use crate::log::GameLog;
use anyhow::{Context, Result};
use fb_ebx::{Complex, Value};
use fb_level::World;
use fb_res::texture::parse_texture;
use fb_vfs::GameFs;

pub const DEFAULT_ENVIRONMENT: &str = "xpack1/levels/mp_verdun/lighting/ve_mp_verdun_base_sun_01";

/// Format code of RGB9E5 textures (HDR sky), worked out from the data.
pub const PIXEL_FORMAT_RGB9E5: i32 = 29;

#[derive(Debug, Clone)]
pub struct Environment {
    /// Short name for the log and the screen (for example "fog_01").
    pub name: String,
    pub sun_colour: [f32; 3],
    /// Degrees: around the vertical axis, and above the horizon.
    pub sun_rotation_x: f32,
    pub sun_rotation_y: f32,
    pub sun_intensity: f32,
    pub sky_colour: [f32; 3],
    pub fog_start: f32,
    pub fog_end: f32,
    /// Fog amount = c.x t^3 + c.y t^2 + c.z t + c.w, t = 0..1 from start to end (logged, not used).
    pub fog_curve: [f32; 4],
    /// End of the fog colour range (metres).
    pub fog_colour_end: f32,
    /// Fog distances are multiplied by this (variants use it for thicker fog).
    pub fog_distance_multiplier: f32,
    /// Height fog visibility range (metres): 3,000 in the base setup, 50 in the fog event.
    pub height_fog_visibility: f32,
    pub min_ev: f32,
    pub max_ev: f32,
    pub bloom_scale: f32,
    /// Sky texture values times this give luminance (cd/m2).
    pub sky_luminance_scale: f32,
    pub panoramic_rotation: f32,
    pub panoramic_uv_min_y: f32,
    pub panoramic_uv_max_y: f32,
    /// Sky texture: RGB9E5 pixels, width x height.
    pub sky: Option<(u32, u32, Vec<u8>)>,
    /// Colour grading tables applied to the final screen colour, in order:
    /// the weather's own table (`HdrColorGradingLut`), then the plain colour
    /// cube (`ColorGradingTexture`). Order chosen by comparing with the user's
    /// screenshots (MODLOG 2026-10-07).
    pub grading: [Option<GradingTable>; 2],
}

/// A 3D colour table: `size`^3 entries, red fastest, then green, then blue;
/// input and output are screen colours (sRGB, 0 to 1).
#[derive(Debug, Clone)]
pub struct GradingTable {
    pub name: String,
    pub size: usize,
    pub rgb: Vec<[f32; 3]>,
}

impl GradingTable {
    /// Trilinear lookup of an sRGB colour.
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let pos = c.map(|v| v.clamp(0.0, 1.0) * (n - 1) as f32);
        let i0 = pos.map(|p| (p as usize).min(n - 2));
        let f = [pos[0] - i0[0] as f32, pos[1] - i0[1] as f32, pos[2] - i0[2] as f32];
        let mut out = [0f32; 3];
        for dz in 0..2 {
            for dy in 0..2 {
                for dx in 0..2 {
                    let w = (if dx == 1 { f[0] } else { 1.0 - f[0] }) * (if dy == 1 { f[1] } else { 1.0 - f[1] }) * (if dz == 1 { f[2] } else { 1.0 - f[2] });
                    let v = self.rgb[((i0[2] + dz) * n + i0[1] + dy) * n + i0[0] + dx];
                    for k in 0..3 {
                        out[k] += w * v[k];
                    }
                }
            }
        }
        out
    }
}

/// Read a grading table texture (TextureAsset or GradingLutAsset). Formats seen
/// on Verdun: 18 = RGBA8 (32^3 plain cube); 27 = 10-bit RGB + 2 bits, stored
/// with the green axis upside down (33^3 weather tables). Worked out from the
/// data: with that layout the corners map to red, green, blue, white and black.
fn read_grading_table(fs: &GameFs, world: &World, part: usize, ptr: &fb_ebx::Pointer) -> Result<GradingTable> {
    let at = world.resolve(part, ptr).context("grading table not loaded")?;
    let name = world.names[at.0].clone();
    let Some(Value::ResourceRef(rid)) = world.get(at).get("Resource") else { anyhow::bail!("{name}: no Resource") };
    let h = parse_texture(&world.read_res(fs, *rid)?)?;
    let n = h.width as usize;
    anyhow::ensure!(h.height as usize == n && h.depth as usize == n && n >= 2, "{name}: not a cube ({}x{}x{})", h.width, h.height, h.depth);
    let data = world.read_chunk(fs, &h.chunk_id)?;
    anyhow::ensure!(data.len() >= n * n * n * 4, "{name}: {} bytes, need {}", data.len(), n * n * n * 4);
    let word = |i: usize| u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
    let mut rgb = vec![[0f32; 3]; n * n * n];
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                rgb[(b * n + g) * n + r] = match h.pixel_format {
                    18 => {
                        let w = word((b * n + g) * n + r);
                        [0, 1, 2].map(|k| ((w >> (8 * k)) & 0xff) as f32 / 255.0)
                    }
                    27 => {
                        let w = word((b * n + (n - 1 - g)) * n + r);
                        [0, 1, 2].map(|k| ((w >> (10 * k)) & 0x3ff) as f32 / 1023.0)
                    }
                    other => anyhow::bail!("{name}: unknown grading table format {other}"),
                };
            }
        }
    }
    Ok(GradingTable { name, size: n, rgb })
}

/// The grading tables of a ColorCorrectionComponentData (keeps `current` where
/// `use_field` says the component does not set the field).
fn grading_from(
    fs: &GameFs,
    world: &World,
    part: usize,
    cc: &Complex,
    current: &[Option<GradingTable>; 2],
    use_field: impl Fn(&str) -> bool,
    log: &GameLog,
) -> [Option<GradingTable>; 2] {
    let enabled = !matches!(cc.get("Enable"), Some(Value::Bool(false))) && !matches!(cc.get("ColorGradingEnable"), Some(Value::Bool(false)));
    let mut out = current.clone();
    for (slot, field) in ["HdrColorGradingLut", "ColorGradingTexture"].into_iter().enumerate() {
        if !use_field(field) {
            continue;
        }
        out[slot] = match (enabled, cc.get(field)) {
            (true, Some(Value::Pointer(ptr))) if !matches!(ptr, fb_ebx::Pointer::Null) => match read_grading_table(fs, world, part, ptr) {
                Ok(t) => Some(t),
                Err(e) => {
                    log.line(format!("grading table {field} not read: {e:#}"));
                    None
                }
            },
            _ => None,
        };
    }
    out
}

fn f(c: &Complex, name: &str) -> Option<f32> {
    match c.get(name) {
        Some(Value::Float(v)) => Some(*v as f32),
        _ => None,
    }
}

fn vec3(c: &Complex, name: &str) -> Option<[f32; 3]> {
    let Some(Value::Struct(s)) = c.get(name) else { return None };
    Some([f(s, "x")?, f(s, "y")?, f(s, "z")?])
}

fn vec4(c: &Complex, name: &str) -> Option<[f32; 4]> {
    let Some(Value::Struct(s)) = c.get(name) else { return None };
    Some([f(s, "x")?, f(s, "y")?, f(s, "z")?, f(s, "w")?])
}

pub fn read(fs: &GameFs, world: &World, name: &str, log: &GameLog) -> Result<Environment> {
    let part = world.partition_by_name(name).with_context(|| format!("visual environment {name} not loaded"))?;
    let p = &world.partitions[part];
    let find = |ty: &str| p.instances.iter().find(|i| i.value.type_name == ty).map(|i| &i.value);
    let light = find("OutdoorLightComponentData").context("no OutdoorLightComponentData")?;
    let sky = find("SkyComponentData").context("no SkyComponentData")?;
    let fog = find("FogComponentData").context("no FogComponentData")?;
    let tone = find("TonemapComponentData").context("no TonemapComponentData")?;

    let mut env = Environment {
        name: name.rsplit('/').next().unwrap_or(name).trim_start_matches("ve_mp_verdun_").to_string(),
        sun_colour: vec3(light, "SunColor").unwrap_or([1.0; 3]),
        sun_rotation_x: f(light, "SunRotationX").unwrap_or(0.0),
        sun_rotation_y: f(light, "SunRotationY").unwrap_or(45.0),
        sun_intensity: f(light, "SunIntensity").unwrap_or(100_000.0),
        sky_colour: vec3(light, "SkyColor").unwrap_or([0.3, 0.35, 0.4]),
        fog_start: f(fog, "Start").unwrap_or(50.0),
        fog_end: f(fog, "End").unwrap_or(1000.0),
        fog_curve: vec4(fog, "Curve").unwrap_or([0.0, 0.0, 1.0, 0.0]),
        fog_colour_end: f(fog, "FogColorEnd").unwrap_or(1200.0),
        fog_distance_multiplier: f(fog, "FogDistanceMultiplier").unwrap_or(1.0),
        height_fog_visibility: f(fog, "HeightFogVisibilityRange").unwrap_or(3000.0),
        min_ev: f(tone, "MinEV").unwrap_or(12.0),
        max_ev: f(tone, "MaxEV").unwrap_or(12.0),
        bloom_scale: vec3(tone, "BloomScale").map(|v| v[0]).unwrap_or(0.05),
        sky_luminance_scale: f(sky, "LuminanceScale").unwrap_or(1.0),
        panoramic_rotation: f(sky, "PanoramicRotation").unwrap_or(0.0),
        panoramic_uv_min_y: f(sky, "PanoramicUVMinY").unwrap_or(0.0),
        panoramic_uv_max_y: f(sky, "PanoramicUVMaxY").unwrap_or(1.0),
        sky: None,
        grading: [None, None],
    };
    if let Some(cc) = find("ColorCorrectionComponentData") {
        env.grading = grading_from(fs, world, part, cc, &env.grading, |_| true, log);
    }

    // Panoramic sky texture (RGB9E5).
    if let Some(Value::Pointer(ptr)) = sky.get("PanoramicTexture") {
        if let Some(at) = world.resolve(part, ptr) {
            if let Some(Value::ResourceRef(rid)) = world.get(at).get("Resource") {
                let h = parse_texture(&world.read_res(fs, *rid)?)?;
                if h.pixel_format == PIXEL_FORMAT_RGB9E5 {
                    let data = world.read_chunk(fs, &h.chunk_id)?;
                    let size = h.width as usize * h.height as usize * 4;
                    if data.len() >= size {
                        env.sky = Some((h.width as u32, h.height as u32, data[..size].to_vec()));
                    }
                } else {
                    log.line(format!("sky texture has pixel format {} (expected RGB9E5)", h.pixel_format));
                }
            }
        }
    }

    log.line(format!(
        "environment {name}: sun colour {:?}, rotation x {:.1} y {:.1}, intensity {}; fog {}..{} m curve {:?}; EV {}..{}; bloom {:.3}; sky {}",
        env.sun_colour,
        env.sun_rotation_x,
        env.sun_rotation_y,
        env.sun_intensity,
        env.fog_start,
        env.fog_end,
        env.fog_curve,
        env.min_ev,
        env.max_ev,
        env.bloom_scale,
        env.sky.as_ref().map(|s| format!("{}x{}", s.0, s.1)).unwrap_or("none".into())
    ));
    log.line(format!("environment {}: grading {}", env.name, env.grading_summary()));
    if let Some((w, h, data)) = &env.sky {
        let (u, v) = brightest(*w, *h, data);
        // Where the sky texture is brightest, as azimuth (degrees, before rotation) and elevation.
        let elevation = (1.0 - (v - env.panoramic_uv_min_y) / (env.panoramic_uv_max_y - env.panoramic_uv_min_y)) * 90.0;
        log.line(format!(
            "sky texture brightest at u {u:.3} v {v:.3}: elevation about {elevation:.1} deg (sun elevation in data: {:.1}), azimuth u*360 = {:.1} deg",
            env.sun_rotation_y,
            u * 360.0
        ));
    }
    Ok(env)
}

/// Decode one RGB9E5 pixel.
pub fn rgb9e5(w: u32) -> [f32; 3] {
    let s = 2f32.powi((w >> 27) as i32 - 24);
    [(w & 0x1ff) as f32 * s, ((w >> 9) & 0x1ff) as f32 * s, ((w >> 18) & 0x1ff) as f32 * s]
}

/// Texture coordinate of the brightest 16 x 16 pixel block.
fn brightest(w: u32, h: u32, data: &[u8]) -> (f32, f32) {
    let (w, h) = (w as usize, h as usize);
    let mut best = (0.0f32, 0usize, 0usize);
    for by in (0..h).step_by(16) {
        for bx in (0..w).step_by(16) {
            let mut sum = 0.0;
            for y in by..(by + 16).min(h) {
                for x in bx..(bx + 16).min(w) {
                    let i = (y * w + x) * 4;
                    let p = rgb9e5(u32::from_le_bytes(data[i..i + 4].try_into().unwrap()));
                    sum += p[0] + p[1] + p[2];
                }
            }
            if sum > best.0 {
                best = (sum, bx, by);
            }
        }
    }
    ((best.1 as f32 + 8.0) / w as f32, (best.2 as f32 + 8.0) / h as f32)
}

impl Environment {
    /// Direction towards the sun in Frostbite axes (x right, y up, z forward).
    pub fn sun_direction(&self) -> [f32; 3] {
        let (az, el) = (self.sun_rotation_x.to_radians(), self.sun_rotation_y.to_radians());
        [az.sin() * el.cos(), el.sin(), az.cos() * el.cos()]
    }
}

impl Environment {
    /// Sky light for the ambient term: the radiance (cd/m2, linear RGB) that a
    /// horizontal surface receives from the sky dome, i.e. irradiance / pi:
    /// the upper half of the sky texture times LuminanceScale, weighted by the
    /// cosine of the angle from straight up. Uses the same texture mapping as the
    /// sky dome. None without a sky texture.
    pub fn sky_ambient(&self) -> Option<[f32; 3]> {
        let (w, h, data) = self.sky.as_ref()?;
        let (w, h) = (*w as usize, *h as usize);
        let span = (self.panoramic_uv_max_y - self.panoramic_uv_min_y).max(1e-6);
        let mut sum = [0f64; 3];
        let mut weight = 0f64;
        for y in 0..h {
            let v = (y as f32 + 0.5) / h as f32;
            if v < self.panoramic_uv_min_y || v > self.panoramic_uv_max_y {
                continue;
            }
            let elevation = (1.0 - (v - self.panoramic_uv_min_y) / span) * std::f32::consts::FRAC_PI_2;
            let zenith = std::f32::consts::FRAC_PI_2 - elevation;
            // Solid angle of a texel row times cos(zenith); sin(zenith) dzenith dphi.
            let wgt = (zenith.cos() * zenith.sin()) as f64;
            for x in 0..w {
                let i = (y * w + x) * 4;
                let p = rgb9e5(u32::from_le_bytes(data[i..i + 4].try_into().unwrap()));
                for k in 0..3 {
                    sum[k] += p[k] as f64 * wgt;
                }
                weight += wgt;
            }
        }
        // The weights integrate to pi over the hemisphere times the texel area; dividing by
        // their sum gives the cosine-weighted mean radiance = irradiance / pi.
        (weight > 0.0).then(|| sum.map(|s| (s / weight) as f32 * self.sky_luminance_scale))
    }

    /// Names of the grading tables, for the log.
    pub fn grading_summary(&self) -> String {
        self.grading
            .iter()
            .map(|t| t.as_ref().map(|t| format!("{} ({}^3)", t.name.rsplit('/').next().unwrap_or(&t.name), t.size)).unwrap_or("none".into()))
            .collect::<Vec<_>>()
            .join(" then ")
    }

    /// Camera exposure (EV100) for this environment: the top of the level's
    /// auto-exposure range. In daylight the game's auto exposure wants a higher
    /// EV than the range allows, so it stays at the top.
    pub fn exposure_ev(&self) -> f32 {
        // BF1_EV=<value> overrides it for tests.
        std::env::var("BF1_EV").ok().and_then(|v| v.parse().ok()).unwrap_or(self.max_ev)
    }

    /// Brightness factor for the (unlit) sky texture on screen: luminance scale
    /// times the camera exposure (exposure = 1 / (1.2 * 2^EV), as in Bevy's
    /// physical camera). Uses the same EV as the camera, so sky and world match.
    pub fn sky_brightness(&self) -> f32 {
        self.sky_luminance_scale / (1.2 * 2f32.powf(self.exposure_ev()))
    }
}

/// Weather variants offered by the G key: base, then these (if present).
pub const VARIANTS: &[&str] = &[
    "xpack1/levels/mp_verdun/lighting/ve_mp_verdun_overcast_01",
    "xpack1/levels/mp_verdun/lighting/ve_mp_verdun_fog_02",
    "xpack1/levels/mp_verdun/lighting/ve_mp_verdun_fog_01",
    "xpack1/levels/mp_verdun/lighting/ve_mp_verdun_firerain_01",
];

/// A variant: a copy of `base` with the sun, sky and fog replaced by the
/// variant's own components where it has them.
pub fn overlay(fs: &GameFs, world: &World, base: &Environment, name: &str, log: &GameLog) -> Result<Environment> {
    let part = world.partition_by_name(name).with_context(|| format!("visual environment {name} not loaded"))?;
    let p = &world.partitions[part];
    let find = |ty: &str| p.instances.iter().find(|i| i.value.type_name == ty).map(|i| &i.value);
    let mut env = base.clone();
    env.name = name.rsplit('/').next().unwrap_or(name).trim_start_matches("ve_mp_verdun_").to_string();
    let mut parts = Vec::new();
    // A variant component replaces only the fields named in its PropertyOverrides list.
    let overrides = |c: &Complex, field: &str| match c.get("PropertyOverrides") {
        Some(Value::Array { items, .. }) => items.iter().any(|v| matches!(v, Value::String(s) if s == field)),
        _ => true,
    };
    if let Some(light) = find("OutdoorLightComponentData") {
        if overrides(light, "SunColor") { env.sun_colour = vec3(light, "SunColor").unwrap_or(env.sun_colour); parts.push("sun colour"); }
        if overrides(light, "SunRotationX") { env.sun_rotation_x = f(light, "SunRotationX").unwrap_or(env.sun_rotation_x); parts.push("sun x"); }
        if overrides(light, "SunRotationY") { env.sun_rotation_y = f(light, "SunRotationY").unwrap_or(env.sun_rotation_y); parts.push("sun y"); }
        if overrides(light, "SunIntensity") { env.sun_intensity = f(light, "SunIntensity").unwrap_or(env.sun_intensity); parts.push("sun intensity"); }
        if overrides(light, "SkyColor") { env.sky_colour = vec3(light, "SkyColor").unwrap_or(env.sky_colour); parts.push("sky colour"); }
    }
    if let Some(cc) = find("ColorCorrectionComponentData") {
        env.grading = grading_from(fs, world, part, cc, &env.grading, |field| overrides(cc, field), log);
        parts.push("colour grading");
    }
    if let Some(fog) = find("FogComponentData") {
        if overrides(fog, "Start") { env.fog_start = f(fog, "Start").unwrap_or(env.fog_start); }
        if overrides(fog, "End") { env.fog_end = f(fog, "End").unwrap_or(env.fog_end); }
        if overrides(fog, "Curve") { env.fog_curve = vec4(fog, "Curve").unwrap_or(env.fog_curve); }
        if overrides(fog, "FogColorEnd") { env.fog_colour_end = f(fog, "FogColorEnd").unwrap_or(env.fog_colour_end); }
        if overrides(fog, "FogDistanceMultiplier") { env.fog_distance_multiplier = f(fog, "FogDistanceMultiplier").unwrap_or(env.fog_distance_multiplier); }
        if overrides(fog, "HeightFogVisibilityRange") { env.height_fog_visibility = f(fog, "HeightFogVisibilityRange").unwrap_or(env.height_fog_visibility); }
        parts.push("fog");
    }
    if let Some(sky) = find("SkyComponentData") {
        if overrides(sky, "LuminanceScale") { env.sky_luminance_scale = f(sky, "LuminanceScale").unwrap_or(env.sky_luminance_scale); }
        if overrides(sky, "PanoramicRotation") { env.panoramic_rotation = f(sky, "PanoramicRotation").unwrap_or(env.panoramic_rotation); }
        if overrides(sky, "PanoramicUVMinY") { env.panoramic_uv_min_y = f(sky, "PanoramicUVMinY").unwrap_or(env.panoramic_uv_min_y); }
        if overrides(sky, "PanoramicUVMaxY") { env.panoramic_uv_max_y = f(sky, "PanoramicUVMaxY").unwrap_or(env.panoramic_uv_max_y); }
        if let (true, Some(Value::Pointer(ptr))) = (overrides(sky, "PanoramicTexture"), sky.get("PanoramicTexture")) {
            if let Some(at) = world.resolve(part, ptr) {
                if let Some(Value::ResourceRef(rid)) = world.get(at).get("Resource") {
                    let h = parse_texture(&world.read_res(fs, *rid)?)?;
                    if h.pixel_format == PIXEL_FORMAT_RGB9E5 {
                        let data = world.read_chunk(fs, &h.chunk_id)?;
                        let size = h.width as usize * h.height as usize * 4;
                        if data.len() >= size {
                            env.sky = Some((h.width as u32, h.height as u32, data[..size].to_vec()));
                        }
                    }
                }
            }
        }
        parts.push("sky");
    }
    log.line(format!(
        "weather variant {}: replaces {:?}; height fog visibility {} m (base {}); fog thickness x{:.2} of base; sun intensity {}; grading {}",
        env.name,
        parts,
        env.height_fog_visibility,
        base.height_fog_visibility,
        env.fog_thickness() / base.fog_thickness(),
        env.sun_intensity,
        env.grading_summary()
    ));
    Ok(env)
}

impl Environment {
    /// How thick the fog is, used only as a ratio between variants: the inverse of
    /// the height fog visibility range (the ground is inside the height fog layer in
    /// every Verdun variant). The distance fog curve barely differs between variants.
    pub fn fog_thickness(&self) -> f32 {
        1.0 / self.height_fog_visibility.max(1.0)
    }
}
