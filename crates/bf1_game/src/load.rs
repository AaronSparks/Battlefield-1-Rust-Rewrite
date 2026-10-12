//! Read the level from the install into plain CPU data (no Bevy types except
//! what the startup system needs). Coordinates are already converted to Bevy.

use crate::convert;
use crate::log::GameLog;
use anyhow::{Context, Result};
use fb_ebx::{Pointer, Value};
use fb_level::{Walker, World};
use fb_res::material::{base_color_rank, is_colour_texture_name};
use fb_res::mesh::{decode_lod, parse_meshset, DecodeStats};
use fb_res::texture::{block_format, parse_texture, BlockFormat};
use fb_vfs::GameFs;
use std::collections::HashMap;
use std::time::Instant;

pub struct Options {
    pub superbundle: String,
    pub level: String,
    pub skip_subworlds: Vec<String>,
    /// Skip this many of the largest mips to save video memory.
    pub texture_mip_skip: usize,
    /// The same for the tiling ground textures. Full size (0) gave no
    /// measurable gain (MODLOG 2026-10-07), so 1; `BF1_GROUND_MIP_SKIP=<n>`.
    pub ground_mip_skip: usize,
    /// Which placements get their object variation (DESIGN.md "decal
    /// variants"): "decals" (meshes under `artassets/decals`), "all" or
    /// "off"; `BF1_VARIATIONS=<mode>`.
    pub variations: String,
    /// Glass without a colour texture is drawn see-through (DESIGN.md
    /// "window glass"); `BF1_GLASS=off` guesses a texture as before.
    pub glass: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            superbundle: "Win32/xpack1/levels/mp_verdun/mp_verdun".into(),
            level: "Xpack1/Levels/MP_Verdun/MP_Verdun".into(),
            skip_subworlds: ["breakthrough0", "rush0", "tugofwar0", "dominiation0", "possession0", "teamdeathmatch0", "autotests"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            texture_mip_skip: 1,
            ground_mip_skip: std::env::var("BF1_GROUND_MIP_SKIP").ok().and_then(|v| v.parse().ok()).unwrap_or(1),
            variations: std::env::var("BF1_VARIATIONS").unwrap_or_else(|_| "decals".into()).to_lowercase(),
            glass: std::env::var("BF1_GLASS").map_or(true, |v| v != "off"),
        }
    }
}

/// One drawable part of a mesh (one material).
pub struct Part {
    pub positions: Vec<[f32; 3]>,
    /// Empty if the mesh has no usable normals (Bevy computes them).
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub texture: Option<usize>,
    /// Plain sRGB colour for parts that have no texture by design (wires).
    pub colour: Option<[f32; 3]>,
    /// Why there is no texture (for the log).
    pub missing: Option<String>,
    /// The part takes the terrain colour under it (shader "virtual texture only").
    pub terrain_coloured: bool,
    /// Colour from a shader value (paint, fabric); see `material_tint`.
    pub tint: Option<Tint>,
    /// Detail texture over the base texture (building shaders), on `uvs1`.
    pub detail: Option<Detail>,
    /// Second texture coordinate set (vertex usage 0x22); only kept for parts
    /// with a detail texture.
    pub uvs1: Vec<[f32; 2]>,
    /// Index into the mesh asset's `Materials`.
    pub material_id: i32,
    /// Set for parts from the "transparent decal" category: drawn blended
    /// over other surfaces (DESIGN.md "decal variants", step 7).
    pub decal: Option<DecalLook>,
    /// Foliage: texture whose alpha is the smoothness (DESIGN.md "shiny
    /// leaves and wet ground"), index into `Loaded::textures`.
    pub smoothness: Option<usize>,
    /// Normal texture (red, green = x, y), index into `Loaded::textures`
    /// (DESIGN.md "normal maps on models").
    pub normal_map: Option<usize>,
    /// Per-vertex tangents for the normal map (xyz along +u, w = sign so
    /// that w * cross(normal, tangent) follows +v); empty without a normal map.
    pub tangents: Vec<[f32; 4]>,
    /// Set for parts whose shader blends in the terrain's ground layers
    /// (DESIGN.md "terrain blend on mounds").
    pub terrain_blend: Option<TerrainBlend>,
    /// Set for glass without a colour texture (DESIGN.md "window glass").
    pub glass: Option<GlassLook>,
}

/// How a glass part is drawn (DESIGN.md "window glass"): see-through, with
/// the material's colour and opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassLook {
    /// `BaseColor` (linear); 0.34 grey when the material gives none.
    pub colour: [f32; 3],
    /// `Opacity`; 0.5 when the material gives none.
    pub opacity: f32,
}

/// The glass look of a material and its variant (the variant's values
/// first), if the shader name contains "glass".
fn glass_look(world: &World, mat: (usize, usize), var: Option<(usize, usize)>) -> Option<GlassLook> {
    let shader = var.and_then(|v| shader_name(world, v)).or_else(|| shader_name(world, mat))?;
    if !shader.rsplit('/').next().unwrap_or("").to_ascii_lowercase().contains("glass") {
        return None;
    }
    let mut g = GlassLook { colour: [0.342; 3], opacity: 0.5 };
    // The material's values, then the variant's on top.
    for m in std::iter::once(mat).chain(var) {
        let Some(Value::Struct(sh)) = world.get(m).get("Shader") else { continue };
        let Some(Value::Array { items, .. }) = sh.get("VectorParameters") else { continue };
        for v in items {
            let Value::Struct(v) = v else { continue };
            let (Some(Value::String(name)), Some(Value::Struct(x))) = (v.get("ParameterName"), v.get("Value")) else { continue };
            let f = |n: &str| match x.get(n) {
                Some(Value::Float(f)) => Some(*f as f32),
                _ => None,
            };
            match (name.as_str(), f("x"), f("y"), f("z")) {
                ("BaseColor", Some(r), Some(gr), Some(b)) => g.colour = [r, gr, b],
                ("Opacity", Some(o), _, _) => g.opacity = o.clamp(0.0, 1.0),
                _ => {}
            }
        }
    }
    Some(g)
}

/// Values of a terrain blend shader (`ObjectPreset_..._TerrainBlend...`;
/// DESIGN.md "terrain blend on mounds"). Our reading, not confirmed: faces
/// that point up more than `threshold` take the terrain's ground layers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerrainBlend {
    /// `UpNormalMaskEnable`.
    pub up_mask: bool,
    /// `UpNormalMaskThreshold`: the up component of the normal where the blend starts.
    pub threshold: f32,
    /// `UpNormalMaskOffset` (meaning not known; for the log).
    pub offset: f32,
    /// `TerrainBlendUpNormals` and `TerrainBlendTextureMask` (meaning not known; for the log).
    pub up_normals: bool,
    pub texture_mask: bool,
}

/// The terrain blend values of a material and its variant (the variant's
/// values first), if either names a terrain blend shader.
fn terrain_blend(world: &World, mat: (usize, usize), var: Option<(usize, usize)>) -> Option<TerrainBlend> {
    let shader = var.and_then(|v| shader_name(world, v)).or_else(|| shader_name(world, mat))?;
    if !shader.to_ascii_lowercase().contains("terrainblend") {
        return None;
    }
    // One parameter value from the variant, else from the material.
    let find = |list: &str, name: &str| -> Option<Value> {
        var.into_iter().chain(std::iter::once(mat)).find_map(|m| {
            let Some(Value::Struct(sh)) = world.get(m).get("Shader") else { return None };
            let Some(Value::Array { items, .. }) = sh.get(list) else { return None };
            items.iter().find_map(|v| {
                let Value::Struct(v) = v else { return None };
                matches!(v.get("ParameterName"), Some(Value::String(n)) if n == name).then(|| v.get("Value").cloned()).flatten()
            })
        })
    };
    let flag = |name: &str| matches!(find("BoolParameters", name), Some(Value::Bool(true)));
    let scalar = |name: &str, default: f32| match find("VectorParameters", name) {
        Some(Value::Struct(x)) => match x.get("x") {
            Some(Value::Float(f)) => *f as f32,
            _ => default,
        },
        _ => default,
    };
    Some(TerrainBlend {
        up_mask: flag("UpNormalMaskEnable"),
        threshold: scalar("UpNormalMaskThreshold", 0.6),
        offset: scalar("UpNormalMaskOffset", 0.0),
        up_normals: flag("TerrainBlendUpNormals"),
        texture_mask: flag("TerrainBlendTextureMask"),
    })
}

/// How a decal part is drawn (DESIGN.md "decal variants", step 7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecalLook {
    /// Alpha multiplier (`Opacity`).
    pub opacity: f32,
    /// Colour multiplier (`DecalColorMultiply`); white if not set.
    pub multiply: [f32; 3],
}

/// Decal shader values of a material (or a material variation): opacity
/// and colour multiplier (defaults 1 and white). The atlas values
/// (`TileCountX/Y`, `TileIndexX/Y`) are not used: the decal textures hold one
/// centred picture, not 2 x 2 tiles (MODLOG "decal variants").
fn decal_params(world: &World, mat: (usize, usize)) -> DecalLook {
    let mut p = DecalLook { opacity: 1.0, multiply: [1.0; 3] };
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return p };
    let Some(Value::Array { items, .. }) = sh.get("VectorParameters") else { return p };
    for v in items {
        let Value::Struct(v) = v else { continue };
        let (Some(Value::String(name)), Some(Value::Struct(x))) = (v.get("ParameterName"), v.get("Value")) else { continue };
        let f = |n: &str| match x.get(n) {
            Some(Value::Float(f)) => *f as f32,
            _ => 0.0,
        };
        match name.as_str() {
            "Opacity" => p.opacity = f("x"),
            "DecalColorMultiply" => p.multiply = [f("x"), f("y"), f("z")],
            _ => {}
        }
    }
    p
}

/// A material's normal texture (DESIGN.md "normal maps on models"): the
/// parameters the Verdun materials use for their main normal map (not the
/// detail or tile-breaker ones).
fn normal_texture(world: &World, mat: (usize, usize)) -> Option<(usize, usize)> {
    const NAMES: [&str; 8] = ["Normal", "_Normal", "Base_Texture_N", "TrunkNormal", "TexNormalmap", "Normal_MetalMask_AO", "NormalMap", "Normals"];
    NAMES.iter().find_map(|n| texture_parameter(world, mat, n))
}

/// True if red and green hold a plain normal: at least 95% of a small mip's
/// texels have (2r - 1)^2 + (2g - 1)^2 <= 1.05.
fn is_normal_map(t: &TextureData) -> bool {
    let Some(px) = small_mip(t, 64) else { return false };
    let inside = px
        .iter()
        .filter(|p| {
            let (x, y) = (p[0] as f32 / 127.5 - 1.0, p[1] as f32 / 127.5 - 1.0);
            x * x + y * y <= 1.05
        })
        .count();
    inside as f32 >= 0.95 * px.len().max(1) as f32
}

/// Normal maps as two-channel BC5 (red, green only; DESIGN.md "normal maps
/// on models"): Bevy's StandardMaterial then rebuilds z itself, so the
/// normal-mapped parts can share its batched (bindless) drawing. A texture
/// that is also used for something else gets a converted copy.
fn convert_normal_maps(loaded: &mut Loaded, log: &GameLog) {
    let t = Instant::now();
    let mut normal: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut other: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for p in loaded.meshes.iter().flat_map(|m| &m.parts) {
        normal.extend(p.normal_map);
        other.extend(p.texture);
        other.extend(p.smoothness);
        other.extend(p.tint.and_then(|t| t.mask));
        if let Some(d) = p.detail {
            other.insert(d.texture);
            other.insert(d.mask);
        }
    }
    for g in &loaded.ground {
        other.insert(g.texture);
        other.extend(g.nrh);
    }
    let list: Vec<usize> = normal.into_iter().collect();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(32);
    let textures = &loaded.textures;
    let converted: Vec<(usize, Option<TextureData>)> = std::thread::scope(|sc| {
        let jobs: Vec<_> = list
            .chunks(list.len().div_ceil(threads).max(1))
            .map(|part| sc.spawn(move || part.iter().map(|&j| (j, to_bc5(&textures[j]))).collect::<Vec<_>>()))
            .collect();
        jobs.into_iter().flat_map(|j| j.join().unwrap_or_default()).collect()
    });
    let (mut in_place, mut copies, mut failed) = (0usize, 0usize, 0usize);
    let mut remap: HashMap<usize, Option<usize>> = HashMap::new();
    for (j, c) in converted {
        match c {
            Some(c) if other.contains(&j) => {
                loaded.textures.push(c);
                remap.insert(j, Some(loaded.textures.len() - 1));
                copies += 1;
            }
            Some(c) => {
                loaded.textures[j] = c;
                in_place += 1;
            }
            None => {
                remap.insert(j, None);
                failed += 1;
            }
        }
    }
    for p in loaded.meshes.iter_mut().flat_map(|m| m.parts.iter_mut()) {
        if let Some(j) = p.normal_map {
            if let Some(r) = remap.get(&j) {
                p.normal_map = *r;
                if r.is_none() {
                    p.tangents.clear();
                }
            }
        }
    }
    log.line(format!(
        "normal maps as BC5: {} converted in place, {} copies (texture also used otherwise), {} failed ({:.2} s)",
        in_place,
        copies,
        failed,
        t.elapsed().as_secs_f64()
    ));
}

