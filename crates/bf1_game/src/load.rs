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
}

/// How a material is drawn.
enum Look {
    Texture(usize, Option<Tint>, Option<DetailSource>),
    Colour([f32; 3]),
    /// Colour from the terrain under the object.
    Terrain,
    /// Not drawn; the text says why.
    Hide(&'static str),
    /// Drawn grey; the text says why (for the log).
    Missing(String),
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
    let terrain = load_terrain(&fs, &world)?;
    log.line(format!("terrain loaded: {} height tiles ({:.2} s)", terrain.tiles.len(), t.elapsed().as_secs_f64()));

    // Meshes and textures.
    let t = Instant::now();
    let mut mesh_index: HashMap<(usize, usize), Option<usize>> = HashMap::new();
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

    for placed in &walker.meshes {
        let Some(at) = placed.mesh_at else { continue };
        // Meshes that only exist for lighting or visibility (light blockers, occluders,
        // Enlighten proxies) are never drawn in the game.
        if is_helper_mesh(&placed.mesh) {
            helper_meshes += 1;
            continue;
        }
        let index = match mesh_index.get(&at) {
            Some(i) => *i,
            None => {
                let i = match load_mesh(&fs, &world, at, opts, &mut loaded.textures, &mut texture_index, &mut stats, &folders, &mut guessed, &mut uv_stats) {
                    Ok(m) => {
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
                mesh_index.insert(at, i);
                i
            }
        };
        if let Some(i) = index {
            let m = convert::transform(placed.transform.to_cols_array());
            loaded.placements.push((i, m.to_cols_array()));
        }
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
                    None => match load_mesh(&fs, &world, (part, 0), opts, &mut loaded.textures, &mut texture_index, &mut stats, &folders, &mut guessed, &mut uv_stats) {
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
    log_files_used(&fs, log);
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

/// The colour texture that a shader-graph shader uses, from the level's
/// shader database (texture names ending `_C`, `_C_Mip`, `_CA` or `_CS`).
fn shader_colour_texture(fs: &GameFs, world: &World, opts: &Options, shader: &str) -> Option<(usize, usize)> {
    use fb_res::shaderdb::{fb_hash, ShaderDb};
    let db = world.read_res_named(fs, &format!("{}/shaderdb", opts.level)).ok()?;
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
    let db_name = format!("{}/shaderdb", opts.level);
    let t = Instant::now();
    let db = world.read_res_named(fs, &db_name)?;
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

fn load_terrain(fs: &GameFs, world: &World) -> Result<fb_terrain::Terrain> {
    let part = (0..world.partitions.len())
        .find(|&i| world.partitions[i].primary().is_some_and(|p| p.value.type_name == "TerrainData"))
        .context("no TerrainData")?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("TerrainStreamingTreeResource") else {
        anyhow::bail!("TerrainData has no TerrainStreamingTreeResource")
    };
    let tree = world.read_res(fs, *rid)?;
    let (terrain, _) = fb_terrain::Terrain::load(
        &tree,
        |id| world.chunks.contains_key(id) || fs.toc_chunks.contains_key(id),
        |id| world.read_chunk(fs, id),
    )?;
    Ok(terrain)
}

fn load_mesh(
    fs: &GameFs,
    world: &World,
    at: (usize, usize),
    opts: &Options,
    textures: &mut Vec<TextureData>,
    texture_index: &mut HashMap<u64, Option<usize>>,
    stats: &mut DecodeStats,
    folders: &TextureFolders,
    guessed: &mut usize,
    uv_stats: &mut UvStats,
) -> Result<LoadedMesh> {
    let asset = world.get(at);
    let Some(Value::ResourceRef(rid)) = asset.get("MeshSetResource") else { anyhow::bail!("no MeshSetResource") };
    let ms = parse_meshset(&world.read_res(fs, *rid)?)?;
    let lod = ms.lods.first().context("no LODs")?;
    let chunk = lod.chunk_id.context("inline mesh data (not supported yet)")?;
    let g = decode_lod(lod, &world.read_chunk(fs, &chunk)?, stats)?;

    // Base colour texture of each material.
    let looks: Vec<Look> = match asset.get("Materials") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .map(|m| {
                let Value::Pointer(p) = m else { return Look::Missing("material is not a link".into()) };
                let Some(mat) = world.resolve(at.0, p) else { return Look::Missing("material not loaded".into()) };
                if is_effect_material(world, mat) {
                    return Look::Hide("effect");
                }
                if is_damage_card(world, mat) {
                    return Look::Hide("damage card");
                }
                if is_terrain_coloured(world, mat) {
                    return Look::Terrain;
                }
                let tex = match base_color_texture(world, mat) {
                    Some(t) => t,
                    None => {
                        if let Some(c) = plain_colour(world, mat) {
                            return Look::Colour(c);
                        }
                        let Some(guess) = guess_colour_texture(world, folders, at, mat) else { return Look::Missing(material_summary(world, mat)) };
                        *guessed += 1;
                        guess
                    }
                };
                match load_texture(fs, world, tex, opts.texture_mip_skip, textures, texture_index) {
                    Some(i) => {
                        let mut tint = material_tint(world, mat);
                        if let Some(t) = tint.as_mut().filter(|t| t.kind == "paint") {
                            // The mask is small (512 pixels); load it at full size.
                            t.mask = texture_parameter(world, mat, "Masks").and_then(|m| load_texture(fs, world, m, 0, textures, texture_index));
                        }
                        let detail = material_detail(world, mat).and_then(|(t, m, swap_sets)| {
                            let texture = load_texture(fs, world, t, opts.texture_mip_skip, textures, texture_index)?;
                            let mask = if m == t { texture } else { load_texture(fs, world, m, opts.texture_mip_skip, textures, texture_index)? };
                            Some(DetailSource { texture, mask, swap_sets })
                        });
                        Look::Texture(i, tint, detail)
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
    for s in &g.submeshes {
        if let Some(Look::Hide(why)) = looks.get(s.material_id as usize) {
            hidden.push(why.to_string());
            continue;
        }
        // Keep only the vertices this part uses.
        let mut remap: HashMap<u32, u32> = HashMap::new();
        let mut part = Part { positions: vec![], normals: vec![], uvs: vec![], indices: vec![], texture: None, colour: None, missing: None, terrain_coloured: false, tint: None, detail: None, uvs1: vec![], material_id: s.material_id };
        // Second texture coordinate set of the part (0x22), if the mesh has one.
        let mut second: Vec<[f32; 2]> = Vec::new();
        for tri in g.indices[s.first_index..s.first_index + s.index_count].chunks(3) {
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
        // Collapsed texture coordinates (for the log), and the detail texture
        // on the second set (DESIGN.md "flat grey walls").
        if let Some(Look::Texture(_, _, detail)) = looks.get(s.material_id as usize) {
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
        match looks.get(s.material_id as usize) {
            Some(Look::Texture(i, tint, _)) => {
                part.texture = Some(*i);
                part.tint = *tint;
            }
            Some(Look::Colour(c)) => part.colour = Some(*c),
            Some(Look::Terrain) => part.terrain_coloured = true,
            Some(Look::Missing(why)) => part.missing = Some(if s.transparent { format!("{why} [transparent]") } else { why.clone() }),
            Some(Look::Hide(_)) => unreachable!("hidden parts are skipped above"),
            None => part.missing = Some(format!("material id {} out of {}", s.material_id, looks.len())),
        }
        parts.push(part);
    }
    Ok(LoadedMesh { name: world.names[at.0].clone(), parts, hidden })
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
    let result = (|| -> Result<TextureData> {
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
    })();
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
    use super::{is_cutout, tint_base_colour, Tint};

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
