//! Ground layer textures on the terrain (steps 2 and 3 of the ground plan in
//! docs/DESIGN.md). A terrain material extends Bevy's StandardMaterial: the
//! shader `terrain.wgsl` blends the level's tiling ground textures (mud,
//! grass, rock, ...) per pixel near the camera, by weights that come from the
//! game's own layer masks (`fb_terrain::layers`). Far away the colour map is
//! used, as in the game.
//!
//! The weights are baked on the CPU into two "weight atlases": the battle area
//! at 0.5 m per texel and the whole map at 4 m per texel. Each atlas is a
//! texture array; texel channel c of array layer k is the weight of ground
//! texture 4k + c.

use crate::load::{GroundTexture, TextureData};
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};
use bevy::shader::ShaderRef;
use fb_res::texture::BlockFormat;
use fb_terrain::layers::{LayerMasks, MAX_LAYERS};

/// The most ground textures the shader takes.
pub const MAX_TEXTURES: usize = 16;

/// Metres per texture repeat (a guess; BF1's value is in the compiled shader).
/// `BF1_GROUND_TILE=<metres>` overrides it for tests.
const DEFAULT_TILE_METRES: f32 = 6.0;

pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, GroundLayers>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct GroundLayers {
    #[uniform(100)]
    pub params: GroundParams,
    #[texture(101, dimension = "2d_array")]
    #[sampler(102)]
    pub layers: Handle<Image>,
    #[texture(103, dimension = "2d_array")]
    #[sampler(104)]
    pub fine_weights: Handle<Image>,
    #[texture(105, dimension = "2d_array")]
    #[sampler(106)]
    pub coarse_weights: Handle<Image>,
    /// Normal (red, green) and height (alpha) of each ground texture, same
    /// order as `layers` (DESIGN.md "ground detail", step 2).
    #[texture(107, dimension = "2d_array")]
    #[sampler(108)]
    pub normals: Handle<Image>,
}

#[derive(ShaderType, Reflect, Debug, Clone)]
pub struct GroundParams {
    /// x: texture count, y: metres per repeat, z: weight atlas planes (4 weights each),
    /// w: normal map strength (0 = off).
    pub info: Vec4,
    /// x, y: distance (m) where detail starts to fade and where it is gone; z, w: unused.
    pub fade: Vec4,
    /// Fine atlas: world x0, z0 and size (m); w: 1 if present.
    pub fine: Vec4,
    /// Coarse atlas: world x0, z0 and size (m); w: 1 if present.
    pub coarse: Vec4,
}

impl MaterialExtension for GroundLayers {
    fn fragment_shader() -> ShaderRef {
        "embedded://bf1_game/terrain.wgsl".into()
    }
}

/// Register the terrain material and its embedded shader.
pub fn plugin(app: &mut App) {
    bevy::asset::embedded_asset!(app, "terrain.wgsl");
    app.add_plugins(MaterialPlugin::<TerrainMaterial>::default());
}

/// Decode the ground textures into one RGBA8 array texture (all layers must
/// share size and format; the game's textures are BC7 and BC3). Alpha keeps
/// the smoothness. Textures with a different size than the first are left
/// out. Returns the image and the ground textures used, in array order.
pub fn build_texture_array<'a>(ground: &'a [GroundTexture], textures: &[TextureData]) -> Option<(Image, Vec<&'a GroundTexture>)> {
    let first = &textures[ground.first()?.texture];
    let (w, h, mips) = (first.width, first.height, first.mip_count);
    let candidates: Vec<&GroundTexture> = ground
        .iter()
        .filter(|g| {
            let t = &textures[g.texture];
            t.width == w && t.height == h && t.mip_count == mips
        })
        .take(MAX_TEXTURES)
        .collect();
    // Decode in parallel (each texture is independent).
    let decoded: Vec<Option<Vec<u8>>> = std::thread::scope(|s| {
        let jobs: Vec<_> = candidates.iter().map(|g| s.spawn(|| decode_all_mips(&textures[g.texture]))).collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    });
    let mut data = Vec::new();
    let mut used = Vec::new();
    for (g, d) in candidates.into_iter().zip(decoded) {
        let Some(d) = d else { continue };
        // Layer-major order: all mips of layer 0, then layer 1, ...
        data.extend_from_slice(&d);
        used.push(g);
    }
    if used.is_empty() {
        return None;
    }
    let mut image = array_image(w, h, used.len() as u32, mips, TextureFormat::Rgba8UnormSrgb, data);
    image.sampler = repeat_sampler();
    Some((image, used))
}