/// The red and green channels of a texture (all its mips) as BC5.
fn to_bc5(t: &TextureData) -> Option<TextureData> {
    let rgba = crate::ground::decode_all_mips(t)?;
    let mut data = Vec::new();
    let (mut w, mut h, mut offset) = (t.width as usize, t.height as usize, 0usize);
    for _ in 0..t.mip_count {
        let mip = rgba.get(offset..offset + w * h * 4)?;
        for by in 0..h.div_ceil(4) {
            for bx in 0..w.div_ceil(4) {
                for channel in 0..2 {
                    let mut v = [0u8; 16];
                    for (k, value) in v.iter_mut().enumerate() {
                        // Pixels past the edge repeat the last row or column.
                        let (x, y) = ((bx * 4 + k % 4).min(w - 1), (by * 4 + k / 4).min(h - 1));
                        *value = mip[(y * w + x) * 4 + channel];
                    }
                    data.extend_from_slice(&bc4_block(&v));
                }
            }
        }
        offset += w * h * 4;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Some(TextureData {
        name: format!("{} (BC5 red-green)", t.name),
        format: BlockFormat::Bc5,
        width: t.width,
        height: t.height,
        mip_count: t.mip_count,
        data,
        alpha: [0.0, 0.0, 1.0],
        cutout: false,
    })
}

/// One BC4 block (8 bytes) for 16 values: the largest and smallest value as
/// end points, 6 steps between them, and a 3-bit index per value.
fn bc4_block(v: &[u8; 16]) -> [u8; 8] {
    let (lo, hi) = (*v.iter().min().expect("16 values"), *v.iter().max().expect("16 values"));
    let mut out = [0u8; 8];
    out[0] = hi;
    out[1] = lo;
    if hi == lo {
        return out;
    }
    // Palette for end point 0 > end point 1: e0, e1, then 6 steps from e0 to e1.
    let mut palette = [0i32; 8];
    palette[0] = hi as i32;
    palette[1] = lo as i32;
    for i in 1..=6 {
        palette[i + 1] = ((7 - i as i32) * hi as i32 + i as i32 * lo as i32) / 7;
    }
    let mut bits = 0u64;
    for (k, &value) in v.iter().enumerate() {
        let best = (0..8).min_by_key(|&i| (palette[i] - value as i32).abs()).expect("8 entries") as u64;
        bits |= best << (3 * k);
    }
    out[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
    out
}

/// Per-vertex tangents from the texture coordinates: the tangent follows +u
/// (made perpendicular to the normal), w = 1 or -1 so that
/// w * cross(normal, tangent) follows +v (as Bevy's mikktspace frame).
pub fn tangents(positions: &[[f32; 3]], normals: &[[f32; 3]], uvs: &[[f32; 2]], indices: &[u32]) -> Vec<[f32; 4]> {
    use bevy::math::Vec3;
    let n = positions.len();
    let (mut tu, mut tv) = (vec![Vec3::ZERO; n], vec![Vec3::ZERO; n]);
    for tri in indices.chunks_exact(3) {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let (p0, p1, p2) = (Vec3::from(positions[a]), Vec3::from(positions[b]), Vec3::from(positions[c]));
        let (e1, e2) = (p1 - p0, p2 - p0);
        let (du1, dv1) = (uvs[b][0] - uvs[a][0], uvs[b][1] - uvs[a][1]);
        let (du2, dv2) = (uvs[c][0] - uvs[a][0], uvs[c][1] - uvs[a][1]);
        let r = du1 * dv2 - du2 * dv1;
        if r.abs() < 1e-12 {
            continue;
        }
        let along_u = (e1 * dv2 - e2 * dv1) / r;
        let along_v = (e2 * du1 - e1 * du2) / r;
        for i in [a, b, c] {
            tu[i] += along_u;
            tv[i] += along_v;
        }
    }
    (0..n)
        .map(|i| {
            let nrm = Vec3::from(normals[i]).normalize_or_zero();
            let mut t = (tu[i] - nrm * nrm.dot(tu[i])).normalize_or_zero();
            if t == Vec3::ZERO {
                // No usable texture direction: any direction across the normal.
                t = nrm.any_orthonormal_vector();
            }
            let w = if nrm.cross(t).dot(tv[i]) < 0.0 { -1.0 } else { 1.0 };
            [t.x, t.y, t.z, w]
        })
        .collect()
}

/// The foliage texture whose alpha is the smoothness: a texture parameter
/// whose name says "Translucency" (`NormalTranslucencySmoothnessMap`), or one
/// whose texture name ends `_NTS` or `_NNRS` (the vegetation `TexNormalmap`).
/// Not `BaseColor_Smoothness` (the `_CS` colour texture of other objects:
/// a later step).
fn smoothness_texture(world: &World, mat: (usize, usize)) -> Option<(usize, usize)> {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
    let Some(Value::Array { items, .. }) = sh.get("TextureParameters") else { return None };
    items.iter().find_map(|tp| {
        let Value::Struct(tp) = tp else { return None };
        let Some(Value::Pointer(p)) = tp.get("Value") else { return None };
        if matches!(p, Pointer::Null) {
            return None;
        }
        let by_param = matches!(tp.get("ParameterName"), Some(Value::String(n)) if n.to_ascii_lowercase().contains("translucency"));
        let name = world.link_name(mat.0, p).to_ascii_lowercase();
        (by_param || name.ends_with("_nts") || name.ends_with("_nnrs")).then(|| world.resolve(mat.0, p)).flatten()
    })
}

/// The shader a material (or material variation) names, if any.
fn shader_name(world: &World, mat: (usize, usize)) -> Option<String> {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
    match sh.get("Shader") {
        Some(Value::Pointer(p)) if !matches!(p, Pointer::Null) => Some(world.link_name(mat.0, p)),
        _ => None,
    }
}

/// A detail texture over the base texture (DESIGN.md "flat grey walls"):
/// colour = mix(base, detail, mask alpha), both sampled with `Part::uvs1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detail {
    /// Indices into the loaded textures.
    pub texture: usize,
    /// Texture whose alpha is the blend mask: `_NormalDetail` (`_NAA`), or
    /// the detail itself for `Detail_Texture_CA`.
    pub mask: usize,
    /// Share of the part's surface collapsed in the detail's set (for the log).
    pub collapsed: f32,
}

/// Where a material's detail comes from (indices into the loaded textures),
/// and whether the base uses the second set (`FortDetailBlend2UV`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailSource {
    pub texture: usize,
    pub mask: usize,
    pub swap_sets: bool,
}

/// A material colour that comes from a shader value, not from the texture
/// (the texture is neutral grey). DESIGN.md "material tints, part 1".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    /// Mean colour of the part, linear RGB (the shader value as stored).
    pub colour: [f32; 3],
    /// Texture repeats per UV unit.
    pub uv_scale: f32,
    /// Which kind of tint (for the log).
    pub kind: &'static str,
    /// Paint only: the `Masks` texture (index into the loaded textures);
    /// green = painted.
    pub mask: Option<usize>,
}

pub struct LoadedMesh {
    pub name: String,
    pub parts: Vec<Part>,
    /// Why parts were left out (one entry per part, for the log).
    pub hidden: Vec<String>,
    /// Sections only in the "transparent decal" category (for the log).
    pub decal_sections: usize,
    /// Triangles left out because they belong to damage-state parts
    /// (DESIGN.md "damaged parts hidden at the start").
    pub damaged_triangles: usize,
}

/// Mesh parts that only damage states show (DESIGN.md "damaged parts hidden
/// at the start"), per mesh asset. From every `...ModelEntityData` with a
/// mesh and part components: the part numbers of each component's other
/// health states, minus those of the start states (the most health). When
/// two entities with the same mesh disagree, only the parts both hide.
#[derive(Default)]
pub struct HiddenParts {
    pub by_mesh: HashMap<(usize, usize), std::collections::BTreeSet<u32>>,
    /// Entities with part components, and meshes where two entities disagree.
    pub entities: usize,
    pub disagreements: usize,
}

fn hidden_parts(world: &World) -> HiddenParts {
    use std::collections::BTreeSet;
    let mut hp = HiddenParts::default();
    for (pi, partition) in world.partitions.iter().enumerate() {
        for inst in &partition.instances {
            let v = &inst.value;
            if !v.type_name.ends_with("ModelEntityData") {
                continue;
            }
            let Some(Value::Pointer(mp)) = v.get("Mesh") else { continue };
            let Some(mesh) = world.resolve(pi, mp) else { continue };
            let Some(Value::Array { items, .. }) = v.get("Components") else { continue };
            let (mut start, mut later) = (BTreeSet::new(), BTreeSet::new());
            let mut any = false;
            for c in items {
                let Value::Pointer(cp) = c else { continue };
                let Some(at) = world.resolve(pi, cp) else { continue };
                let comp = world.get(at);
                if comp.type_name != "PartComponentData" {
                    continue;
                }
                let Some(Value::Array { items: states, .. }) = comp.get("HealthStates") else { continue };
                // (health, mesh part) of each state; part None = nothing shown.
                let list: Vec<(f64, Option<u32>)> = states
                    .iter()
                    .filter_map(|st| {
                        let Value::Pointer(sp) = st else { return None };
                        let st = world.get(world.resolve(at.0, sp)?);
                        let health = match st.get("Health") {
                            Some(Value::Float(h)) => *h,
                            Some(Value::Int(h)) => *h as f64,
                            Some(Value::UInt(h)) => *h as f64,
                            _ => 0.0,
                        };
                        let part = match st.get("PartIndex") {
                            Some(Value::UInt(p)) if *p < u64::from(u32::MAX) => Some(*p as u32),
                            Some(Value::Int(p)) if (0..i64::from(u32::MAX)).contains(p) => Some(*p as u32),
                            _ => None,
                        };
                        Some((health, part))
                    })
                    .collect();
                // The start state: the most health (the first one on a tie).
                let Some(first) = (0..list.len()).max_by(|&a, &b| list[a].0.total_cmp(&list[b].0).then(b.cmp(&a))) else { continue };
                any = true;
                for (k, (_, p)) in list.iter().enumerate() {
                    if let Some(p) = p {
                        if k == first {
                            start.insert(*p);
                        } else {
                            later.insert(*p);
                        }
                    }
                }
            }
            if !any {
                continue;
            }
            hp.entities += 1;
            let hide: BTreeSet<u32> = later.difference(&start).copied().collect();
            match hp.by_mesh.get_mut(&mesh) {
                Some(old) if *old != hide => {
                    hp.disagreements += 1;
                    *old = old.intersection(&hide).copied().collect();
                }
                Some(_) => {}
                None => {
                    hp.by_mesh.insert(mesh, hide);
                }
            }
        }
    }
    hp.by_mesh.retain(|_, s| !s.is_empty());
    hp
}