/// The `_NRH` textures of the used ground textures, decoded into one RGBA8
/// array in the same order as the colour array. A layer without a matching
/// `_NRH` (missing, or another size) gets a flat normal. Returns the image
/// and the number of layers with a real normal map.
pub fn build_normal_array(used: &[&GroundTexture], textures: &[TextureData], colour: &TextureData) -> (Image, usize) {
    let (w, h, mips) = (colour.width, colour.height, colour.mip_count);
    let decoded: Vec<Option<Vec<u8>>> = std::thread::scope(|s| {
        let jobs: Vec<_> = used
            .iter()
            .map(|g| {
                s.spawn(move || {
                    let t = &textures[g.nrh?];
                    if t.width != w || t.height != h || t.mip_count != mips {
                        return None;
                    }
                    decode_all_mips(t)
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    });
    let flat_len: usize = (0..mips).map(|m| ((w >> m).max(1) * (h >> m).max(1)) as usize * 4).sum();
    let mut data = Vec::new();
    let mut real = 0;
    for d in decoded {
        match d {
            Some(d) if d.len() == flat_len => {
                data.extend_from_slice(&d);
                real += 1;
            }
            // Flat normal (0.5, 0.5), middle height.
            _ => data.extend(std::iter::repeat_n([128u8, 128, 255, 128], flat_len / 4).flatten()),
        }
    }
    let mut image = array_image(w, h, used.len() as u32, mips, TextureFormat::Rgba8Unorm, data);
    image.sampler = repeat_sampler();
    (image, real)
}

/// Shader parameters for the given atlases.
pub fn params(textures: usize, planes: usize, fine: Option<[f32; 3]>, coarse: Option<[f32; 3]>) -> GroundParams {
    let env = |name: &str, default: f32| std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default);
    let rect = |r: Option<[f32; 3]>| r.map(|r| Vec4::new(r[0], r[1], r[2], 1.0)).unwrap_or(Vec4::ZERO);
    GroundParams {
        info: Vec4::new(textures as f32, env("BF1_GROUND_TILE", DEFAULT_TILE_METRES), planes as f32, env("BF1_GROUND_NORMAL", 1.0)),
        fade: Vec4::new(150.0, 450.0, 0.0, 0.0),
        fine: rect(fine),
        coarse: rect(coarse),
    }
}

#[derive(Debug, Default)]
pub struct AtlasStats {
    pub texels: usize,
    /// Texels where no textured layer has weight (the shader uses the colour map there).
    pub empty: usize,
    /// Texels per mask level used.
    pub by_level: [usize; 8],
    /// Sum of weights per texture (to report the share of each).
    pub weight: Vec<f64>,
}

/// Bake the weight atlas for a square (world x0, z0, size in metres, texels
/// per side). `texture_of_layer[l]` is the array index of layer l's texture.
pub fn build_weight_atlas(masks: &LayerMasks, texture_of_layer: &[Option<usize>; MAX_LAYERS], textures: usize, rect: [f32; 3], texels: u32) -> (Image, AtlasStats) {
    let planes = textures.div_ceil(4).max(1);
    let n = texels as usize;
    let cell = rect[2] / texels as f32;
    let mut skip = [true; MAX_LAYERS];
    for (l, t) in texture_of_layer.iter().enumerate() {
        skip[l] = t.is_none();
    }
    // One row at a time, rows split over the CPU cores. Row data: planes x n texels x 4 bytes.
    let threads = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(32);
    let rows_per = n.div_ceil(threads);
    let parts: Vec<(Vec<u8>, AtlasStats)> = std::thread::scope(|s| {
        let jobs: Vec<_> = (0..threads)
            .map(|t| {
                let skip = &skip;
                s.spawn(move || {
                    let (r0, r1) = (t * rows_per, ((t + 1) * rows_per).min(n));
                    let mut out = vec![0u8; planes * (r1.saturating_sub(r0)) * n * 4];
                    let mut st = AtlasStats { weight: vec![0.0; textures], ..Default::default() };
                    let mut w = [0f32; MAX_LAYERS];
                    for j in r0..r1 {
                        let z = rect[1] + (j as f32 + 0.5) * cell;
                        for i in 0..n {
                            let x = rect[0] + (i as f32 + 0.5) * cell;
                            st.texels += 1;
                            let Some(level) = masks.weights_at(x, z, skip, &mut w) else {
                                st.empty += 1;
                                continue;
                            };
                            st.by_level[(level as usize).min(7)] += 1;
                            let mut per_tex = [0f32; MAX_TEXTURES];
                            for (l, &wl) in w.iter().enumerate() {
                                if let (true, Some(t)) = (wl > 0.0, texture_of_layer[l]) {
                                    per_tex[t] += wl;
                                }
                            }
                            let total: f32 = per_tex.iter().sum();
                            if total < 0.02 {
                                st.empty += 1;
                                continue;
                            }
                            for (t, &v) in per_tex.iter().enumerate().take(textures) {
                                let v = v / total;
                                st.weight[t] += v as f64;
                                let (plane, c) = (t / 4, t % 4);
                                let at = ((plane * (r1 - r0) + (j - r0)) * n + i) * 4 + c;
                                out[at] = (v * 255.0).round() as u8;
                            }
                        }
                    }
                    (out, st)
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().expect("weight atlas thread")).collect()
    });
    // Join the row bands into planes (layer-major), then add mips.
    let mut stats = AtlasStats { weight: vec![0.0; textures], ..Default::default() };
    let mut planes_data: Vec<Vec<u8>> = vec![Vec::with_capacity(n * n * 4); planes];
    for (t, (band, st)) in parts.into_iter().enumerate() {
        let rows = (((t + 1) * rows_per).min(n)).saturating_sub(t * rows_per);
        for (p, plane) in planes_data.iter_mut().enumerate() {
            plane.extend_from_slice(&band[p * rows * n * 4..(p + 1) * rows * n * 4]);
        }
        stats.texels += st.texels;
        stats.empty += st.empty;
        for k in 0..8 {
            stats.by_level[k] += st.by_level[k];
        }
        for (a, b) in stats.weight.iter_mut().zip(&st.weight) {
            *a += b;
        }
    }
    let mut data = Vec::new();
    let mut mips = 0;
    for plane in &planes_data {
        let (chain, count) = mip_chain(plane, n);
        data.extend_from_slice(&chain);
        mips = count;
    }
    let mut image = array_image(texels, texels, planes as u32, mips, TextureFormat::Rgba8Unorm, data);
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    (image, stats)
}

/// RGBA8 square image and its box-filtered mips; returns the bytes and the mip count.
fn mip_chain(base: &[u8], size: usize) -> (Vec<u8>, u32) {
    let mut out = base.to_vec();
    let mut level = base.to_vec();
    let (mut s, mut count) = (size, 1u32);
    while s > 1 {
        let ns = (s / 2).max(1);
        let mut next = vec![0u8; ns * ns * 4];
        for y in 0..ns {
            for x in 0..ns {
                for c in 0..4 {
                    let at = |xx: usize, yy: usize| level[((yy.min(s - 1)) * s + xx.min(s - 1)) * 4 + c] as u32;
                    let v = at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1);
                    next[(y * ns + x) * 4 + c] = ((v + 2) / 4) as u8;
                }
            }
        }
        out.extend_from_slice(&next);
        level = next;
        s = ns;
        count += 1;
    }
    (out, count)
}

fn array_image(w: u32, h: u32, layers: u32, mips: u32, format: TextureFormat, data: Vec<u8>) -> Image {
    let mut image = Image::default();
    image.texture_descriptor.size = Extent3d { width: w, height: h, depth_or_array_layers: layers };
    image.texture_descriptor.dimension = TextureDimension::D2;
    image.texture_descriptor.format = format;
    image.texture_descriptor.mip_level_count = mips;
    image.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::D2Array), ..default() });
    image.data = Some(data);
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    image
}

fn repeat_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    })
}