/// How a material is drawn.
enum Look {
    /// Colour texture, tint, detail texture, smoothness texture (foliage),
    /// normal texture.
    Texture(usize, Option<Tint>, Option<DetailSource>, Option<usize>, Option<usize>),
    Colour([f32; 3]),
    /// Colour from the terrain under the object.
    Terrain,
    /// Not drawn; the text says why.
    Hide(&'static str),
    /// Drawn grey; the text says why (for the log).
    Missing(String),
    /// See-through glass without a colour texture (DESIGN.md "window glass").
    Glass(GlassLook),
}

pub struct TextureData {
    /// Asset name (for the log).
    pub name: String,
    pub format: BlockFormat,
    pub width: u32,
    pub height: u32,
    pub mip_count: u32,
    /// All used mips, largest first, as stored in the chunk.
    pub data: Vec<u8>,
    /// Share of pixels with alpha below 25%, between, and above 75% (from a small mip).
    pub alpha: [f32; 3],
    /// The alpha is a cut-out shape (leaves, branches): see `is_cutout`.
    pub cutout: bool,
}

/// A cut-out texture (leaf and branch cards) has many clear pixels, some fully
/// solid ones and few in between. Textures named `_CS` keep smoothness in their
/// alpha: rough ones have many low values but almost no fully solid pixels, or
/// many middle values. Measured on Verdun (MODLOG 2026-10-07).
pub fn is_cutout(alpha: [f32; 3]) -> bool {
    alpha[0] >= 0.05 && alpha[2] >= 0.02 && alpha[1] <= 0.25
}

/// A tiling ground layer colour texture (for the terrain shader).
pub struct GroundTexture {
    /// Asset name as the shader database writes it.
    pub name: String,
    /// Index into `Loaded::textures`.
    pub texture: usize,
    /// Terrain layer numbers whose main colour texture this is (from the shader database).
    pub layers: Vec<u8>,
    /// The layer's normal/roughness/height texture (`_NRH`; red and green =
    /// normal, alpha = height), index into `Loaded::textures`.
    pub nrh: Option<usize>,
}

pub struct Loaded {
    pub meshes: Vec<LoadedMesh>,
    /// Ground layer colour textures used by the level's terrain shaders.
    pub ground: Vec<GroundTexture>,
    /// (index into meshes, Bevy transform as a column-major 4x4 matrix)
    pub placements: Vec<(usize, [f32; 16])>,
    pub textures: Vec<TextureData>,
    pub terrain: fb_terrain::Terrain,
    /// Bevy coordinates of the middle of the battle area, on the ground.
    pub centre: [f32; 3],
    /// Sun, sky, fog and exposure: the level's base setup, then weather variants.
    /// Empty if the base could not be read.
    pub environments: Vec<crate::environment::Environment>,
    /// Terrain mesh scattering (grass, debris): one entry per layer and type.
    pub scatter: Vec<ScatterType>,
}

/// One terrain mesh scattering type on one ground layer (DESIGN.md
/// "scattered grass and debris").
#[derive(Debug, Clone)]
pub struct ScatterType {
    pub layer: u8,
    pub identifier: u32,
    /// Index into `Loaded::meshes`.
    pub mesh: usize,
    /// Instances per square metre at full layer weight ("High" quality).
    pub density: f32,
    /// Scale range: x = horizontal, y = height.
    pub min_scale: [f32; 2],
    pub max_scale: [f32; 2],
    /// Draw distance (metres): the largest of the record's 4 distance values.
    pub distance: f32,
    /// Untextured scatter parts whose shader takes the ground colour
    /// (`TerrainBlend` 1): their tint `Color1` (linear). None otherwise.
    pub ground_tint: Option<[f32; 3]>,
}

pub fn load_level(log: &GameLog, opts: &Options) -> Result<Loaded> {
    let t = Instant::now();
    let root = fb_vfs::find_game_dir()?;
    log.line(format!("game dir: {}", root.display()));
    let mut fs = GameFs::open(&root)?;
    let toc_chunks = fs.index_toc_chunks()?;
    let fs = fs;
    log.line(format!("archives indexed: {} sha1, {} toc chunks ({:.2} s)", fs.index.len(), toc_chunks, t.elapsed().as_secs_f64()));

    let t = Instant::now();
    let world = World::load(&fs, &[opts.superbundle.as_str()])?;
    log.line(format!("EBX loaded: {} partitions ({:.2} s)", world.partitions.len(), t.elapsed().as_secs_f64()));

    let t = Instant::now();
    let mut walker = Walker::new(&world, &fs);
    walker.skip_subworlds = opts.skip_subworlds.clone();
    walker.walk_level(&opts.level)?;
    log.line(format!("level walked: {} placed meshes ({:.2} s)", walker.meshes.len(), t.elapsed().as_secs_f64()));

    let t = Instant::now();
    let (terrain, mask_note) = load_terrain(&fs, &world)?;
    log.line(format!("terrain loaded: {} height tiles ({:.2} s); {mask_note}", terrain.tiles.len(), t.elapsed().as_secs_f64()));

    // Meshes and textures.
    let t = Instant::now();
    // Loaded meshes by (mesh asset, object variation; 0 = none).
    let mut mesh_index: HashMap<((usize, usize), u32), Option<usize>> = HashMap::new();
    let mut texture_index: HashMap<u64, Option<usize>> = HashMap::new();
    let mut environments = Vec::new();
    match crate::environment::read(&fs, &world, crate::environment::DEFAULT_ENVIRONMENT, log) {
        Ok(base) => {
            for v in crate::environment::VARIANTS {
                match crate::environment::overlay(&fs, &world, &base, v, log) {
                    Ok(e) => environments.push(e),
                    Err(e) => log.line(format!("weather variant {v} not read: {e:#}")),
                }
            }
            environments.insert(0, base);
        }
        Err(e) => log.line(format!("environment not read: {e:#}")),
    }
    let mut loaded = Loaded { meshes: vec![], ground: vec![], placements: vec![], textures: vec![], terrain, centre: [0.0; 3], environments, scatter: vec![] };
    let mut failed_meshes = 0usize;
    let mut stats = DecodeStats::default();
    let (mut parts_without_texture, mut parts_computed_normals) = (0usize, 0usize);
    let folders = texture_folders(&world);
    let mut helper_meshes = 0usize;
    let mut guessed = 0usize;
    let mut uv_stats = UvStats::default();
    // Object variations (DESIGN.md "decal variants").
    let variation_db = read_variation_db(&world);
    let variation_names = read_variation_names(&world);
    // Damage-state mesh parts (DESIGN.md "damaged parts hidden at the start");
    // BF1_DAMAGED_PARTS=show draws them as before.
    let t_parts = Instant::now();
    let show_damaged = std::env::var("BF1_DAMAGED_PARTS").is_ok_and(|v| v == "show");
    let damaged = if show_damaged { HiddenParts::default() } else { hidden_parts(&world) };
    log.line(format!(
        "damaged parts: {} entities with part components, {} meshes hide damage-state parts ({} meshes where entities disagree; {:.2} s){}",
        damaged.entities,
        damaged.by_mesh.len(),
        damaged.disagreements,
        t_parts.elapsed().as_secs_f64(),
        if show_damaged { " (off: BF1_DAMAGED_PARTS=show)" } else { "" }
    ));
    // Placements per variant mesh: (count, index into loaded.meshes).
    let mut variant_counts: std::collections::BTreeMap<String, (usize, usize)> = std::collections::BTreeMap::new();
    let (mut variants_wanted, mut variants_unchanged, mut variants_switched_off) = (0usize, 0usize, 0usize);
    let t_prefetch = Instant::now();
    let prefetched = prefetch_textures(&fs, &world, &walker, opts, &variation_db, &folders);
    log.line(format!("textures unpacked ahead on all cores: {prefetched} ({:.2} s)", t_prefetch.elapsed().as_secs_f64()));

    for placed in &walker.meshes {
        let Some(at) = placed.mesh_at else { continue };
        // Meshes that only exist for lighting or visibility (light blockers, occluders,
        // Enlighten proxies) are never drawn in the game.
        if is_helper_mesh(&placed.mesh) {
            helper_meshes += 1;
            continue;
        }
        // The variant, if it changes a material of this mesh and the switch allows it.
        let variant = match placed.variation {
            0 => 0,
            _ if !variation_applies(&opts.variations, &placed.mesh) => {
                variants_switched_off += 1;
                0
            }
            v => {
                variants_wanted += 1;
                if variation_db.contains_key(&(at, v)) {
                    v
                } else {
                    variants_unchanged += 1;
                    0
                }
            }
        };
        let index = match mesh_index.get(&(at, variant)) {
            Some(i) => *i,
            None => {
                let overrides = variation_db.get(&(at, variant));
                let i = match load_mesh(&fs, &world, at, opts, overrides, &mut loaded.textures, &mut texture_index, &mut stats, &folders, &mut guessed, &mut uv_stats, &damaged.by_mesh) {
                    Ok(mut m) => {
                        if variant != 0 {
                            m.name = format!("{} (variant {})", m.name, variation_names.get(&variant).cloned().unwrap_or_else(|| variant.to_string()));
                        }
                        parts_without_texture += m.parts.iter().filter(|p| p.texture.is_none()).count();
                        parts_computed_normals += m.parts.iter().filter(|p| p.normals.is_empty()).count();
                        loaded.meshes.push(m);
                        Some(loaded.meshes.len() - 1)
                    }
                    Err(e) => {
                        failed_meshes += 1;
                        if failed_meshes <= 5 {
                            log.line(format!("  mesh FAIL {}: {e:#}", world.names[at.0]));
                        }
                        None
                    }
                };
                mesh_index.insert((at, variant), i);
                i
            }
        };
        if let Some(i) = index {
            let m = convert::transform(placed.transform.to_cols_array());
            loaded.placements.push((i, m.to_cols_array()));
            if variant != 0 {
                let e = variant_counts.entry(loaded.meshes[i].name.rsplit('/').next().unwrap_or("").to_string()).or_insert((0, i));
                e.0 += 1;
            }
        }
    }
    log.line(format!(
        "object variations (BF1_VARIATIONS={}): {} database entries that change a material; placements with a variant {} (left out by the switch {}), drawn with it {}, no entry or no change {}; {} variant meshes",
        opts.variations,
        variation_db.len(),
        variants_wanted + variants_switched_off,
        variants_switched_off,
        variant_counts.values().map(|v| v.0).sum::<usize>(),
        variants_unchanged,
        variant_counts.len()
    ));
    let mut top: Vec<_> = variant_counts.iter().collect();
    top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (name, (n, i)) in top.iter().take(20) {
        let textures: Vec<&str> = loaded.meshes[*i].parts.iter().filter_map(|p| p.texture).map(|t| loaded.textures[t].name.rsplit('/').next().unwrap_or("")).collect();
        log.line(format!("  {n:>4} x {name}: {}", textures.join(", ")));
    }
    // Why parts are grey, counted per placement (what you see), most common first.
    let mut grey: HashMap<String, (usize, std::collections::BTreeMap<String, usize>)> = HashMap::new();
    let (mut placed_parts, mut placed_grey) = (0usize, 0usize);
    for (i, _) in &loaded.placements {
        for p in &loaded.meshes[*i].parts {
            placed_parts += 1;
            if let Some(why) = &p.missing {
                placed_grey += 1;
                let e = grey.entry(why.clone()).or_default();
                e.0 += 1;
                *e.1.entry(loaded.meshes[*i].name.clone()).or_default() += 1;
            }
        }
    }
    let unused = PREFETCH.with(|p| std::mem::take(&mut *p.borrow_mut())).len();
    log.line(format!("textures unpacked ahead but not used by the meshes: {unused}"));
    log.line(format!(
        "time in mesh geometry {:.2} s, in textures {:.2} s (read, unpack, check)",
        GEOMETRY_NANOS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9,
        TEXTURE_NANOS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
    ));
    log.line(format!("materials without a texture list, colour texture guessed from the folder: {guessed}"));
    log.line(uv_stats.summary());
    let mut flat: Vec<_> = uv_stats.flat_by_mesh.iter().filter(|(_, a)| **a > 0.0).collect();
    flat.sort_by(|a, b| b.1.total_cmp(a.1));
    log.line(format!(
        "  most untextured surface (m2 per mesh, not counting copies): {}",
        flat.iter().take(12).map(|(n, a)| format!("{n} {a:.0}")).collect::<Vec<_>>().join(", ")
    ));
    for d in &uv_stats.debug {
        log.line(format!("  uv debug: {d}"));
    }
    log.line(format!("placed parts: {placed_parts}, grey: {placed_grey}"));
    // Damage-state triangles left out, per mesh and over all placements.
    let mut damaged_by_mesh: std::collections::BTreeMap<String, (usize, usize)> = std::collections::BTreeMap::new();
    for (i, _) in &loaded.placements {
        let m = &loaded.meshes[*i];
        if m.damaged_triangles > 0 {
            let e = damaged_by_mesh.entry(m.name.rsplit('/').next().unwrap_or("").to_string()).or_insert((0, m.damaged_triangles));
            e.0 += 1;
        }
    }
    let mut top: Vec<_> = damaged_by_mesh.iter().collect();
    top.sort_by(|a, b| (b.1 .0 * b.1 .1).cmp(&(a.1 .0 * a.1 .1)));
    log.line(format!(
        "damage-state triangles left out: {} meshes, {} placements, {} triangles in all placements; most: {}",
        damaged_by_mesh.len(),
        damaged_by_mesh.values().map(|v| v.0).sum::<usize>(),
        damaged_by_mesh.values().map(|v| v.0 * v.1).sum::<usize>(),
        top.iter().take(10).map(|(n, (c, t))| format!("{c} x {n} ({t})")).collect::<Vec<_>>().join(", ")
    ));
    // Terrain blend parts (DESIGN.md "terrain blend on mounds"): placements per
    // mesh and the values they use.
    let mut blend: std::collections::BTreeMap<String, (usize, String)> = std::collections::BTreeMap::new();
    for (i, _) in &loaded.placements {
        for p in &loaded.meshes[*i].parts {
            if let Some(b) = p.terrain_blend {
                let e = blend.entry(loaded.meshes[*i].name.rsplit('/').next().unwrap_or("").to_string()).or_default();
                e.0 += 1;
                e.1 = format!(
                    "up mask {}, threshold {}, offset {}, up normals {}, texture mask {}, textured {}",
                    b.up_mask,
                    b.threshold,
                    b.offset,
                    b.up_normals,
                    b.texture_mask,
                    p.texture.is_some() && p.tint.is_none() && p.detail.is_none()
                );
            }
        }
    }
    log.line(format!("terrain blend parts: {} placed in {} meshes", blend.values().map(|v| v.0).sum::<usize>(), blend.len()));
    for (name, (n, values)) in &blend {
        log.line(format!("  terrain blend {n:4} x {name}: {values}"));
    }
    let mut top: Vec<_> = grey.into_iter().collect();
    top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (why, (n, meshes)) in top.iter().take(25) {
        let mut m: Vec<_> = meshes.iter().collect();
        m.sort_by(|a, b| b.1.cmp(a.1));
        let ex: Vec<String> = m.iter().take(4).map(|(k, c)| format!("{c}x {}", k.rsplit('/').next().unwrap_or(k))).collect();
        log.line(format!("  grey x{n}: {why}; meshes: {}", ex.join(", ")));
    }
    log.line(format!("left out {helper_meshes} placements of helper meshes (lighting, visibility, gameplay)"));
    let mut hidden: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut coloured = 0usize;
    for (i, _) in &loaded.placements {
        for why in &loaded.meshes[*i].hidden {
            *hidden.entry(why.as_str()).or_default() += 1;
        }
        coloured += loaded.meshes[*i].parts.iter().filter(|p| p.colour.is_some()).count();
    }
    log.line(format!("placed parts left out: {hidden:?}; drawn with a plain colour: {coloured}"));
    // Decal parts (DESIGN.md "decal variants", step 8): placements per mesh,
    // texture, tile, opacity, colour multiplier; and per decal texture the
    // mean alpha on the middle lines (u or v = 0.5) against the whole: near 0
    // on the lines means a 2 x 2 atlas.
    let mut decal_parts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut decal_textures: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for (i, _) in &loaded.placements {
        for p in loaded.meshes[*i].parts.iter() {
            let Some(d) = p.decal else { continue };
            let tex = p.texture.map(|t| loaded.textures[t].name.rsplit('/').next().unwrap_or("").to_string()).unwrap_or_else(|| "no texture".into());
            let (u0, u1) = p.uvs.iter().fold((f32::MAX, f32::MIN), |(a, b), uv| (a.min(uv[0]), b.max(uv[0])));
            let (v0, v1) = p.uvs.iter().fold((f32::MAX, f32::MIN), |(a, b), uv| (a.min(uv[1]), b.max(uv[1])));
            let key = format!("{}: {tex}, uv {u0:.2}..{u1:.2} x {v0:.2}..{v1:.2}, opacity {:.2}, multiply {:?}", loaded.meshes[*i].name.rsplit('/').next().unwrap_or(""), d.opacity, d.multiply);
            *decal_parts.entry(key).or_default() += 1;
            if let Some(t) = p.texture {
                decal_textures.insert(t);
            }
        }
    }
    log.line(format!("decal parts placed: {} ({} kinds)", decal_parts.values().sum::<usize>(), decal_parts.len()));
    let mut top: Vec<_> = decal_parts.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    for (k, n) in top.iter().take(30) {
        log.line(format!("  {n:>4} x {k}"));
    }
    for t in decal_textures {
        if let Some((w, h, px)) = small_mip_sized(&loaded.textures[t], 128) {
            let a = |x: usize, y: usize| px[y * w + x][3] as f32 / 255.0;
            let whole = px.iter().map(|p| p[3] as f32 / 255.0).sum::<f32>() / px.len() as f32;
            let lines = ((0..h).map(|y| a(w / 2, y)).sum::<f32>() + (0..w).map(|x| a(x, h / 2)).sum::<f32>()) / (w + h) as f32;
            log.line(format!("  decal texture {}: {w}x{h} mip, mean alpha {whole:.3}, on the middle lines {lines:.3}", loaded.textures[t].name.rsplit('/').next().unwrap_or("")));
        }
    }
    // Decal sections (category "transparent decal"), drawn since step 5.
    let mut decal_meshes: std::collections::BTreeMap<&str, (usize, usize)> = std::collections::BTreeMap::new();
    for (i, _) in &loaded.placements {
        let m = &loaded.meshes[*i];
        if m.decal_sections > 0 {
            let e = decal_meshes.entry(m.name.rsplit('/').next().unwrap_or("")).or_insert((0, m.decal_sections));
            e.0 += 1;
        }
    }
    let mut top: Vec<_> = decal_meshes.iter().collect();
    top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    log.line(format!(
        "decal sections: {} meshes, {} placements, {} sections placed; most placed: {}",
        decal_meshes.len(),
        decal_meshes.values().map(|v| v.0).sum::<usize>(),
        decal_meshes.values().map(|v| v.0 * v.1).sum::<usize>(),
        top.iter().take(15).map(|(n, (c, s))| format!("{n} {c}x{s}")).collect::<Vec<_>>().join(", ")
    ));
    // Winding check: a triangle faces the viewer when its corners go counter-clockwise,
    // so its geometric normal must point the same way as its stored normals.
    let (mut agree, mut disagree) = (0usize, 0usize);
    for p in loaded.meshes.iter().flat_map(|m| &m.parts).filter(|p| !p.normals.is_empty()) {
        for t in p.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| bevy::math::Vec3::from(p.positions[i as usize]));
            let n: bevy::math::Vec3 = [t[0], t[1], t[2]].iter().map(|&i| bevy::math::Vec3::from(p.normals[i as usize])).sum();
            match (b - a).cross(c - a).dot(n) {
                d if d > 0.0 => agree += 1,
                d if d < 0.0 => disagree += 1,
                _ => {}
            }
        }
    }
    log.line(format!(
        "winding check: {agree} triangles face the way of their normals, {disagree} the other way ({:.1}% agree)",
        100.0 * agree as f64 / (agree + disagree).max(1) as f64
    ));
    let world_tris: usize =loaded.placements.iter().map(|(m, _)| loaded.meshes[*m].parts.iter().map(|p| p.indices.len() / 3).sum::<usize>()).sum();
    let unique_tris: usize = loaded.meshes.iter().map(|m| m.parts.iter().map(|p| p.indices.len() / 3).sum::<usize>()).sum();
    log.line(format!("triangles: {unique_tris} in unique meshes, {world_tris} in all placements"));
    let tex_bytes: usize = loaded.textures.iter().map(|t| t.data.len()).sum();
    log.line(format!(
        "meshes: {} loaded, {} failed; parts: {}, without texture {}, normals computed {}; placements: {} ({:.2} s)",
        loaded.meshes.len(),
        failed_meshes,
        loaded.meshes.iter().map(|m| m.parts.len()).sum::<usize>(),
        parts_without_texture,
        parts_computed_normals,
        loaded.placements.len(),
        t.elapsed().as_secs_f64()
    ));
    log.line(format!("textures: {} ({:.0} MB of GPU data)", loaded.textures.len(), tex_bytes as f64 / 1e6));
    // Alpha study: textures whose alpha has many clear and many solid pixels.
    let mut by_suffix: std::collections::BTreeMap<String, (usize, usize)> = std::collections::BTreeMap::new();
    for t in &loaded.textures {
        let suffix = t.name.rsplit('_').next().unwrap_or("").to_string();
        let e = by_suffix.entry(suffix).or_default();
        e.0 += 1;
        if t.alpha[0] >= 0.05 && t.alpha[2] >= 0.05 {
            e.1 += 1;
        }
    }
    log.line(format!("textures by name ending (count, with 5%+ clear and 5%+ solid alpha): {by_suffix:?}"));
    // Brightest textures by mean colour (to spot overly light materials).
    let mut bright: Vec<(f32, [f32; 3], &str)> = loaded.textures.iter().filter_map(|t| mean_colour(t).map(|m| ((m[0] + m[1] + m[2]) / 3.0, m, t.name.as_str()))).collect();
    bright.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, m, n) in bright.iter().take(12) {
        log.line(format!("  bright texture: mean sRGB [{:.2}, {:.2}, {:.2}]  {n}", m[0], m[1], m[2]));
    }
    for (_, m, n) in bright.iter().filter(|b| b.2.contains("bunker")) {
        log.line(format!("  bunker texture: mean sRGB [{:.2}, {:.2}, {:.2}]  {n}", m[0], m[1], m[2]));
    }
    log.line(format!("cut-out textures (alpha test): {}", loaded.textures.iter().filter(|t| t.cutout).count()));
    for t in loaded.textures.iter().filter(|t| t.cutout) {
        log.line(format!(
            "  cut-out: alpha clear {:3.0}% middle {:3.0}% solid {:3.0}%  {:?}  {}",
            t.alpha[0] * 100.0,
            t.alpha[1] * 100.0,
            t.alpha[2] * 100.0,
            t.format,
            t.name
        ));
    }

    // Centre: median position of the placed meshes, on the ground.
    let mut xs: Vec<f32> = walker.meshes.iter().map(|m| m.transform.translation.x).collect();
    let mut zs: Vec<f32> = walker.meshes.iter().map(|m| m.transform.translation.z).collect();
    xs.sort_by(|a, b| a.total_cmp(b));
    zs.sort_by(|a, b| a.total_cmp(b));
    let (cx, cz) = (xs[xs.len() / 2], zs[zs.len() / 2]);
    let cy = loaded.terrain.height_at(cx, cz).unwrap_or(0.0);
    loaded.centre = convert::point([cx, cy, cz]);
    log.line(format!("battle area centre (Frostbite x, y, z): {cx:.1}, {cy:.1}, {cz:.1}"));
    // Landmarks to compare with the game's deploy map (village houses, balloons).
    for key in ["houserural", "balloon"] {
        let at: Vec<_> = walker.meshes.iter().filter(|m| m.mesh.to_ascii_lowercase().contains(key)).map(|m| m.transform.translation).collect();
        if !at.is_empty() {
            let n = at.len() as f32;
            let (x, z) = (at.iter().map(|p| p.x).sum::<f32>() / n, at.iter().map(|p| p.z).sum::<f32>() / n);
            log.line(format!("landmark {key}: {} placements, mean Frostbite x {x:.0}, z {z:.0}", at.len()));
        }
    }
    let names: Vec<String> = loaded.meshes.iter().map(|m| m.name.clone()).collect();
    log_largest(log, &loaded, &names);

    // Ground layer textures, listed by the level's terrain shaders.
    let t = Instant::now();
    match load_ground_textures(&fs, &world, opts, &mut loaded.textures, log) {
        Ok(g) => loaded.ground = g,
        Err(e) => log.line(format!("ground textures not loaded: {e:#}")),
    }
    log.line(format!("ground textures: {} loaded ({:.2} s)", loaded.ground.len(), t.elapsed().as_secs_f64()));

    // Terrain mesh scattering: types per layer (EBX) + mesh names and distances
    // (the terrain's visual resource), and the scatter meshes themselves.
    let t = Instant::now();
    match scatter_types(&fs, &world) {
        Ok(found) => {
            for (layer, identifier, mesh_name, density, min_scale, max_scale, distance) in found {
                let Some(part) = world.partition_by_name(&mesh_name) else {
                    log.line(format!("  scatter type {identifier}: mesh not in the loaded bundles: {mesh_name}"));
                    continue;
                };
                // The same mesh can serve several types: load it once.
                let mesh = match loaded.meshes.iter().position(|m| m.name.eq_ignore_ascii_case(&world.names[part])) {
                    Some(i) => i,
                    None => match load_mesh(&fs, &world, (part, 0), opts, None, &mut loaded.textures, &mut texture_index, &mut stats, &folders, &mut guessed, &mut uv_stats, &damaged.by_mesh) {
                        Ok(mut m) => {
                            // Shader-graph materials keep their textures in the
                            // shader: take the colour texture from the shader database.
                            for p in m.parts.iter_mut().filter(|p| p.texture.is_none()) {
                                let shader = material_shader(&world, (part, 0), p.material_id);
                                match shader_colour_texture(&fs, &world, opts, &shader) {
                                    Some(tex) => {
                                        p.texture = load_texture(&fs, &world, tex, opts.texture_mip_skip, &mut loaded.textures, &mut texture_index);
                                        p.missing = None;
                                        log.line(format!("  scatter mesh {}: colour texture {} from the shader database ({shader})", mesh_name.rsplit('/').next().unwrap_or(""), world.names[tex.0]));
                                    }
                                    None => log.line(format!("  scatter mesh {}: no colour texture for shader {shader}", mesh_name.rsplit('/').next().unwrap_or(""))),
                                }
                            }
                            loaded.meshes.push(m);
                            loaded.meshes.len() - 1
                        }
                        Err(e) => {
                            log.line(format!("  scatter type {identifier}: mesh failed: {mesh_name}: {e:#}"));
                            continue;
                        }
                    },
                };
                // Scatter billboards (backdrop trees) have soft-edged alpha masks
                // that the leaf-card rule misses: at least 20% clear pixels means
                // cut-out. Not for `_CS` textures, whose alpha is smoothness.
                for p in &loaded.meshes[mesh].parts {
                    if let Some(t) = p.texture {
                        let tex = &mut loaded.textures[t];
                        if !tex.cutout && tex.alpha[0] >= 0.2 && !tex.name.to_ascii_lowercase().ends_with("_cs") {
                            tex.cutout = true;
                            log.line(format!("  scatter texture {}: cut-out ({:.0}% clear pixels)", tex.name.rsplit('/').next().unwrap_or(""), tex.alpha[0] * 100.0));
                        }
                    }
                }
                // Grass blades without a texture take the ground colour, tinted (`TerrainBlend`, `Color1`).
                let ground_tint = loaded.meshes[mesh].parts.iter().filter(|p| p.texture.is_none()).find_map(|p| {
                    let shader_mat = material_at(&world, (part, 0), p.material_id)?;
                    let blend = material_vector(&world, shader_mat, "TerrainBlend")?;
                    (blend[0] > 0.5).then(|| material_vector(&world, shader_mat, "Color1").map(|c| [c[0], c[1], c[2]]).unwrap_or([1.0; 3]))
                });
                let m = &loaded.meshes[mesh];
                let parts: Vec<String> = m
                    .parts
                    .iter()
                    .map(|p| match p.texture {
                        Some(i) => format!("{}{}", loaded.textures[i].name.rsplit('/').next().unwrap_or(""), if loaded.textures[i].cutout { " (cut-out)" } else { "" }),
                        None => "no texture".into(),
                    })
                    .collect();
                log.line(format!(
                    "  scatter layer {layer:2} type {identifier:2}: {} ({} parts: {}{}), {density} per m2, scale {min_scale:?}..{max_scale:?}, draw distance {distance} m",
                    mesh_name.rsplit('/').next().unwrap_or(&mesh_name),
                    m.parts.len(),
                    parts.join(", "),
                    ground_tint.map(|t| format!("; ground colour x {t:.2?}")).unwrap_or_default()
                ));
                loaded.scatter.push(ScatterType { layer, identifier, mesh, density, min_scale, max_scale, distance, ground_tint });
            }
        }
        Err(e) => log.line(format!("terrain mesh scattering not read: {e:#}")),
    }
    log.line(format!("terrain mesh scattering: {} types ({:.2} s)", loaded.scatter.len(), t.elapsed().as_secs_f64()));
    convert_normal_maps(&mut loaded, log);
    log_files_used(&fs, log);
    SHADER_DB.with(|c| c.borrow_mut().take());
    Ok(loaded)
}

/// Which game files this run read: every `.toc` and `cas.cat` in `Data` and
/// `Patch` (indexes), the level's `.sb`, and only some `.cas` archives (the
/// data). Logged per install folder with sizes, for the README.
fn log_files_used(fs: &GameFs, log: &GameLog) {
    let used = fs.cas_used.lock().map(|u| u.clone()).unwrap_or_default();
    let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    // Per folder (the cas file's parent, relative to the game folder): (used files, used bytes, all files, all bytes).
    let mut per: std::collections::BTreeMap<String, (usize, u64, usize, u64)> = std::collections::BTreeMap::new();
    for (i, p) in fs.cas_files.iter().enumerate() {
        let dir = p.parent().and_then(|d| d.strip_prefix(&fs.root).ok()).map(|d| d.display().to_string()).unwrap_or_default();
        let e = per.entry(dir).or_default();
        let n = size(p);
        e.2 += 1;
        e.3 += n;
        if used.contains(&(i as u32)) {
            e.0 += 1;
            e.1 += n;
        }
    }
    let gb = |b: u64| b as f64 / 1e9;
    let (u_files, u_bytes, a_files, a_bytes) = per.values().fold((0, 0, 0, 0), |a, e| (a.0 + e.0, a.1 + e.1, a.2 + e.2, a.3 + e.3));
    log.line(format!(
        "game files read: {u_files} of {a_files} .cas archives ({:.1} of {:.1} GB) in {} of {} folders, plus every .toc and cas.cat in Data and Patch and the level's .sb",
        gb(u_bytes),
        gb(a_bytes),
        per.values().filter(|e| e.0 > 0).count(),
        per.len()
    ));
    for (dir, e) in per.iter().filter(|(_, e)| e.0 > 0) {
        log.line(format!("  {dir}: {} of {} .cas ({:.2} of {:.2} GB)", e.0, e.2, gb(e.1), gb(e.3)));
    }
}

/// The mesh's material `id` (the MeshMaterial asset).
fn material_at(world: &World, mesh: (usize, usize), id: i32) -> Option<(usize, usize)> {
    let Some(Value::Array { items, .. }) = world.get(mesh).get("Materials") else { return None };
    let Some(Value::Pointer(p)) = items.get(id as usize) else { return None };
    world.resolve(mesh.0, p)
}

/// A material's vector shader parameter by name (x, y, z, w).
fn material_vector(world: &World, mat: (usize, usize), name: &str) -> Option<[f32; 4]> {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
    let Some(Value::Array { items, .. }) = sh.get("VectorParameters") else { return None };
    items.iter().find_map(|v| {
        let Value::Struct(v) = v else { return None };
        if !matches!(v.get("ParameterName"), Some(Value::String(n)) if n == name) {
            return None;
        }
        let Some(Value::Struct(x)) = v.get("Value") else { return None };
        let f = |k: &str| match x.get(k) {
            Some(Value::Float(f)) => Some(*f as f32),
            _ => None,
        };
        Some([f("x")?, f("y")?, f("z")?, f("w")?])
    })
}

thread_local! {
    /// The level's shader database (159 MB on Verdun), read once while
    /// loading and dropped at the end of `load_level` (faster start).
    static SHADER_DB: std::cell::RefCell<Option<std::rc::Rc<Vec<u8>>>> = const { std::cell::RefCell::new(None) };
}

/// The level's shader database bytes (read on first use, then kept).
fn shader_db(fs: &GameFs, world: &World, opts: &Options) -> Result<std::rc::Rc<Vec<u8>>> {
    if let Some(db) = SHADER_DB.with(|c| c.borrow().clone()) {
        return Ok(db);
    }
    let db = std::rc::Rc::new(world.read_res_named(fs, &format!("{}/shaderdb", opts.level))?);
    SHADER_DB.with(|c| *c.borrow_mut() = Some(db.clone()));
    Ok(db)
}

/// The colour texture that a shader-graph shader uses, from the level's
/// shader database (texture names ending `_C`, `_C_Mip`, `_CA` or `_CS`).
fn shader_colour_texture(fs: &GameFs, world: &World, opts: &Options, shader: &str) -> Option<(usize, usize)> {
    use fb_res::shaderdb::{fb_hash, ShaderDb};
    let db = shader_db(fs, world, opts).ok()?;
    let sdb = ShaderDb::parse(&db, fb_hash(&shader.to_lowercase())).ok()?;
    let mut names: Vec<String> = sdb.textures_of(shader)?.into_iter().collect();
    names.sort();
    let colour = |n: &str| {
        let l = n.to_ascii_lowercase();
        let l = l.strip_suffix("_mip").unwrap_or(&l).to_string();
        ["_c", "_ca", "_cs"].iter().any(|s| l.ends_with(s))
    };
    names.iter().filter(|n| colour(n)).find_map(|n| world.partition_by_name(n).map(|p| (p, 0)))
}

/// (layer, identifier, mesh name, density, min scale, max scale, draw distance)
type ScatterFound = (u8, u32, String, f32, [f32; 2], [f32; 2], f32);

/// Terrain mesh scattering types per ground layer: `TerrainData.TerrainLayers[n]
/// .MeshScatteringTypes` (EBX), joined by identifier with the terrain's
/// `VisualResource` records (mesh name, distances).
fn scatter_types(fs: &GameFs, world: &World) -> Result<Vec<ScatterFound>> {
    let part = (0..world.partitions.len())
        .find(|&i| world.partitions[i].primary().is_some_and(|p| p.value.type_name == "TerrainData"))
        .context("no TerrainData")?;
    let terrain = world.get((part, 0));
    let Some(Value::ResourceRef(rid)) = terrain.get("VisualResource") else { anyhow::bail!("TerrainData has no VisualResource") };
    let records = fb_terrain::scatter::parse_visual_resource(&world.read_res(fs, *rid)?);
    let Some(Value::Array { items: layers, .. }) = terrain.get("TerrainLayers") else { anyhow::bail!("TerrainData has no TerrainLayers") };
    let num = |v: Option<&Value>| match v {
        Some(Value::Float(f)) => Some(*f as f32),
        Some(Value::Int(i)) => Some(*i as f32),
        Some(Value::UInt(u)) => Some(*u as f32),
        _ => None,
    };
    let pair = |v: Option<&Value>| match v {
        Some(Value::Struct(c)) => Some([num(c.get("x"))?, num(c.get("y"))?]),
        _ => None,
    };
    let mut out = Vec::new();
    for (layer, item) in layers.iter().enumerate() {
        let Value::Pointer(p) = item else { continue };
        let Some(layer_at) = world.resolve(part, p) else { continue };
        let Some(Value::Array { items: types, .. }) = world.get(layer_at).get("MeshScatteringTypes") else { continue };
        for t in types {
            let Value::Pointer(tp) = t else { continue };
            let Some(type_at) = world.resolve(layer_at.0, tp) else { continue };
            let ty = world.get(type_at);
            let Some(identifier) = num(ty.get("Identifier")).map(|v| v as u32) else { continue };
            let density = match ty.get("Density") {
                Some(Value::Struct(d)) => num(d.get("High")).unwrap_or(0.0),
                _ => 0.0,
            };
            let (Some(min_scale), Some(max_scale)) = (pair(ty.get("MinScale")), pair(ty.get("MaxScale"))) else { continue };
            let Some(record) = records.get(identifier as usize) else { continue };
            let distance = record.draw_distance().unwrap_or(30.0);
            out.push((layer as u8, identifier, record.mesh.clone(), density, min_scale, max_scale, distance));
        }
    }
    Ok(out)
}