/// All mips of a block-compressed texture as RGBA8 bytes, largest first.
fn decode_all_mips(t: &TextureData) -> Option<Vec<u8>> {
    let block_bytes = match t.format {
        BlockFormat::Bc1 { .. } => 8,
        BlockFormat::Bc3 { .. } | BlockFormat::Bc7 { .. } => 16,
        BlockFormat::Bc5 => return None,
    };
    let mut out = Vec::new();
    let (mut w, mut h, mut offset) = (t.width as usize, t.height as usize, 0usize);
    for _ in 0..t.mip_count {
        let (bw, bh) = (w.div_ceil(4) * 4, h.div_ceil(4) * 4);
        let size = (bw / 4) * (bh / 4) * block_bytes;
        let src = t.data.get(offset..offset + size)?;
        let mut px = vec![0u32; bw * bh];
        match t.format {
            BlockFormat::Bc1 { .. } => texture2ddecoder::decode_bc1(src, bw, bh, &mut px).ok()?,
            BlockFormat::Bc3 { .. } => texture2ddecoder::decode_bc3(src, bw, bh, &mut px).ok()?,
            BlockFormat::Bc7 { .. } => texture2ddecoder::decode_bc7(src, bw, bh, &mut px).ok()?,
            BlockFormat::Bc5 => return None,
        }
        // Crop the block padding (mips smaller than 4 pixels); decoder bytes are [b, g, r, a].
        for y in 0..h {
            for x in 0..w {
                let b = px[y * bw + x].to_le_bytes();
                out.extend_from_slice(&[b[2], b[1], b[0], b[3]]);
            }
        }
        offset += size;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chain_halves_down_to_one() {
        let base = vec![200u8; 4 * 4 * 4];
        let (chain, count) = mip_chain(&base, 4);
        assert_eq!(count, 3);
        assert_eq!(chain.len(), (16 + 4 + 1) * 4);
        assert!(chain.iter().all(|&v| v == 200));
    }
}

/// Build everything the terrain material needs, or None (the terrain then
/// uses its colour map only). `centre` is the battle area centre (world x, z).
pub fn setup(ground: &[GroundTexture], textures: &[TextureData], terrain: &fb_terrain::Terrain, centre: [f32; 2], images: &mut Assets<Image>, log: &crate::log::GameLog) -> Option<GroundLayers> {
    let t = std::time::Instant::now();
    let Some(masks) = terrain.layers.as_ref() else {
        log.line("ground layers: no layer masks, the terrain uses its colour map only");
        return None;
    };
    let (array, used) = build_texture_array(ground, textures)?;
    let (normals, with_normals) = build_normal_array(&used, textures, &textures[used[0].texture]);
    let mut texture_of_layer = [None; MAX_LAYERS];
    for (i, g) in used.iter().enumerate() {
        for &l in &g.layers {
            if (l as usize) < MAX_LAYERS {
                texture_of_layer[l as usize] = Some(i);
            }
        }
    }
    let names: Vec<&str> = used.iter().map(|g| g.name.rsplit('/').next().unwrap_or(&g.name)).collect();
    log.line(format!(
        "ground textures in the shader: {} ({:.2} s to decode, {with_normals} with a normal map): {}",
        used.len(),
        t.elapsed().as_secs_f64(),
        names.join(", ")
    ));
    let planes = used.len().div_ceil(4);
    let width = terrain.max[0] - terrain.min[0];
    let fine_rect = [centre[0] - 512.0, centre[1] - 512.0, 1024.0];
    let coarse_rect = [terrain.min[0], terrain.min[2], width];
    let mut report = |name: &str, rect: [f32; 3], texels: u32| {
        let t = std::time::Instant::now();
        let (image, st) = build_weight_atlas(masks, &texture_of_layer, used.len(), rect, texels);
        let total: f64 = st.weight.iter().sum::<f64>().max(1.0);
        let share: Vec<String> = st.weight.iter().zip(&names).filter(|(w, _)| **w / total > 0.005).map(|(w, n)| format!("{n} {:.0}%", 100.0 * w / total)).collect();
        log.line(format!(
            "ground weights {name}: {texels} x {texels} texels over {:.0} m ({:.2} s); no textured layer at {:.1}%; mask level used {:?}; share: {}",
            rect[2],
            t.elapsed().as_secs_f64(),
            100.0 * st.empty as f64 / st.texels.max(1) as f64,
            &st.by_level[..6],
            share.join(", ")
        ));
        images.add(image)
    };
    let fine = report("battle area", fine_rect, 2048);
    let coarse = report("whole map", coarse_rect, 1024);
    let params = params(used.len(), planes, Some(fine_rect), Some(coarse_rect));
    Some(GroundLayers { params, layers: images.add(array), fine_weights: fine, coarse_weights: coarse, normals: images.add(normals) })
}