/// The colour textures (`_CS`) of the level's terrain shaders, from the shader
/// database: each compiled terrain shader lists its layer textures, then its
/// constants, which include a `maskMap` slot. Loaded like mesh textures.
fn load_ground_textures(
    fs: &GameFs,
    world: &World,
    opts: &Options,
    textures: &mut Vec<TextureData>,
    log: &GameLog,
) -> Result<Vec<GroundTexture>> {
    let t = Instant::now();
    let db = shader_db(fs, world, opts)?;
    let mut names = terrain_texture_names(&db);
    log.line(format!(
        "shader database: {:.0} MB read in {:.2} s; terrain colour textures listed: {}",
        db.len() as f64 / 1e6,
        t.elapsed().as_secs_f64(),
        names.len()
    ));
    // Layer number -> main colour texture (STATUS.md update 8).
    let t = Instant::now();
    let (layer_map, normal_map) = match terrain_layer_map(fs, world, &db) {
        Ok((map, normals, summary)) => {
            log.line(format!("terrain layer textures ({:.2} s): {summary}", t.elapsed().as_secs_f64()));
            for (l, tex) in &map {
                let nrh = normals.get(&tex.to_ascii_lowercase()).map(|n| n.rsplit('/').next().unwrap_or(n)).unwrap_or("no _NRH");
                log.line(format!("  layer {l:2}: {} + {nrh}", tex.rsplit('/').next().unwrap_or(tex)));
            }
            (map, normals)
        }
        Err(e) => {
            log.line(format!("terrain layer textures not found: {e:#}"));
            Default::default()
        }
    };
    for tex in layer_map.values() {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(tex)) {
            names.push(tex.clone());
        }
    }
    // Unpack the colour and _NRH textures on all cores first (faster start);
    // the loop below takes them from there.
    let mut wanted = Vec::new();
    for name in names.iter().chain(names.iter().filter_map(|n| normal_map.get(&n.to_ascii_lowercase()))) {
        if let Some(part) = world.partition_by_name(name) {
            if let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("Resource") {
                if !wanted.iter().any(|(k, _)| *k == (*rid, opts.ground_mip_skip)) {
                    wanted.push(((*rid, opts.ground_mip_skip), (part, 0)));
                }
            }
        }
    }
    unpack_ahead(fs, world, &wanted);
    let mut out = Vec::new();
    // Own cache: the ground textures load at their own mip (full size by default).
    let mut ground_index = HashMap::new();
    for name in names {
        let Some(part) = world.partition_by_name(&name) else {
            log.line(format!("  ground texture not in the loaded bundles: {name}"));
            continue;
        };
        let Some(i) = load_texture(fs, world, (part, 0), opts.ground_mip_skip, textures, &mut ground_index) else {
            log.line(format!("  ground texture failed: {name}"));
            continue;
        };
        let tex = &textures[i];
        let Some(mean) = mean_colour(tex) else {
            log.line(format!("  ground texture {name}: no mean colour ({:?})", tex.format));
            continue;
        };
        log.line(format!(
            "  ground texture {name}: {}x{} {:?}, {} mips, mean colour (sRGB) [{:.2}, {:.2}, {:.2}]",
            tex.width, tex.height, tex.format, tex.mip_count, mean[0], mean[1], mean[2]
        ));
        let layers = layer_map.iter().filter(|(_, t)| t.eq_ignore_ascii_case(&name)).map(|(l, _)| *l).collect();
        let nrh = normal_map.get(&name.to_ascii_lowercase()).and_then(|n| {
            let loaded = world.partition_by_name(n).and_then(|part| load_texture(fs, world, (part, 0), opts.ground_mip_skip, textures, &mut ground_index));
            match loaded {
                Some(j) => log.line(format!("    {}: {}x{} {:?}, {} mips", n.rsplit('/').next().unwrap_or(n), textures[j].width, textures[j].height, textures[j].format, textures[j].mip_count)),
                None => log.line(format!("    {n}: not loaded")),
            }
            loaded
        });
        out.push(GroundTexture { name, texture: i, layers, nrh });
    }
    PREFETCH.with(|p| p.borrow_mut().clear());
    Ok(out)
}

/// Layer number, colour and normal texture maps, and a summary for the log.
type LayerMaps = (std::collections::BTreeMap<u8, String>, std::collections::BTreeMap<String, String>, String);

/// Layer number -> main colour texture name, from the terrain's layer
/// combinations (shader names with their layers) and the shader database.
/// Also: the `_NRH` texture of each colour texture (lower-case name -> name).
fn terrain_layer_map(fs: &GameFs, world: &World, db: &[u8]) -> Result<LayerMaps> {
    use fb_res::shaderdb::{fb_hash, terrain_colour_shaders, terrain_layer_normals, terrain_layer_textures, ShaderDb};
    let part = (0..world.partitions.len())
        .find(|&i| world.partitions[i].primary().is_some_and(|p| p.value.type_name == "TerrainData"))
        .context("no TerrainData")?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("TerrainLayerCombinationsResource") else {
        anyhow::bail!("TerrainData has no TerrainLayerCombinationsResource")
    };
    let lc = world.read_res(fs, *rid)?;
    let shaders = terrain_colour_shaders(&lc);
    let first = shaders.first().context("no colour terrain shaders in the layer combinations")?;
    let sdb = ShaderDb::parse(db, fb_hash(&first.0.to_lowercase()))?;
    let (map, st) = terrain_layer_textures(&sdb, &shaders);
    let (normals, disagree) = terrain_layer_normals(&sdb, &shaders);
    let summary = format!(
        "{}; {} colour shader names, {} found; layers without a texture: {:?}; _NRH pairs: {} ({disagree} votes disagree)",
        sdb.summary(),
        st.names,
        st.names_found,
        st.layers_without,
        normals.len()
    );
    // Every other texture the terrain shaders use (not a layer's _CS or
    // _NRH), with the number of shaders that use it.
    let mut other: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (name, _) in &shaders {
        for t in sdb.textures_of(name).unwrap_or_default() {
            let l = t.to_ascii_lowercase();
            if !l.ends_with("_cs") && !l.ends_with("_nrh") {
                *other.entry(t).or_default() += 1;
            }
        }
    }
    let other: Vec<String> = other.iter().map(|(t, n)| format!("{t} ({n})")).collect();
    let summary = format!("{summary}; other textures of the terrain shaders: [{}]", other.join(", "));
    Ok((map, normals, summary))
}

/// Colour texture names (`.../TerrainTextures/..._CS`) in the terrain shader
/// entries of a shader database, in first-seen order.
fn terrain_texture_names(db: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut group: Vec<String> = Vec::new();
    let mut in_textures = false;
    let mut start = None;
    for (i, &b) in db.iter().chain(std::iter::once(&0u8)).enumerate() {
        if (0x20..0x7f).contains(&b) {
            start.get_or_insert(i);
            continue;
        }
        let Some(s0) = start.take() else { continue };
        if i - s0 < 5 {
            continue;
        }
        let s = String::from_utf8_lossy(&db[s0..i]).into_owned();
        if s.contains('/') {
            // A texture name: a new list starts after any constant.
            if !in_textures {
                group.clear();
                in_textures = true;
            }
            group.push(s);
        } else {
            in_textures = false;
            if s == "maskMap" {
                for g in group.drain(..) {
                    let lower = g.to_ascii_lowercase();
                    if lower.contains("terraintextures/") && lower.ends_with("_cs") && !out.contains(&g) {
                        out.push(g);
                    }
                }
            }
        }
    }
    out
}

/// Mean sRGB colour (0 to 1) of a block-compressed texture, from its first
/// mip that is 32 pixels wide or smaller.
fn mean_colour(t: &TextureData) -> Option<[f32; 3]> {
    let px = small_mip(t, 32)?;
    let mut sum = [0f64; 3];
    for p in &px {
        for c in 0..3 {
            sum[c] += p[c] as f64;
        }
    }
    let n = (px.len() as f64) * 255.0;
    Some(sum.map(|s| (s / n) as f32))
}

/// Share of pixels with alpha below 25%, between, and above 75%, from the
/// first mip that is 64 pixels wide or smaller.
fn alpha_shares(t: &TextureData) -> [f32; 3] {
    let Some(px) = small_mip(t, 64) else { return [0.0, 0.0, 1.0] };
    let mut c = [0usize; 3];
    for p in &px {
        c[if p[3] < 64 { 0 } else if p[3] <= 192 { 1 } else { 2 }] += 1;
    }
    let n = px.len().max(1) as f32;
    c.map(|v| v as f32 / n)
}

/// RGBA pixels of the first mip that is `max_width` pixels wide or smaller.
fn small_mip(t: &TextureData, max_width: usize) -> Option<Vec<[u8; 4]>> {
    small_mip_sized(t, max_width).map(|(_, _, px)| px)
}

/// Like `small_mip`, with the mip's width and height.
fn small_mip_sized(t: &TextureData, max_width: usize) -> Option<(usize, usize, Vec<[u8; 4]>)> {
    let block_bytes = match t.format {
        BlockFormat::Bc1 { .. } => 8,
        BlockFormat::Bc3 { .. } | BlockFormat::Bc7 { .. } => 16,
        BlockFormat::Bc5 => return None,
    };
    let (mut w, mut h, mut offset) = (t.width as usize, t.height as usize, 0usize);
    for _ in 0..t.mip_count {
        let size = w.div_ceil(4) * h.div_ceil(4) * block_bytes;
        if w <= max_width || offset + size >= t.data.len() {
            let data = t.data.get(offset..offset + size)?;
            let (bw, bh) = (w.div_ceil(4) * 4, h.div_ceil(4) * 4);
            let mut px = vec![0u32; bw * bh];
            match t.format {
                BlockFormat::Bc1 { .. } => texture2ddecoder::decode_bc1(data, bw, bh, &mut px).ok()?,
                BlockFormat::Bc3 { .. } => texture2ddecoder::decode_bc3(data, bw, bh, &mut px).ok()?,
                BlockFormat::Bc7 { .. } => texture2ddecoder::decode_bc7(data, bw, bh, &mut px).ok()?,
                BlockFormat::Bc5 => return None,
            }
            // Decoder pixels are bytes [b, g, r, a]; keep the w x h part.
            let mut out = Vec::with_capacity(w * h);
            for y in 0..h {
                for x in 0..w {
                    let b = px[y * bw + x].to_le_bytes();
                    out.push([b[2], b[1], b[0], b[3]]);
                }
            }
            return Some((w, h, out));
        }
        offset += size;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    None
}

/// The terrain, and a line for the log about its layer masks.
fn load_terrain(fs: &GameFs, world: &World) -> Result<(fb_terrain::Terrain, String)> {
    let part = (0..world.partitions.len())
        .find(|&i| world.partitions[i].primary().is_some_and(|p| p.value.type_name == "TerrainData"))
        .context("no TerrainData")?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("TerrainStreamingTreeResource") else {
        anyhow::bail!("TerrainData has no TerrainStreamingTreeResource")
    };
    let tree = world.read_res(fs, *rid)?;
    let (terrain, stats) = fb_terrain::Terrain::load(
        &tree,
        |id| world.chunks.contains_key(id) || fs.toc_chunks.contains_key(id),
        |id| world.read_chunk(fs, id),
    )?;
    let note = format!(
        "patches whose layers are not in ascending order: {}",
        stats.masks.as_ref().map_or("?".to_string(), |m| m.patches_not_ascending.to_string())
    );
    Ok((terrain, note))
}

/// Material changes of the level's object variations (DESIGN.md "decal
/// variants"): for each (mesh asset, variant `NameHash`), the
/// `MeshMaterialVariation` that replaces each changed mesh material. Read from
/// every loaded `MeshVariationDatabase`; entries that change nothing are left
/// out.
type VariationDb = HashMap<((usize, usize), u32), HashMap<(usize, usize), (usize, usize)>>;

fn read_variation_db(world: &World) -> VariationDb {
    let mut db = VariationDb::new();
    // ObjectVariation partitions by NameHash (for redirect entries).
    let mut variation_parts: HashMap<u32, usize> = HashMap::new();
    for part in 0..world.partitions.len() {
        if let Some(p) = world.partitions[part].primary().filter(|p| p.value.type_name == "ObjectVariation") {
            if let Some(Value::UInt(h)) = p.value.get("NameHash") {
                variation_parts.insert(*h as u32, part);
            }
        }
    }
    for part in 0..world.partitions.len() {
        let Some(primary) = world.partitions[part].primary() else { continue };
        if primary.value.type_name != "MeshVariationDatabase" {
            continue;
        }
        let Some(Value::Array { items, .. }) = primary.value.get("Entries") else { continue };
        for e in items {
            let Value::Struct(e) = e else { continue };
            let hash = match e.get("VariationAssetNameHash") {
                Some(Value::UInt(h)) => *h as u32,
                Some(Value::Int(h)) => *h as u32,
                _ => 0,
            };
            let (Some(Value::Pointer(mesh)), Some(Value::Array { items: materials, .. })) = (e.get("Mesh"), e.get("Materials")) else { continue };
            let Some(mesh) = world.resolve(part, mesh).filter(|_| hash != 0) else { continue };
            let mut changes = HashMap::new();
            for m in materials {
                let Value::Struct(m) = m else { continue };
                let (Some(Value::Pointer(a)), Some(Value::Pointer(v))) = (m.get("Material"), m.get("MaterialVariation")) else { continue };
                if let (Some(a), Some(v)) = (world.resolve(part, a), world.resolve(part, v)) {
                    changes.insert(a, v);
                }
            }
            if !changes.is_empty() {
                db.entry((mesh, hash)).or_default().extend(changes);
            }
        }
        // Redirect entries name only (mesh, hash); the material change lives
        // elsewhere. Fallback (our rule): a mesh with one material and a
        // variant asset with exactly one `MeshMaterialVariation` pair up.
        let Some(Value::Array { items: redirects, .. }) = primary.value.get("RedirectEntries") else { continue };
        for e in redirects {
            let Value::Struct(e) = e else { continue };
            let hash = match e.get("VariationAssetNameHash") {
                Some(Value::UInt(h)) => *h as u32,
                Some(Value::Int(h)) => *h as u32,
                _ => 0,
            };
            let Some(Value::Pointer(mesh)) = e.get("Mesh") else { continue };
            let Some(mesh) = world.resolve(part, mesh).filter(|m| hash != 0 && !db.contains_key(&(*m, hash))) else { continue };
            let Some(Value::Array { items: mats, .. }) = world.get(mesh).get("Materials") else { continue };
            let [Value::Pointer(only)] = mats.as_slice() else { continue };
            let Some(only) = world.resolve(mesh.0, only) else { continue };
            let Some(vpart) = variation_parts.get(&hash) else { continue };
            let vars: Vec<usize> = (0..world.partitions[*vpart].instances.len()).filter(|&i| world.get((*vpart, i)).type_name == "MeshMaterialVariation").collect();
            if let [v] = vars.as_slice() {
                db.entry((mesh, hash)).or_default().insert(only, (*vpart, *v));
            }
        }
    }
    db
}

/// Short names of the loaded `ObjectVariation` assets, by `NameHash`.
fn read_variation_names(world: &World) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    for part in 0..world.partitions.len() {
        let Some(primary) = world.partitions[part].primary().filter(|p| p.value.type_name == "ObjectVariation") else { continue };
        let hash = match primary.value.get("NameHash") {
            Some(Value::UInt(h)) => *h as u32,
            Some(Value::Int(h)) => *h as u32,
            _ => continue,
        };
        out.insert(hash, world.names[part].rsplit('/').next().unwrap_or("").to_string());
    }
    out
}

/// Whether the `BF1_VARIATIONS` mode applies to this mesh.
fn variation_applies(mode: &str, mesh: &str) -> bool {
    match mode {
        "all" => true,
        "off" => false,
        // Both `artassets/decals/` and `xpack1/artassets/decal/`.
        _ => mesh.to_ascii_lowercase().contains("artassets/decal"),
    }
}

fn load_mesh(
    fs: &GameFs,
    world: &World,
    at: (usize, usize),
    opts: &Options,
    // Object variation: mesh material -> the material variation that replaces it.
    variant: Option<&HashMap<(usize, usize), (usize, usize)>>,
    textures: &mut Vec<TextureData>,
    texture_index: &mut HashMap<u64, Option<usize>>,
    stats: &mut DecodeStats,
    folders: &TextureFolders,
    guessed: &mut usize,
    uv_stats: &mut UvStats,
    // Damage-state parts to leave out (DESIGN.md "damaged parts hidden at the start").
    hidden_parts: &HashMap<(usize, usize), std::collections::BTreeSet<u32>>,
) -> Result<LoadedMesh> {
    let asset = world.get(at);
    let hide = hidden_parts.get(&at);
    let t0 = Instant::now();
    let Some(Value::ResourceRef(rid)) = asset.get("MeshSetResource") else { anyhow::bail!("no MeshSetResource") };
    let ms = parse_meshset(&world.read_res(fs, *rid)?)?;
    let lod = ms.lods.first().context("no LODs")?;
    let chunk = lod.chunk_id.context("inline mesh data (not supported yet)")?;
    let g = decode_lod(lod, &world.read_chunk(fs, &chunk)?, stats)?;
    add_time(&GEOMETRY_NANOS, t0);

    // Each material and the variant material that replaces it (if any).
    let sources: Vec<Option<((usize, usize), Option<(usize, usize)>)>> = match asset.get("Materials") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .map(|m| {
                let Value::Pointer(p) = m else { return None };
                let mat = world.resolve(at.0, p)?;
                Some((mat, variant.and_then(|v| v.get(&mat)).copied()))
            })
            .collect(),
        _ => vec![],
    };

    // Base colour texture of each material.
    let looks: Vec<Look> = match asset.get("Materials") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .map(|m| {
                let Value::Pointer(p) = m else { return Look::Missing("material is not a link".into()) };
                let Some(mat) = world.resolve(at.0, p) else { return Look::Missing("material not loaded".into()) };
                // A variant replaces the material's textures and values; what it
                // does not set comes from the mesh material.
                let var = variant.and_then(|v| v.get(&mat)).copied();
                if is_effect_material(world, mat) {
                    return Look::Hide("effect");
                }
                if is_damage_card(world, mat) {
                    return Look::Hide("damage card");
                }
                if is_terrain_coloured(world, mat) {
                    return Look::Terrain;
                }
                // A variant with its own shader and no colour texture (the
                // puddle): the shader database may name one; else the part is
                // left out rather than drawn with the mesh's texture.
                let var_texture = match var {
                    Some(v) if base_color_texture(world, v).is_none() && shader_name(world, v).is_some_and(|sv| Some(&sv) != shader_name(world, mat).as_ref()) => {
                        match shader_name(world, v).and_then(|sv| shader_colour_texture(fs, world, opts, &sv)) {
                            Some(t) => Some(t),
                            None => return Look::Hide("variant without a texture"),
                        }
                    }
                    Some(v) => base_color_texture(world, v),
                    None => None,
                };
                let tex = match var_texture.or_else(|| base_color_texture(world, mat)) {
                    Some(t) => t,
                    None => {
                        if let Some(c) = plain_colour(world, mat) {
                            return Look::Colour(c);
                        }
                        // Glass names no texture: drawn see-through rather than with a
                        // guessed texture (DESIGN.md "window glass"). BF1_GLASS=off: old way.
                        if opts.glass {
                            if let Some(g) = glass_look(world, mat, var) {
                                return Look::Glass(g);
                            }
                        }
                        let Some(guess) = guess_colour_texture(world, folders, at, mat) else { return Look::Missing(material_summary(world, mat)) };
                        *guessed += 1;
                        guess
                    }
                };
                match load_texture(fs, world, tex, opts.texture_mip_skip, textures, texture_index) {
                    Some(i) => {
                        let tint_from = var.filter(|&v| material_tint(world, v).is_some()).unwrap_or(mat);
                        let mut tint = material_tint(world, tint_from);
                        if let Some(t) = tint.as_mut().filter(|t| t.kind == "paint") {
                            // The mask is small (512 pixels); load it at full size.
                            t.mask = texture_parameter(world, tint_from, "Masks").and_then(|m| load_texture(fs, world, m, 0, textures, texture_index));
                        }
                        let detail = material_detail(world, mat).and_then(|(t, m, swap_sets)| {
                            let texture = load_texture(fs, world, t, opts.texture_mip_skip, textures, texture_index)?;
                            let mask = if m == t { texture } else { load_texture(fs, world, m, opts.texture_mip_skip, textures, texture_index)? };
                            Some(DetailSource { texture, mask, swap_sets })
                        });
                        // Foliage: the normal / translucency / smoothness texture
                        // (DESIGN.md "shiny leaves and wet ground").
                        let smoothness = var
                            .and_then(|v| smoothness_texture(world, v))
                            .or_else(|| smoothness_texture(world, mat))
                            .and_then(|t| load_texture(fs, world, t, opts.texture_mip_skip, textures, texture_index));
                        // Normal map (DESIGN.md "normal maps on models"), only if
                        // red and green hold a plain normal.
                        let normal = var
                            .and_then(|v| normal_texture(world, v))
                            .or_else(|| normal_texture(world, mat))
                            .and_then(|t| load_texture(fs, world, t, opts.texture_mip_skip, textures, texture_index))
                            .filter(|&j| is_normal_map(&textures[j]));
                        Look::Texture(i, tint, detail, smoothness, normal)
                    }
                    None => Look::Missing(format!("texture failed: {}", world.names[tex.0])),
                }
            })
            .collect(),
        _ => vec![],
    };

    let normals_ok = !g.normals.is_empty()
        && g.normals.iter().take(64).all(|n| {
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            (0.8..1.2).contains(&l)
        });
    let mut parts = Vec::new();
    let mut hidden = Vec::new();
    let mut damaged_triangles = 0usize;
    for s in &g.submeshes {
        if let Some(Look::Hide(why)) = looks.get(s.material_id as usize) {
            hidden.push(why.to_string());
            continue;
        }
        // Decals are drawn by their texture's alpha. A decal whose colour
        // texture is not `_CA` (colour and alpha; `_CS` alpha is smoothness)
        // would need another opacity source we do not know: left out.
        if s.decal && !matches!(looks.get(s.material_id as usize), Some(Look::Texture(i, ..)) if textures[*i].name.to_ascii_lowercase().ends_with("_ca")) {
            hidden.push("decal without an alpha texture".to_string());
            continue;
        }
        // Keep only the vertices this part uses.
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut part = Part { positions: vec![], normals: vec![], uvs: vec![], indices: vec![], texture: None, colour: None, missing: None, terrain_coloured: false, tint: None, detail: None, uvs1: vec![], material_id: s.material_id, decal: None, smoothness: None, normal_map: None, tangents: vec![], terrain_blend: None, glass: None };
        // Second texture coordinate set of the part (0x22), if the mesh has one.
        let mut second: Vec<[f32; 2]> = Vec::new();
        for tri in g.indices[s.first_index..s.first_index + s.index_count].chunks(3) {
            // Damage-state parts are not shown at the start of a round.
            if let (Some(h), false) = (hide, g.parts.is_empty()) {
                if tri.iter().any(|&v| h.contains(&u32::from(g.parts[v as usize]))) {
                    damaged_triangles += 1;
                    continue;
                }
            }
            // Stored winding: counter-clockwise faces the viewer, as in Bevy.
            for &old in tri {
                let new = *remap.entry(old).or_insert_with(|| {
                    let o = old as usize;
                    part.positions.push(convert::point(g.positions[o]));
                    if normals_ok {
                        part.normals.push(convert::point(g.normals[o]));
                    }
                    if !g.uvs.is_empty() {
                        part.uvs.push(g.uvs[o]);
                    }
                    if !g.uvs1.is_empty() {
                        second.push(g.uvs1[o]);
                    }
                    (part.positions.len() - 1) as u32
                });
                part.indices.push(new);
            }
        }
        // A part made only of damage-state pieces is left out.
        if part.indices.is_empty() {
            hidden.push("damage state part".to_string());
            continue;
        }
        // Collapsed texture coordinates (for the log), and the detail texture
        // on the second set (DESIGN.md "flat grey walls").
        if let Some(Look::Texture(_, _, detail, _, _)) = looks.get(s.material_id as usize) {
            let first = collapsed_share(&part.positions, &part.uvs, &part.indices);
            let area = surface_area(&part.positions, &part.indices);
            uv_stats.parts += 1;
            uv_stats.area += area;
            uv_stats.collapsed_before += first.unwrap_or(0.0) * area;
            if let (Some(src), true) = (detail, second.len() == part.positions.len()) {
                // `FortDetailBlend2UV`: the base uses the second set, the detail the first.
                if src.swap_sets {
                    std::mem::swap(&mut part.uvs, &mut second);
                }
                let collapsed = collapsed_share(&part.positions, &second, &part.indices).unwrap_or(0.0);
                part.detail = Some(Detail { texture: src.texture, mask: src.mask, collapsed: collapsed as f32 });
                part.uvs1 = std::mem::take(&mut second);
                uv_stats.with_detail += 1;
                uv_stats.detail_area += area;
                let both = collapsed_in_both(&part.positions, &part.uvs, &part.uvs1, &part.indices) * area;
                uv_stats.collapsed_after += both;
                *uv_stats.flat_by_mesh.entry(format!("{} material {}", world.names[at.0].rsplit('/').next().unwrap_or(""), s.material_id)).or_default() += both;
                *uv_stats.shaders.entry(material_shader(world, at, s.material_id)).or_default() += 1;
                if std::env::var("BF1_UV_DEBUG").is_ok_and(|dbg| world.names[at.0].to_lowercase().contains(&dbg.to_lowercase())) {
                    uv_stats.debug.push(format!(
                        "{} material {}: detail {}, mask {}, sets swapped {}, base set collapsed {:.1}%, detail set {:.1}%",
                        world.names[at.0].rsplit('/').next().unwrap_or(""),
                        s.material_id,
                        textures[src.texture].name.rsplit('/').next().unwrap_or(""),
                        textures[src.mask].name.rsplit('/').next().unwrap_or(""),
                        src.swap_sets,
                        collapsed_share(&part.positions, &part.uvs, &part.indices).unwrap_or(0.0) * 100.0,
                        collapsed * 100.0
                    ));
                }
            } else {
                let flat = first.unwrap_or(0.0) * area;
                uv_stats.collapsed_after += flat;
                *uv_stats.flat_by_mesh.entry(format!("{} material {}", world.names[at.0].rsplit('/').next().unwrap_or(""), s.material_id)).or_default() += flat;
            }
        }
        // Terrain blend (DESIGN.md "terrain blend on mounds").
        if let Some(Some((mat, var))) = sources.get(s.material_id as usize) {
            part.terrain_blend = terrain_blend(world, *mat, *var);
        }
        // Decal parts: how to blend (from the variant material if any).
        if s.decal {
            part.decal = Some(match sources.get(s.material_id as usize) {
                Some(Some((mat, var))) => decal_params(world, var.unwrap_or(*mat)),
                _ => DecalLook { opacity: 1.0, multiply: [1.0; 3] },
            });
        }
        match looks.get(s.material_id as usize) {
            Some(Look::Texture(i, tint, _, smoothness, normal)) => {
                part.texture = Some(*i);
                part.tint = *tint;
                part.smoothness = *smoothness;
                // Tangents need normals and texture coordinates for every vertex.
                if let Some(n) = normal.filter(|_| part.normals.len() == part.positions.len() && part.uvs.len() == part.positions.len()) {
                    part.tangents = tangents(&part.positions, &part.normals, &part.uvs, &part.indices);
                    part.normal_map = Some(n);
                }
            }
            Some(Look::Colour(c)) => part.colour = Some(*c),
            Some(Look::Terrain) => part.terrain_coloured = true,
            Some(Look::Missing(why)) => part.missing = Some(if s.transparent { format!("{why} [transparent]") } else { why.clone() }),
            Some(Look::Glass(g)) => part.glass = Some(*g),
            Some(Look::Hide(_)) => unreachable!("hidden parts are skipped above"),
            None => part.missing = Some(format!("material id {} out of {}", s.material_id, looks.len())),
        }
        parts.push(part);
    }
    let decal_sections = lod.categories[fb_res::mesh::category::TRANSPARENT_DECAL]
        .iter()
        .filter(|s| !lod.categories[fb_res::mesh::category::OPAQUE].contains(s) && !lod.categories[fb_res::mesh::category::TRANSPARENT].contains(s))
        .count();
    Ok(LoadedMesh { name: world.names[at.0].clone(), parts, hidden, decal_sections, damaged_triangles })
}

/// The colour texture of a material: from its own texture parameters, else
/// from the shader preset it points to (a `SurfaceShaderPreset` holds default
/// textures in its own `ShaderPreset`; presets can point to presets).
fn base_color_texture(world: &World, mat: (usize, usize)) -> Option<(usize, usize)> {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
    colour_from_shader_struct(world, mat.0, sh, 4)
}

fn colour_from_shader_struct(world: &World, part: usize, sh: &fb_ebx::Complex, depth: u32) -> Option<(usize, usize)> {
    let own = match sh.get("TextureParameters") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .filter_map(|tp| {
                let Value::Struct(tp) = tp else { return None };
                let Some(Value::String(n)) = tp.get("ParameterName") else { return None };
                let rank = base_color_rank(n)?;
                let Some(Value::Pointer(t)) = tp.get("Value") else { return None };
                if matches!(t, Pointer::Null) {
                    return None;
                }
                Some((rank, world.resolve(part, t)?))
            })
            .min_by_key(|(r, _)| *r)
            .map(|(_, at)| at),
        _ => None,
    };
    if own.is_some() || depth == 0 {
        return own;
    }
    let Some(Value::Pointer(p)) = sh.get("Shader") else { return None };
    let target = world.resolve(part, p)?;
    let preset = world.get(target);
    if !preset.is_a("SurfaceShaderPreset") {
        return None;
    }
    let Some(Value::Struct(inner)) = preset.get("ShaderPreset") else { return None };
    colour_from_shader_struct(world, target.0, inner, depth - 1)
}

/// Time spent reading and decoding mesh geometry and textures (for the
/// start-up log, DESIGN.md "faster start"), in nanoseconds.
static GEOMETRY_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TEXTURE_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn add_time(counter: &std::sync::atomic::AtomicU64, since: Instant) {
    counter.fetch_add(since.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
}

fn load_texture(
    fs: &GameFs,
    world: &World,
    tex: (usize, usize),
    mip_skip: usize,
    textures: &mut Vec<TextureData>,
    texture_index: &mut HashMap<u64, Option<usize>>,
) -> Option<usize> {
    let Some(Value::ResourceRef(rid)) = world.get(tex).get("Resource") else { return None };
    if let Some(i) = texture_index.get(rid) {
        return *i;
    }
    let t0 = Instant::now();
    let prefetched = PREFETCH.with(|p| p.borrow_mut().remove(&(*rid, mip_skip)));
    let result = prefetched.unwrap_or_else(|| decode_texture(fs, world, tex, mip_skip));
    add_time(&TEXTURE_NANOS, t0);
    let index = match result {
        Ok(t) => {
            textures.push(t);
            Some(textures.len() - 1)
        }
        Err(_) => None,
    };
    texture_index.insert(*rid, index);
    index
}

/// Read, unpack and check one texture (all mips from `mip_skip` on).
fn decode_texture(fs: &GameFs, world: &World, tex: (usize, usize), mip_skip: usize) -> Result<TextureData> {
    let Some(Value::ResourceRef(rid)) = world.get(tex).get("Resource") else { anyhow::bail!("texture without a resource") };
    {
        let h = parse_texture(&world.read_res(fs, *rid)?)?;
        let format = block_format(h.pixel_format).with_context(|| format!("pixel format {}", h.pixel_format))?;
        let data = world.read_chunk(fs, &h.chunk_id)?;
        let mips = h.mips();
        anyhow::ensure!(!mips.is_empty(), "no mips");
        // Skip large mips, but keep the first used mip a multiple of 4 wide and high.
        let mut first = mip_skip.min(mips.len() - 1);
        while first > 0 && (mips[first].width % 4 != 0 || mips[first].height % 4 != 0) {
            first -= 1;
        }
        anyhow::ensure!(mips[first].width % 4 == 0 && mips[first].height % 4 == 0, "size not a multiple of 4");
        let start = mips[first].offset;
        let end = mips.last().map(|m| m.offset + m.size).unwrap_or(start);
        let mut t = TextureData {
            name: world.names[tex.0].clone(),
            format,
            width: mips[first].width,
            height: mips[first].height,
            mip_count: (mips.len() - first) as u32,
            data: data.get(start..end).context("mips past end of chunk")?.to_vec(),
            alpha: [0.0, 0.0, 1.0],
            cutout: false,
        };
        t.alpha = alpha_shares(&t);
        t.cutout = is_cutout(t.alpha);
        Ok(t)
    }
}

thread_local! {
    /// Textures unpacked ahead on all cores (`prefetch_textures`), by
    /// (resource id, mips skipped). `load_texture` takes them from here and
    /// unpacks anything else itself, so the result is the same either way.
    static PREFETCH: std::cell::RefCell<HashMap<(u64, usize), Result<TextureData>>> = std::cell::RefCell::new(HashMap::new());
}

/// Unpack on all cores the textures the mesh loading will most likely ask
/// for (DESIGN.md "faster start"): per used mesh and material (and its
/// variant), the colour texture (or the folder guess), the paint mask and the
/// detail textures. Returns how many were unpacked.
fn prefetch_textures(fs: &GameFs, world: &World, walker: &Walker, opts: &Options, variation_db: &VariationDb, folders: &TextureFolders) -> usize {
    let mut seen_mesh = std::collections::HashSet::new();
    let mut wanted: Vec<((u64, usize), (usize, usize))> = Vec::new();
    let mut seen_tex = std::collections::HashSet::new();
    let mut want = |tex: (usize, usize), skip: usize, wanted: &mut Vec<((u64, usize), (usize, usize))>| {
        if let Some(Value::ResourceRef(rid)) = world.get(tex).get("Resource") {
            if seen_tex.insert((*rid, skip)) {
                wanted.push(((*rid, skip), tex));
            }
        }
    };
    for placed in &walker.meshes {
        let Some(at) = placed.mesh_at else { continue };
        if is_helper_mesh(&placed.mesh) || !seen_mesh.insert((at, placed.variation)) {
            continue;
        }
        let overrides = variation_db.get(&(at, placed.variation)).filter(|_| variation_applies(&opts.variations, &placed.mesh));
        let Some(Value::Array { items, .. }) = world.get(at).get("Materials") else { continue };
        for m in items {
            let Value::Pointer(p) = m else { continue };
            let Some(mat) = world.resolve(at.0, p) else { continue };
            let var = overrides.and_then(|o| o.get(&mat)).copied();
            let colour = var.and_then(|v| base_color_texture(world, v)).or_else(|| base_color_texture(world, mat)).or_else(|| guess_colour_texture(world, folders, at, mat));
            if let Some(t) = colour {
                want(t, opts.texture_mip_skip, &mut wanted);
            }
            for from in [Some(mat), var].into_iter().flatten() {
                if let Some(t) = texture_parameter(world, from, "Masks") {
                    want(t, 0, &mut wanted);
                }
            }
            if let Some((t, mask, _)) = material_detail(world, mat) {
                want(t, opts.texture_mip_skip, &mut wanted);
                want(mask, opts.texture_mip_skip, &mut wanted);
            }
            if let Some(t) = var.and_then(|v| smoothness_texture(world, v)).or_else(|| smoothness_texture(world, mat)) {
                want(t, opts.texture_mip_skip, &mut wanted);
            }
            if let Some(t) = var.and_then(|v| normal_texture(world, v)).or_else(|| normal_texture(world, mat)) {
                want(t, opts.texture_mip_skip, &mut wanted);
            }
        }
    }
    unpack_ahead(fs, world, &wanted)
}

/// Unpack these textures ((resource id, mips skipped), texture asset) on all
/// cores into `PREFETCH`. Returns how many.
fn unpack_ahead(fs: &GameFs, world: &World, wanted: &[((u64, usize), (usize, usize))]) -> usize {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(32);
    let done: Vec<((u64, usize), Result<TextureData>)> = std::thread::scope(|sc| {
        let jobs: Vec<_> = wanted
            .chunks(wanted.len().div_ceil(threads).max(1))
            .map(|part| sc.spawn(move || part.iter().map(|&(key, tex)| (key, decode_texture(fs, world, tex, key.1))).collect::<Vec<_>>()))
            .collect();
        jobs.into_iter().flat_map(|j| j.join().unwrap_or_default()).collect()
    });
    let n = done.len();
    PREFETCH.with(|p| p.borrow_mut().extend(done));
    n
}

/// Shader name and texture parameter names of a material, for the log.
fn material_summary(world: &World, mat: (usize, usize)) -> String {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return "no Shader".into() };
    let shader = match sh.get("Shader") {
        Some(Value::Pointer(p)) => world.link_name(mat.0, p),
        _ => "?".into(),
    };
    let names: Vec<String> = match sh.get("TextureParameters") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .filter_map(|tp| match tp {
                Value::Struct(tp) => match tp.get("ParameterName") {
                    Some(Value::String(n)) => Some(n.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => vec![],
    };
    format!("no colour parameter; shader {shader}; textures [{}]", names.join(", "))
}

/// Colour textures by folder: folder -> (texture leaf name, location).
type TextureFolders = HashMap<String, Vec<(String, (usize, usize))>>;

fn texture_folders(world: &World) -> TextureFolders {
    let mut out: TextureFolders = HashMap::new();
    for (i, p) in world.partitions.iter().enumerate() {
        if p.primary().is_some_and(|x| x.value.type_name == "TextureAsset") {
            let name = &world.names[i];
            if let Some((folder, leaf)) = name.rsplit_once('/') {
                if is_colour_texture_name(leaf) {
                    out.entry(folder.to_string()).or_default().push((leaf.to_string(), (i, 0)));
                }
            }
        }
    }
    out
}

/// A guess for materials that list no textures (their shader graph holds them):
/// a colour texture in the folder of the shader or the mesh (or a "textures"
/// subfolder), with the name that shares the longest start with the mesh or
/// shader name.
fn guess_colour_texture(world: &World, folders: &TextureFolders, mesh: (usize, usize), mat: (usize, usize)) -> Option<(usize, usize)> {
    let mesh_name = world.names[mesh.0].clone();
    let shader_name = match world.get(mat).get("Shader") {
        Some(Value::Struct(sh)) => match sh.get("Shader") {
            Some(Value::Pointer(p)) if !matches!(p, Pointer::Null) => Some(world.link_name(mat.0, p)),
            _ => None,
        },
        _ => None,
    };
    let stem = |n: &str| -> String {
        let leaf = n.rsplit('/').next().unwrap_or(n);
        leaf.trim_start_matches("ss_").trim_start_matches("t_").trim_end_matches("_mesh").to_string()
    };
    let mut keys = vec![stem(&mesh_name)];
    let mut dirs = Vec::new();
    if let Some(s) = shader_name.as_deref().filter(|s| !s.starts_with('<')) {
        keys.push(stem(s));
        if let Some((d, _)) = s.rsplit_once('/') {
            dirs.push(d.to_string());
        }
    }
    if let Some((d, _)) = mesh_name.rsplit_once('/') {
        dirs.push(d.to_string());
    }
    let mut best: Option<(usize, (usize, usize))> = None;
    for d in &dirs {
        for folder in [d.clone(), format!("{d}/textures"), d.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default()] {
            for (leaf, at) in folders.get(&folder).into_iter().flatten() {
                let s = stem(leaf);
                let score = keys.iter().map(|k| k.chars().zip(s.chars()).take_while(|(a, b)| a == b).count()).max().unwrap_or(0);
                if best.is_none_or(|b| score > b.0) {
                    best = Some((score, *at));
                }
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.map(|b| b.1)
}

/// Effect materials (fire, smoke, glow cards) need special shaders we do not
/// have yet; drawn plainly they look like grey sheets, so we leave them out.
fn is_effect_material(world: &World, mat: (usize, usize)) -> bool {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return false };
    let Some(Value::Pointer(p)) = sh.get("Shader") else { return false };
    let name = world.link_name(mat.0, p);
    name.starts_with("fx/") || name.contains("/fx/") || name.contains("smoke") || name.contains("fire")
}

/// Small cards on wooden props (planks, fences, furniture) that have no texture
/// and only a wood colour named "AlphaColor", and mostly cast no shadow. Our
/// guess (not confirmed): they show damage such as bullet holes after a hit, so
/// they are not visible on undamaged objects. Drawn plainly they are coloured squares.
fn is_damage_card(world: &World, mat: (usize, usize)) -> bool {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return false };
    let no_textures = matches!(sh.get("TextureParameters"), Some(Value::Array { items, .. }) if items.is_empty());
    let only_alpha_colour = match sh.get("VectorParameters") {
        Some(Value::Array { items, .. }) => {
            items.len() == 1
                && matches!(&items[0], Value::Struct(v) if matches!(v.get("ParameterName"), Some(Value::String(n)) if n == "AlphaColor"))
        }
        _ => false,
    };
    no_textures && only_alpha_colour
}

/// A plain colour for materials that have no texture by design. Wires: the
/// shader graph holds their colour, which we cannot read; dark grey is a guess.
fn plain_colour(world: &World, mat: (usize, usize)) -> Option<[f32; 3]> {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
    let Some(Value::Pointer(p)) = sh.get("Shader") else { return None };
    let name = world.link_name(mat.0, p);
    name.rsplit('/').next().is_some_and(|leaf| leaf.contains("wire")).then_some([0.05, 0.05, 0.05])
}

/// A colour that the material's shader value gives (DESIGN.md "material
/// tints, part 1"):
/// - paint: a colour value `Color` together with a `Masks` texture (shader
///   45812999: guns, howitzers, mortars);
/// - fabric: `TintColor` in the `..._Fabric` presets, with `BaseColorTiling`.
fn material_tint(world: &World, mat: (usize, usize)) -> Option<Tint> {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
    let vectors: Vec<(&str, [f32; 4], bool)> = match sh.get("VectorParameters") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .filter_map(|v| {
                let Value::Struct(v) = v else { return None };
                let Some(Value::String(name)) = v.get("ParameterName") else { return None };
                let Some(Value::Struct(x)) = v.get("Value") else { return None };
                let f = |n: &str| match x.get(n) {
                    Some(Value::Float(f)) => Some(*f as f32),
                    _ => None,
                };
                let is_colour = matches!(v.get("ParameterType"), Some(Value::Enum { name: Some(t), .. }) if t == "ShaderParameterType_Color");
                Some((name.as_str(), [f("x")?, f("y")?, f("z")?, f("w")?], is_colour))
            })
            .collect(),
        _ => vec![],
    };
    let textures: Vec<&str> = match sh.get("TextureParameters") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .filter_map(|t| match t {
                Value::Struct(t) => match t.get("ParameterName") {
                    Some(Value::String(n)) => Some(n.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => vec![],
    };
    let colour = |name: &str| vectors.iter().find(|(n, _, c)| *n == name && *c).map(|(_, v, _)| [v[0], v[1], v[2]]);
    if textures.contains(&"Masks") {
        if let Some(c) = colour("Color") {
            return Some(Tint { colour: c, uv_scale: 1.0, kind: "paint", mask: None });
        }
    }
    let preset = match sh.get("Shader") {
        Some(Value::Pointer(p)) => world.link_name(mat.0, p),
        _ => String::new(),
    };
    if preset.to_ascii_lowercase().ends_with("_fabric") {
        if let Some(c) = colour("TintColor") {
            let tiling = vectors.iter().find(|(n, _, _)| *n == "BaseColorTiling").map(|(_, v, _)| v[0]).filter(|t| *t > 0.0).unwrap_or(1.0);
            return Some(Tint { colour: c, uv_scale: tiling, kind: "fabric", mask: None });
        }
    }
    None
}

/// Bevy base colour (multiplies the texture) for a tint. `mean` is the
/// texture's mean linear colour; the tint is a linear colour. The rules are
/// fits to the user's screenshot 44 (sunlit part / sunlit ground, MODLOG
/// 2026-10-07), not known from the data:
/// - paint: the tint is the part's mean colour; the texture keeps its detail;
/// - fabric: texture x tint x 2.
pub fn tint_base_colour(tint: &Tint, mean: [f32; 3]) -> [f32; 3] {
    match tint.kind {
        "fabric" => tint.colour.map(|c| 2.0 * c),
        _ => std::array::from_fn(|k| tint.colour[k] / mean[k].max(0.02)),
    }
}

/// Texture coordinate choice per mesh part (for the log).
#[derive(Default)]
pub struct UvStats {
    pub parts: usize,
    /// Parts drawn with a detail texture, and their surface area.
    pub with_detail: usize,
    pub detail_area: f64,
    /// Surface area (mesh units, not placed); collapsed in the first set; and
    /// collapsed in both sets where a detail texture is drawn (no texture
    /// shows there).
    pub area: f64,
    pub collapsed_before: f64,
    pub collapsed_after: f64,
    pub shaders: std::collections::BTreeMap<String, usize>,
    /// Per-part lines for `BF1_UV_DEBUG`.
    pub debug: Vec<String>,
    /// Untextured (collapsed) area per mesh part, mesh units.
    pub flat_by_mesh: std::collections::BTreeMap<String, f64>,
}

impl UvStats {
    fn summary(&self) -> String {
        let top: Vec<String> = {
            let mut v: Vec<_> = self.shaders.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            v.iter().take(8).map(|(n, c)| format!("{} {c}", n.rsplit('/').next().unwrap_or(n))).collect()
        };
        format!(
            "detail textures: {} of {} textured parts ({:.1}% of the textured surface); surface with collapsed base coordinates: {:.1}%, with no texture at all after the detail: {:.1}%; shaders: {}",
            self.with_detail,
            self.parts,
            100.0 * self.detail_area / self.area.max(1e-9),
            100.0 * self.collapsed_before / self.area.max(1e-9),
            100.0 * self.collapsed_after / self.area.max(1e-9),
            top.join(", ")
        )
    }
}

/// Total triangle area of a part.
fn surface_area(positions: &[[f32; 3]], indices: &[u32]) -> f64 {
    indices.chunks_exact(3).map(|t| triangle_area(positions, t)).sum()
}

fn triangle_area(p: &[[f32; 3]], t: &[u32]) -> f64 {
    let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
    let (ab, ac) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
    let cr = [ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0]];
    0.5 * ((cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]) as f64).sqrt()
}

/// Per face: are its texture coordinates collapsed (texture area under 1% of
/// what the part's median scale, metres per repeat along triangle edges,
/// gives)? None without coordinates.
fn collapsed_faces(positions: &[[f32; 3]], uvs: &[[f32; 2]], indices: &[u32]) -> Option<Vec<bool>> {
    if uvs.len() != positions.len() || indices.is_empty() {
        return None;
    }
    let mut per: Vec<f64> = Vec::new();
    for t in indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (pa, pb) = (positions[a as usize], positions[b as usize]);
            let dw = (((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)) as f64).sqrt();
            let (ua, ub) = (uvs[a as usize], uvs[b as usize]);
            let du = (((ua[0] - ub[0]).powi(2) + (ua[1] - ub[1]).powi(2)) as f64).sqrt();
            if du > 1e-6 && dw > 1e-4 {
                per.push(dw / du);
            }
        }
    }
    if per.is_empty() {
        return Some(vec![true; indices.len() / 3]);
    }
    per.sort_by(f64::total_cmp);
    let median = per[per.len() / 2];
    Some(
        indices
            .chunks_exact(3)
            .map(|t| {
                let wa = triangle_area(positions, t);
                let (ua, ub, uc) = (uvs[t[0] as usize], uvs[t[1] as usize], uvs[t[2] as usize]);
                let uva = 0.5 * (((ub[0] - ua[0]) * (uc[1] - ua[1]) - (uc[0] - ua[0]) * (ub[1] - ua[1])) as f64).abs();
                uva < 0.01 * wa / (median * median)
            })
            .collect(),
    )
}

/// Share of a part's surface (by area) whose texture coordinates are
/// collapsed (see `collapsed_faces`). None without coordinates.
pub fn collapsed_share(positions: &[[f32; 3]], uvs: &[[f32; 2]], indices: &[u32]) -> Option<f64> {
    let flags = collapsed_faces(positions, uvs, indices)?;
    let (mut area, mut collapsed) = (0f64, 0f64);
    for (t, c) in indices.chunks_exact(3).zip(flags) {
        let wa = triangle_area(positions, t);
        area += wa;
        if c {
            collapsed += wa;
        }
    }
    Some(if area > 0.0 { collapsed / area } else { 0.0 })
}

/// Share of the surface collapsed in both sets (no texture shows there).
fn collapsed_in_both(positions: &[[f32; 3]], a: &[[f32; 2]], b: &[[f32; 2]], indices: &[u32]) -> f64 {
    let (Some(fa), Some(fb)) = (collapsed_faces(positions, a, indices), collapsed_faces(positions, b, indices)) else { return 0.0 };
    let (mut area, mut both) = (0f64, 0f64);
    for ((t, x), y) in indices.chunks_exact(3).zip(fa).zip(fb) {
        let wa = triangle_area(positions, t);
        area += wa;
        if x && y {
            both += wa;
        }
    }
    if area > 0.0 { both / area } else { 0.0 }
}

/// Detail texture of a building material (DESIGN.md "flat grey walls"):
/// (detail, mask texture, base on the second set).
/// - `..._DetailsOverlay`: `_BaseColorDetail`, mask = `_NormalDetail` alpha;
/// - `FortDetailBlend2UV`: `Detail_Texture_CA`, mask = its own alpha; the
///   base uses the second set.
fn material_detail(world: &World, mat: (usize, usize)) -> Option<((usize, usize), (usize, usize), bool)> {
    if let Some(t) = texture_parameter(world, mat, "_BaseColorDetail") {
        return texture_parameter(world, mat, "_NormalDetail").map(|m| (t, m, false));
    }
    texture_parameter(world, mat, "Detail_Texture_CA").map(|t| (t, t, true))
}

/// Shader (preset) name of the mesh's material `id`, for the log.
fn material_shader(world: &World, mesh: (usize, usize), id: i32) -> String {
    let Some(Value::Array { items, .. }) = world.get(mesh).get("Materials") else { return "?".into() };
    let Some(Value::Pointer(p)) = items.get(id as usize) else { return "?".into() };
    let Some(mat) = world.resolve(mesh.0, p) else { return "?".into() };
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return "?".into() };
    match sh.get("Shader") {
        Some(Value::Pointer(p)) => world.link_name(mat.0, p),
        _ => "?".into(),
    }
}

/// A material's texture parameter by name (the texture asset).
fn texture_parameter(world: &World, mat: (usize, usize), name: &str) -> Option<(usize, usize)> {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
    let Some(Value::Array { items, .. }) = sh.get("TextureParameters") else { return None };
    items.iter().find_map(|tp| {
        let Value::Struct(tp) = tp else { return None };
        if !matches!(tp.get("ParameterName"), Some(Value::String(n)) if n == name) {
            return None;
        }
        match tp.get("Value") {
            Some(Value::Pointer(p)) if !matches!(p, Pointer::Null) => world.resolve(mat.0, p),
            _ => None,
        }
    })
}

/// Share of pixels whose channel `c` (0 red .. 3 alpha) is over one half,
/// from a small mip (for the log: the painted share of a paint mask).
pub fn channel_share(t: &TextureData, c: usize) -> Option<f32> {
    let px = small_mip(t, 64)?;
    Some(px.iter().filter(|p| p[c] > 127).count() as f32 / px.len().max(1) as f32)
}

/// Mean linear colour of a texture (from a small mip), for tints.
pub fn mean_linear(t: &TextureData) -> Option<[f32; 3]> {
    let px = small_mip(t, 32)?;
    let to_linear = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let srgb = matches!(t.format, BlockFormat::Bc1 { srgb: true } | BlockFormat::Bc3 { srgb: true } | BlockFormat::Bc7 { srgb: true });
    let mut sum = [0f64; 3];
    for p in &px {
        for c in 0..3 {
            sum[c] += if srgb { to_linear(p[c]) } else { p[c] as f32 / 255.0 } as f64;
        }
    }
    let n = px.len().max(1) as f64;
    Some(sum.map(|v| (v / n) as f32))
}

/// Materials whose shader takes the colour from the terrain ("virtual texture only").
fn is_terrain_coloured(world: &World, mat: (usize, usize)) -> bool {
    let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return false };
    let Some(Value::Pointer(p)) = sh.get("Shader") else { return false };
    world.link_name(mat.0, p).contains("virtualtexture")
}

/// Log the placements with the largest world size (to find giant objects).
pub fn log_largest(log: &GameLog, loaded: &Loaded, names: &[String]) {
    let mut sizes: Vec<(f32, f32, usize, bevy::math::Vec3, bevy::math::Vec3)> = loaded
        .placements
        .iter()
        .map(|(mesh, m)| {
            let m = bevy::math::Mat4::from_cols_array(m);
            let (mut lo, mut hi) = (bevy::math::Vec3::splat(f32::MAX), bevy::math::Vec3::splat(f32::MIN));
            for p in &loaded.meshes[*mesh].parts {
                for v in &p.positions {
                    let w = m.transform_point3(bevy::math::Vec3::from(*v));
                    lo = lo.min(w);
                    hi = hi.max(w);
                }
            }
            ((hi - lo).max_element(), hi.y, *mesh, lo, hi)
        })
        .collect();
    sizes.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (size, top, mesh, lo, hi) in sizes.iter().take(8) {
        let grey = loaded.meshes[*mesh].parts.iter().filter(|p| p.texture.is_none() && !p.terrain_coloured).count();
        let tex = loaded.meshes[*mesh].parts.iter().filter(|p| p.texture.is_some()).count();
        log.line(format!("  large: {size:.0} m wide, top at {top:.0} m, {grey} grey, {tex} textured parts, Bevy box {lo:.0} .. {hi:.0}: {}", names[*mesh]));
    }
}

/// Meshes made only for lighting, visibility or gameplay (not drawn by the game):
/// light blockers, occluders, Enlighten proxies, collision spheres, vehicle
/// spawner markers and test meshes.
fn is_helper_mesh(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["lightblocker", "occluder", "enlightenproxy", "enlighten_proxy", "gameplay/collisions/", "vehiclespawner", "characters/test/"]
        .iter()
        .any(|k| n.contains(k))
}

#[cfg(test)]
mod tests {
    use super::{bc4_block, is_cutout, tangents, tint_base_colour, Tint};

    #[test]
    fn bc4_blocks_decode_back_close() {
        // A ramp and a flat block, decoded with the same decoder the game uses.
        let mut ramp = [0u8; 16];
        for (k, v) in ramp.iter_mut().enumerate() {
            *v = (k * 16) as u8;
        }
        for v in [ramp, [77u8; 16]] {
            let block = bc4_block(&v);
            // BC5 = red block + green block; decode and compare the red channel.
            let mut both = block.to_vec();
            both.extend_from_slice(&block);
            let mut px = vec![0u32; 16];
            texture2ddecoder::decode_bc5(&both, 4, 4, &mut px).unwrap();
            // At most half a step of the 8-value palette off.
            let half_step = (*v.iter().max().unwrap() as i32 - *v.iter().min().unwrap() as i32) / 14 + 1;
            for (k, p) in px.iter().enumerate() {
                let red = p.to_le_bytes()[2] as i32;
                assert!((red - v[k] as i32).abs() <= half_step, "texel {k}: {red} against {}", v[k]);
            }
        }
    }

    #[test]
    fn tangents_follow_u_and_the_sign_follows_v() {
        // A flat square, normal +y, texture u along +x and v along +z: the
        // tangent is +x and w * cross(normal, tangent) must point to +z.
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]];
        let normals = [[0.0, 1.0, 0.0]; 4];
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let t = tangents(&positions, &normals, &uvs, &[0, 2, 1, 0, 3, 2]);
        for v in &t {
            assert!((v[0] - 1.0).abs() < 1e-5 && v[1].abs() < 1e-5 && v[2].abs() < 1e-5, "{v:?}");
            // cross(+y, +x) = -z, so w must be -1 for +z.
            assert_eq!(v[3], -1.0);
        }
        // v running the other way flips the sign.
        let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
        assert!(tangents(&positions, &normals, &uvs, &[0, 2, 1, 0, 3, 2]).iter().all(|v| v[3] == 1.0));
    }

    #[test]
    fn collapsed_share_finds_point_mapped_faces() {
        use super::collapsed_share;
        // Two unit squares side by side: the left one mapped normally, the
        // right one with all its corners on one texture point.
        let p = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 1.0, 0.0], [1.0, 1.0, 0.0]];
        let uv = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.5, 0.5], [0.5, 0.5], [0.5, 0.5], [0.5, 0.5]];
        let idx = [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7];
        let share = collapsed_share(&p, &uv, &idx).unwrap();
        assert!((share - 0.5).abs() < 1e-9, "{share}");
        assert_eq!(collapsed_share(&p, &[], &idx), None);
    }

    #[test]
    fn tint_rules() {
        let mean = [0.287, 0.282, 0.274];
        let paint = Tint { colour: [0.122, 0.184, 0.082], uv_scale: 1.0, kind: "paint", mask: None };
        let c = tint_base_colour(&paint, mean);
        // Paint: the texture's mean times the base colour is the tint.
        for k in 0..3 {
            assert!((c[k] * mean[k] - paint.colour[k]).abs() < 1e-5);
        }
        let fabric = Tint { colour: [0.337, 0.328, 0.239], uv_scale: 2.0, kind: "fabric", mask: None };
        assert_eq!(tint_base_colour(&fabric, mean), [0.674, 0.656, 0.478]);
    }

    /// Alpha shares (clear, middle, solid) measured on Verdun textures.
    #[test]
    fn cutout_rule_matches_verdun_textures() {
        assert!(is_cutout([0.91, 0.00, 0.09]), "dead tree branches");
        assert!(is_cutout([0.92, 0.05, 0.03]), "yew branch (thin needles)");
        assert!(is_cutout([0.73, 0.20, 0.07]), "larch billboard");
        assert!(!is_cutout([1.00, 0.00, 0.00]), "sandbags _CS (rough: low smoothness)");
        assert!(!is_cutout([0.05, 0.48, 0.47]), "weathervane _CS");
        assert!(!is_cutout([0.10, 0.89, 0.02]), "harvester _CS");
        assert!(!is_cutout([0.00, 0.00, 1.00]), "no alpha");
    }
}
