//! bf1_game: walk around a Battlefield 1 level, read at runtime from the
//! user's own install. Step 6a: terrain + placed meshes + fly camera.

mod camera;
mod collision;
mod convert;
mod detail;
mod environment;
mod grading;
mod inspect;
mod ground;
mod load;
mod log;
mod overlay;
mod paint;
mod player;
mod probes;
mod scatter;
mod scene;
mod terrain_mesh;
mod view_probe;

use bevy::asset::RenderAssetUsages;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use fb_res::texture::BlockFormat;
use log::GameLog;
use std::sync::Mutex;

/// The loaded level, taken once by the startup system.
#[derive(Resource)]
struct PendingLevel(Mutex<Option<load::Loaded>>);


fn main() {
    let log = match GameLog::create() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot create the log file: {e}");
            std::process::exit(1);
        }
    };
    log.line(format!("bf1_game starting (step 6a: fly camera), log file {}", log.path.display()));
    let mut loaded = match load::load_level(&log, &load::Options::default()) {
        Ok(l) => l,
        Err(e) => {
            log.line(format!("LOAD FAILED: {e:#}"));
            std::process::exit(1);
        }
    };
    let centre = Vec3::from(loaded.centre);
    // Mean ground colour of the map, linear, for the bounce light (scene.rs).
    let ground_albedo = loaded.terrain.ground_colour().map(|v| Color::srgb_u8(v, v, v).to_linear().red);
    let environments = std::mem::take(&mut loaded.environments);
    let start = start_weather(&environments, &log);

    App::new()
        .insert_resource(ClearColor(Color::srgb(0.55, 0.58, 0.62)))
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: "BF1 Rewrite - Verdun Heights".into(), resolution: window_size(), ..default() }),
            ..default()
        }))
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        .add_plugins(ground::plugin)
        .add_plugins(paint::plugin)
        .add_plugins(detail::plugin)
        .insert_resource(log)
        .insert_resource(scene::Centre(centre))
        .insert_resource(scene::Env { current: start, list: environments, ground_albedo })
        .insert_resource(PendingLevel(Mutex::new(Some(loaded))))
        .add_systems(Startup, (spawn_level, scene::spawn_scene))
        .insert_resource(player::Walk { enabled: false, feet: Vec3::ZERO, velocity: Vec3::ZERO, grounded: false })
        .add_systems(Update, (camera::fly_camera, player::toggle_walk, player::walk_player, status_log, scene::follow_sky, scene::cycle_weather).chain())
        .add_systems(Update, (exit_after_env, screenshot_test, view_probe::log_view))
        .init_resource::<grading::GradingState>()
        .add_systems(Update, grading::apply_grading.after(scene::cycle_weather))
        .add_systems(Update, probes::apply_volumes.after(scene::cycle_weather))
        .init_resource::<inspect::InspectResult>()
        .add_systems(Startup, overlay::spawn_overlay)
        .add_systems(Update, (inspect::inspect_key, overlay::toggle_overlay, overlay::update_overlay).chain())
        .add_systems(Update, (scatter::update_scatter, scatter::face_cards))
        .run();
}


/// BF1_WEATHER=<part of a variant name> (for example firerain) starts in that
/// weather variant instead of the base.
fn start_weather(list: &[environment::Environment], log: &GameLog) -> usize {
    let Ok(want) = std::env::var("BF1_WEATHER") else { return 0 };
    let want = want.to_lowercase();
    match list.iter().position(|e| e.name.to_lowercase().contains(&want)) {
        Some(i) => {
            log.line(format!("starting weather: {} (BF1_WEATHER={want})", list[i].name));
            i
        }
        None => {
            log.line(format!("BF1_WEATHER={want} matches no weather variant; starting with the base"));
            0
        }
    }
}

/// `uvs1` (second texture coordinate set) is only added when it matches.
fn make_mesh(positions: Vec<[f32; 3]>, normals: Vec<[f32; 3]>, uvs: Vec<[f32; 2]>, uvs1: Vec<[f32; 2]>, indices: Vec<u32>) -> Mesh {
    let count = positions.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, if uvs.len() == count { uvs } else { vec![[0.0, 0.0]; count] })
        .with_inserted_indices(Indices::U32(indices));
    if uvs1.len() == count && count > 0 {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uvs1);
    }
    if normals.len() == count {
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    } else {
        mesh.compute_smooth_normals();
    }
    mesh
}

fn make_image(t: &load::TextureData) -> Image {
    let format = match t.format {
        BlockFormat::Bc1 { srgb: false } => TextureFormat::Bc1RgbaUnorm,
        BlockFormat::Bc1 { srgb: true } => TextureFormat::Bc1RgbaUnormSrgb,
        BlockFormat::Bc3 { srgb: false } => TextureFormat::Bc3RgbaUnorm,
        BlockFormat::Bc3 { srgb: true } => TextureFormat::Bc3RgbaUnormSrgb,
        BlockFormat::Bc5 => TextureFormat::Bc5RgUnorm,
        BlockFormat::Bc7 { srgb: false } => TextureFormat::Bc7RgbaUnorm,
        BlockFormat::Bc7 { srgb: true } => TextureFormat::Bc7RgbaUnormSrgb,
    };
    let mut image = Image::default();
    image.texture_descriptor.size = Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 };
    image.texture_descriptor.dimension = TextureDimension::D2;
    image.texture_descriptor.format = format;
    image.texture_descriptor.mip_level_count = t.mip_count;
    image.data = Some(t.data.clone());
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    image
}

fn spawn_level(
    mut commands: Commands,
    pending: Res<PendingLevel>,
    log: Res<GameLog>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut terrain_materials: ResMut<Assets<ground::TerrainMaterial>>,
    mut paint_materials: ResMut<Assets<paint::PaintMaterial>>,
    mut detail_materials: ResMut<Assets<detail::DetailMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(level) = pending.0.lock().ok().and_then(|mut l| l.take()) else { return };
    let t = std::time::Instant::now();
    if let Some(probe) = view_probe::build(&level, &log) {
        commands.insert_resource(probe);
    }
    // Inspect key: mesh parts in mesh space and placement boxes (inspect.rs).
    let t_inspect = std::time::Instant::now();
    commands.insert_resource(inspect::build(&level));
    log.line(format!("inspect index built ({:.2} s)", t_inspect.elapsed().as_secs_f64()));
    // Indoor light: light probe boxes around buildings and bunkers (DESIGN.md
    // "indoor light with a light probe grid"). BF1_PROBES=off leaves them out.
    if std::env::var("BF1_PROBES").map_or(true, |v| v != "off") {
        let c = level.centre;
        let solid = probes::solid_triangles(&level, [c[0] - 512.0, c[2] - 512.0], [c[0] + 512.0, c[2] + 512.0]);
        let (boxes, coverage) = probes::find_boxes(&level, &solid, &log);
        let t = std::time::Instant::now();
        let occ = probes::Occupancy::build(&solid);
        drop(solid);
        let hg = probes::HeightGrid::build(&level.terrain, c[0] - 560.0, c[2] - 560.0, 1120);
        log.line(format!("light probes: occupancy {} blocks of 8 m, terrain grid 1120 m ({:.2} s)", occ.blocks(), t.elapsed().as_secs_f64()));
        let baked = probes::bake(&boxes, &coverage, &occ, &hg, &log);
        commands.insert_resource(probes::ProbeVolumes { baked, applied: None, images: vec![] });
    } else {
        log.line("light probes: off (BF1_PROBES=off)");
    }

    // Collision data for walking (local-space triangles per mesh, world placements).
    let mut collision = collision::CollisionWorld::default();
    let mut collision_index: Vec<Option<usize>> = Vec::with_capacity(level.meshes.len());
    for m in &level.meshes {
        if collision::is_non_blocking(&m.name) {
            collision_index.push(None);
            continue;
        }
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        // Decals lie on other surfaces (some float a little above them): not solid.
        for p in m.parts.iter().filter(|p| p.decal.is_none()) {
            let base = verts.len() as u32;
            verts.extend(p.positions.iter().map(|v| Vec3::from(*v)));
            tris.extend(p.indices.chunks_exact(3).map(|t| [base + t[0], base + t[1], base + t[2]]));
        }
        collision_index.push(Some(collision.add_mesh(&m.name, verts, tris)));
    }
    for (mesh, matrix) in &level.placements {
        if let Some(c) = collision_index[*mesh] {
            collision.add_placement(c, Mat4::from_cols_array(matrix));
        }
    }
    log.line(format!("collision: {} meshes, {} placements", collision.meshes.len(), collision.placements.len()));
    commands.insert_resource(collision);
    commands.insert_resource(player::Ground(level.terrain.clone()));

    // Terrain colour materials: one per colour tile (decoded, with mips).
    let ground_rgb = level.terrain.ground_colour();
    let mut markers = 0usize;
    let mut colour_images = Vec::new();
    let colour_materials: Vec<Handle<StandardMaterial>> = level
        .terrain
        .colours
        .iter()
        .map(|c| {
            let (image, n) = terrain_colour_image(&c.data, ground_rgb);
            markers += n;
            let image = images.add(image);
            colour_images.push(image.clone());
            materials.add(StandardMaterial { base_color_texture: Some(image), perceptual_roughness: 0.95, ..default() })
        })
        .collect();
    let all = level.terrain.colours.len() * fb_terrain::COLOUR_SIZE * fb_terrain::COLOUR_SIZE;
    log.line(format!(
        "terrain colour: {markers} marker pixels of {all} ({:.1}%) replaced with the ground colour {ground_rgb:?} (sRGB)",
        100.0 * markers as f64 / all.max(1) as f64
    ));

    // Ground layer textures for the terrain shader (None: plain colour-map materials).
    // BF1_GROUND_OFF=1 turns them off (to compare in tests).
    let ground_off = std::env::var("BF1_GROUND_OFF").is_ok_and(|v| v == "1");
    let centre_xz = convert::to_frostbite(level.centre);
    let ground_layers = if ground_off {
        log.line("ground layers: off (BF1_GROUND_OFF=1)");
        None
    } else {
        ground::setup(&level.ground, &level.textures, &level.terrain, [centre_xz[0], centre_xz[2]], &mut images, &log)
    };

    // Mesh materials: one per texture, plus a grey one for parts without a texture.
    let grey = materials.add(StandardMaterial { base_color: Color::srgb(0.6, 0.6, 0.6), perceptual_roughness: 0.9, ..default() });
    let texture_images: Vec<Handle<Image>> = level.textures.iter().map(|t| images.add(make_image(t))).collect();
    // Test switch: BF1_CUTOUT=<alpha> sets the cut-out threshold of leaf cards (default 0.5).
    let cutout = std::env::var("BF1_CUTOUT").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.5);
    let texture_materials: Vec<Handle<StandardMaterial>> = level
        .textures
        .iter()
        .zip(&texture_images)
        .map(|(t, img)| {
            let img = img.clone();
            if t.cutout {
                // Leaf and branch cards: cut out by the texture's alpha, seen from both sides.
                materials.add(StandardMaterial {
                    base_color_texture: Some(img),
                    perceptual_roughness: 0.85,
                    alpha_mode: AlphaMode::Mask(cutout),
                    cull_mode: None,
                    double_sided: true,
                    ..default()
                })
            } else {
                materials.add(StandardMaterial { base_color_texture: Some(img), perceptual_roughness: 0.85, ..default() })
            }
        })
        .collect();
    // Plain-colour materials (parts without a texture by design), one per colour.
    let mut colour_materials_plain: std::collections::HashMap<[u32; 3], Handle<StandardMaterial>> = std::collections::HashMap::new();
    // Tinted materials (paint, fabric): one per texture and tint. The tint is the
    // part's mean colour; the texture keeps its detail (DESIGN.md "material tints").
    let mut tinted: std::collections::HashMap<(usize, [u32; 4], Option<usize>), PartMaterial> = std::collections::HashMap::new();
    let mut tint_counts: std::collections::BTreeMap<&'static str, usize> = std::collections::BTreeMap::new();

    // Meshes: one Bevy mesh per part. Terrain-coloured parts keep their data,
    // because each placed copy needs its own texture coordinates.
    enum PartGpu {
        Shared(Handle<Mesh>, PartMaterial),
        TerrainColoured(load::Part),
    }
    // BF1_TINTS=off draws tinted parts with their plain texture; BF1_PAINT_MASK=off
    // paints the whole part (to compare in tests).
    let tints_on = std::env::var("BF1_TINTS").map_or(true, |v| v != "off");
    let masks_on = std::env::var("BF1_PAINT_MASK").map_or(true, |v| v != "off");
    // BF1_DETAIL=off draws building parts without their detail texture (to compare).
    let details_on = std::env::var("BF1_DETAIL").map_or(true, |v| v != "off");
    let mut detailed: std::collections::HashMap<(usize, usize, usize), PartMaterial> = std::collections::HashMap::new();
    // Decal materials by (texture, opacity and colour multiplier bits). The
    // depth bias keeps a decal in front of the surface it lies on;
    // `BF1_DECAL_BIAS=<n>` overrides it (wgpu constant bias units).
    let decal_bias = std::env::var("BF1_DECAL_BIAS").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(100.0);
    let mut decal_materials: std::collections::HashMap<(usize, [u32; 4]), Handle<StandardMaterial>> = std::collections::HashMap::new();
    // Scatter mesh names, before the meshes move to the GPU.
    let scatter_labels: Vec<String> = level
        .scatter
        .iter()
        .map(|st| format!("L{} T{} {}", st.layer, st.identifier, level.meshes[st.mesh].name.rsplit('/').next().unwrap_or("").trim_end_matches("_mesh")))
        .collect();
    // Scatter meshes that are one flat card (a quad): the turn of the card's
    // face about the vertical axis, so the card can face the camera.
    let scatter_cards: Vec<Option<f32>> = level
        .scatter
        .iter()
        .map(|st| {
            let parts = &level.meshes[st.mesh].parts;
            let [p] = parts.as_slice() else { return None };
            if p.positions.len() != 4 || p.indices.len() < 3 {
                return None;
            }
            let v = |i: usize| Vec3::from(p.positions[p.indices[i] as usize]);
            let n = (v(1) - v(0)).cross(v(2) - v(0));
            (n.x.hypot(n.z) > 0.5 * n.length()).then(|| n.x.atan2(n.z))
        })
        .collect();
    let parts: Vec<Vec<PartGpu>> = level
        .meshes
        .into_iter()
        .map(|m| {
            m.parts
                .into_iter()
                .map(|p| {
                    if p.terrain_coloured {
                        return PartGpu::TerrainColoured(p);
                    }
                    let material = match (p.texture, p.colour) {
                        (Some(i), _) if p.decal.is_some() => {
                            let d = p.decal.expect("checked");
                            let key = (i, [d.opacity.to_bits(), d.multiply[0].to_bits(), d.multiply[1].to_bits(), d.multiply[2].to_bits()]);
                            PartMaterial::Decal(
                                decal_materials
                                    .entry(key)
                                    .or_insert_with(|| {
                                        materials.add(StandardMaterial {
                                            base_color: Color::linear_rgba(d.multiply[0], d.multiply[1], d.multiply[2], d.opacity),
                                            base_color_texture: Some(texture_images[i].clone()),
                                            perceptual_roughness: 0.85,
                                            alpha_mode: AlphaMode::Blend,
                                            depth_bias: decal_bias,
                                            cull_mode: None,
                                            double_sided: true,
                                            ..default()
                                        })
                                    })
                                    .clone(),
                            )
                        }
                        (Some(i), _) if p.detail.is_some() && details_on => {
                            let d = p.detail.expect("checked");
                            detailed
                                .entry((i, d.texture, d.mask))
                                .or_insert_with(|| {
                                    PartMaterial::Detail(detail_materials.add(detail::DetailMaterial {
                                        base: StandardMaterial { base_color_texture: Some(texture_images[i].clone()), perceptual_roughness: 0.85, ..default() },
                                        extension: detail::DetailTexture { texture: texture_images[d.texture].clone(), mask: texture_images[d.mask].clone() },
                                    }))
                                })
                                .clone()
                        }
                        (Some(i), _) if p.tint.is_some() && tints_on => {
                            let tint = p.tint.expect("checked");
                            *tint_counts.entry(tint.kind).or_default() += 1;
                            let mask = tint.mask.filter(|_| masks_on);
                            let key = (i, [tint.colour[0].to_bits(), tint.colour[1].to_bits(), tint.colour[2].to_bits(), tint.uv_scale.to_bits()], mask);
                            tinted
                                .entry(key)
                                .or_insert_with(|| {
                                    let mean = load::mean_linear(&level.textures[i]).unwrap_or([0.5; 3]);
                                    let c = load::tint_base_colour(&tint, mean);
                                    let short = |t: usize| level.textures[t].name.rsplit('/').next().unwrap_or("").to_string();
                                    let mask_text = match mask {
                                        Some(m) => match load::channel_share(&level.textures[m], 1) {
                                            Some(p) => format!("{} ({:.0}% painted)", short(m), p * 100.0),
                                            None => format!("{} (painted share not known)", short(m)),
                                        },
                                        None => "none".into(),
                                    };
                                    log.line(format!(
                                        "  tint {}: {} value {:.3?}, texture mean (linear) {:.3?}, base colour {:.3?}, repeats {}, mask {}",
                                        tint.kind,
                                        short(i),
                                        tint.colour,
                                        mean,
                                        c,
                                        tint.uv_scale,
                                        mask_text
                                    ));
                                    match mask {
                                        // Painted: the plain texture, coloured by the shader where the mask is set.
                                        Some(m) => PartMaterial::Paint(paint_materials.add(paint::PaintMaterial {
                                            base: StandardMaterial { base_color_texture: Some(texture_images[i].clone()), perceptual_roughness: 0.85, ..default() },
                                            extension: paint::PaintMask { paint: Vec4::new(c[0], c[1], c[2], 0.0), mask: texture_images[m].clone() },
                                        })),
                                        None => PartMaterial::Standard(materials.add(StandardMaterial {
                                            base_color: Color::linear_rgb(c[0], c[1], c[2]),
                                            base_color_texture: Some(texture_images[i].clone()),
                                            perceptual_roughness: 0.85,
                                            uv_transform: bevy::math::Affine2::from_scale(Vec2::splat(tint.uv_scale)),
                                            ..default()
                                        })),
                                    }
                                })
                                .clone()
                        }
                        (Some(i), _) => PartMaterial::Standard(texture_materials[i].clone()),
                        (None, Some(c)) => PartMaterial::Standard(
                            colour_materials_plain
                                .entry(c.map(f32::to_bits))
                                .or_insert_with(|| materials.add(StandardMaterial { base_color: Color::srgb(c[0], c[1], c[2]), perceptual_roughness: 0.6, ..default() }))
                                .clone(),
                        ),
                        (None, None) => PartMaterial::Standard(grey.clone()),
                    };
                    PartGpu::Shared(meshes.add(make_mesh(p.positions, p.normals, p.uvs, p.uvs1, p.indices)), material)
                })
                .collect()
        })
        .collect();

    log.line(format!("tinted parts: {tint_counts:?}, {} tinted materials{}", tinted.len(), if tints_on { "" } else { " (off: BF1_TINTS=off)" }));
    log.line(format!("detail materials: {}{}", detailed.len(), if details_on { "" } else { " (off: BF1_DETAIL=off)" }));

    // Terrain mesh scattering (scatter.rs). BF1_SCATTER=off leaves it out.
    if std::env::var("BF1_SCATTER").map_or(true, |v| v != "off") && !level.scatter.is_empty() {
        // Backdrop trees stand 0.5 to 2 km out, but their stored distances
        // are 500 and 30 m (DESIGN.md "backdrop trees on the far hills"). Our
        // rule: kinds with "backdrop" in the mesh name reach the whole map.
        let backdrop_distance = std::env::var("BF1_BACKDROP_DISTANCE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(6000.0);
        let kinds: Vec<scatter::ScatterKind> = level
            .scatter
            .iter()
            .zip(&scatter_labels)
            .zip(&scatter_cards)
            .map(|((st, label), card)| scatter::ScatterKind {
                label: label.clone(),
                layer: st.layer as usize,
                density: st.density,
                min_scale: st.min_scale,
                max_scale: st.max_scale,
                distance: if label.contains("backdrop") { backdrop_distance } else { st.distance },
                card_facing: *card,
                parts: parts[st.mesh]
                    .iter()
                    .filter_map(|p| match p {
                        PartGpu::Shared(m, PartMaterial::Standard(h)) => Some((m.clone(), h.clone())),
                        _ => None,
                    })
                    .collect(),
                ground_tint: st.ground_tint,
            })
            .collect();
        let mut layer_colour = vec![None; fb_terrain::layers::MAX_LAYERS];
        for g in &level.ground {
            let c = load::mean_linear(&level.textures[g.texture]);
            for &l in &g.layers {
                if let Some(slot) = layer_colour.get_mut(l as usize) {
                    *slot = c;
                }
            }
        }
        log.line(format!("scatter: {} kinds ready", kinds.len()));
        for k in kinds.iter().filter(|k| k.label.contains("backdrop") || k.card_facing.is_some()) {
            log.line(format!(
                "  scatter {}: draw distance {:.0} m (backdrop rule), {}",
                k.label,
                k.distance,
                if k.card_facing.is_some() { "one card, faces the camera" } else { "not one card" }
            ));
        }
        // Where the scatter layers are: covered area (sum of mask weights,
        // whole map on an 8 m grid) by distance from the level centre.
        if let Some(masks) = level.terrain.layers.as_ref() {
            const RINGS: [f32; 5] = [250.0, 500.0, 1000.0, 2000.0, f32::MAX];
            let mut used: Vec<usize> = kinds.iter().map(|k| k.layer).collect();
            used.sort();
            used.dedup();
            const GRID: usize = 16;
            let mut area = vec![[0.0f64; RINGS.len()]; fb_terrain::layers::MAX_LAYERS];
            // Coarse map: hectares per cell of a 16 x 16 grid over the map.
            let mut map = vec![[[0.0f64; GRID]; GRID]; fb_terrain::layers::MAX_LAYERS];
            let mut w = [0.0f32; fb_terrain::layers::MAX_LAYERS];
            let step = 8.0f32;
            let n = (masks.width / step) as i32;
            for iz in 0..n {
                for ix in 0..n {
                    let (x, z) = (masks.min[0] + (ix as f32 + 0.5) * step, masks.min[1] + (iz as f32 + 0.5) * step);
                    if masks.weights_at(x, z, &[false; fb_terrain::layers::MAX_LAYERS], &mut w).is_none() {
                        continue;
                    }
                    let d = (x - level.centre[0]).hypot(z - level.centre[2]);
                    let ring = RINGS.iter().position(|&r| d < r).unwrap_or(RINGS.len() - 1);
                    let (gx, gz) = (ix as usize * GRID / n as usize, iz as usize * GRID / n as usize);
                    for &l in &used {
                        area[l][ring] += f64::from(w[l]) * f64::from(step * step) / 1e6;
                        map[l][gz][gx] += f64::from(w[l]) * f64::from(step * step) / 1e4;
                    }
                }
            }
            let cell = masks.width / GRID as f32;
            let grid_of = |x: f32, z: f32| (((x - masks.min[0]) / cell) as i32, ((z - masks.min[1]) / cell) as i32);
            log.line(format!(
                "  scatter map: {GRID} x {GRID} cells of {cell:.0} m from x {:.0}, z {:.0} (rows = z rising, columns = x rising); level centre in cell {:?}",
                masks.min[0],
                masks.min[1],
                grid_of(level.centre[0], level.centre[2])
            ));
            for &l in &used {
                let rings: Vec<String> = area[l].iter().map(|a| format!("{a:.3}")).collect();
                let names: Vec<&str> = kinds.iter().filter(|k| k.layer == l).map(|k| k.label.as_str()).collect();
                log.line(format!(
                    "  scatter layer {l:>2} cover: {:.3} km2; by distance from the centre (<250, <500, <1000, <2000, more m): [{}] km2; kinds {names:?}",
                    area[l].iter().sum::<f64>(),
                    rings.join(", ")
                ));
                if area[l].iter().sum::<f64>() > 0.01 {
                    for (gz, row) in map[l].iter().enumerate() {
                        let cells: Vec<String> = row.iter().map(|ha| format!("{:>3.0}", ha)).collect();
                        log.line(format!("    layer {l:>2} hectares, row {gz:>2}: {}", cells.join("")));
                    }
                }
            }
        }
        commands.insert_resource(scatter::Scatter::new(kinds, layer_colour));
    } else {
        log.line("scatter: off");
    }
    let (mut entities, mut terrain_coloured) = (0usize, 0usize);
    // Test switch: BF1_DECALS=off leaves the decal parts out (to compare).
    let decals_on = std::env::var("BF1_DECALS").map_or(true, |v| v != "off");
    for (mesh_index, matrix) in &level.placements {
        let m = Mat4::from_cols_array(matrix);
        commands.spawn((Transform::from_matrix(m), Visibility::default())).with_children(|c| {
            for part in &parts[*mesh_index] {
                if let PartGpu::Shared(_, PartMaterial::Decal(_)) = part {
                    if !decals_on {
                        continue;
                    }
                }
                if let PartGpu::Shared(mesh, material) = part {
                    match material {
                        PartMaterial::Standard(h) => c.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(h.clone()))),
                        PartMaterial::Paint(h) => c.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(h.clone()))),
                        PartMaterial::Detail(h) => c.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(h.clone()))),
                        PartMaterial::Decal(h) => c.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(h.clone()), bevy::light::NotShadowCaster)),
                    };
                    entities += 1;
                }
            }
        });
        // Terrain-coloured parts: a world-space copy with texture coordinates from the terrain colour tile.
        for part in &parts[*mesh_index] {
            let PartGpu::TerrainColoured(p) = part else { continue };
            let world: Vec<[f32; 3]> = p.positions.iter().map(|v| m.transform_point3(Vec3::from(*v)).to_array()).collect();
            let normals: Vec<[f32; 3]> = p.normals.iter().map(|n| m.transform_vector3(Vec3::from(*n)).normalize_or_zero().to_array()).collect();
            let centre = m.transform_point3(Vec3::ZERO);
            let centre = convert::to_frostbite(centre.to_array());
            let material = match level.terrain.colour_at(centre[0], centre[2]) {
                Some((tile, _)) => {
                    let uvs = world
                        .iter()
                        .map(|v| {
                            let f = convert::to_frostbite(*v);
                            level.terrain.colour_uv(tile, f[0], f[2])
                        })
                        .collect();
                    commands.spawn((Mesh3d(meshes.add(make_mesh(world.clone(), normals, uvs, vec![], p.indices.clone()))), MeshMaterial3d(colour_materials[tile].clone())));
                    terrain_coloured += 1;
                    continue;
                }
                None => grey.clone(),
            };
            commands.spawn((Mesh3d(meshes.add(make_mesh(world, normals, vec![], vec![], p.indices.clone()))), MeshMaterial3d(material)));
        }
    }
    log.line(format!(
        "spawned {} placements as {entities} shared mesh entities and {terrain_coloured} terrain-coloured copies",
        level.placements.len()
    ));

    // Terrain.
    let centre_fb = convert::to_frostbite(level.centre);
    let centre_fb = [centre_fb[0], centre_fb[2]];
    let (patches, build_stats) = terrain_mesh::build(&level.terrain, centre_fb, 512.0, 64.0, 1.0, 8.0);
    let ground = materials.add(StandardMaterial { base_color: Color::srgb(0.36, 0.34, 0.26), perceptual_roughness: 0.95, ..default() });
    let (mut verts, mut tris) = (0usize, 0usize);
    for p in patches.iter() {
        verts += p.positions.len();
        tris += p.indices.len() / 3;
    }
    let count = patches.len();
    // Terrain shader materials: one per colour tile, plus one for patches without a colour tile.
    let terrain_material = |base: StandardMaterial, terrain_materials: &mut Assets<ground::TerrainMaterial>| {
        ground_layers.as_ref().map(|ext| terrain_materials.add(ground::TerrainMaterial { base, extension: ext.clone() }))
    };
    let tile_terrain: Vec<Option<Handle<ground::TerrainMaterial>>> = colour_images
        .iter()
        .map(|img| terrain_material(StandardMaterial { base_color_texture: Some(img.clone()), perceptual_roughness: 0.95, ..default() }, &mut terrain_materials))
        .collect();
    let plain_terrain = terrain_material(StandardMaterial { base_color: Color::srgb(0.36, 0.34, 0.26), perceptual_roughness: 0.95, ..default() }, &mut terrain_materials);
    for p in patches {
        if p.indices.is_empty() {
            continue;
        }
        let mesh = Mesh3d(meshes.add(make_mesh(p.positions, p.normals, p.uvs, vec![], p.indices)));
        let shader_material = match p.colour {
            Some(c) => tile_terrain[c].clone(),
            None => plain_terrain.clone(),
        };
        match shader_material {
            Some(m) => commands.spawn((mesh, MeshMaterial3d(m))),
            None => commands.spawn((mesh, MeshMaterial3d(p.colour.map(|c| colour_materials[c].clone()).unwrap_or_else(|| ground.clone())))),
        };
    }
    log.line(format!("terrain: {count} patches, {verts} vertices, {tris} triangles"));
    log.line(format!(
        "terrain colour tiles: {}; patches with holes: {}; triangles left out for holes: {}",
        colour_materials.len(),
        build_stats.patches_with_holes,
        build_stats.triangles_left_out
    ));
    log.line(format!(
        "terrain seams: {} patch edges next to a different grid; largest gap {:.2} m at Frostbite x {:.0}, z {:.0}; skirts {} triangles, deepest {:.2} m",
        build_stats.seam_edges,
        build_stats.max_gap,
        build_stats.max_gap_at[0],
        build_stats.max_gap_at[1],
        build_stats.skirt_triangles,
        build_stats.max_skirt_depth
    ));
    log.line(format!("level spawned in {:.2} s", t.elapsed().as_secs_f64()));
}

/// Every 5 seconds: camera position (Bevy and Frostbite coordinates) and frame rate.
fn status_log(
    time: Res<Time>,
    mut next: Local<f32>,
    log: Res<GameLog>,
    diagnostics: Res<DiagnosticsStore>,
    cams: Query<&Transform, With<camera::FlyCam>>,
) {
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 5.0;
    let fps = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    for tf in &cams {
        let p = tf.translation;
        let f = convert::to_frostbite(p.to_array());
        log.line(format!(
            "camera bevy ({:.1}, {:.1}, {:.1}) frostbite ({:.1}, {:.1}, {:.1})  fps {:.0}",
            p.x, p.y, p.z, f[0], f[1], f[2], fps
        ));
    }
}

/// For test runs: if BF1_SCREENSHOT_AT=<seconds> is set, capture one frame at that
/// time, save it as logs/screenshot-<time>.png and log numbers about the lower
/// 40% of the frame (mostly ground): mean colour and fine detail (mean brightness
/// difference between side-by-side pixels, 0 to 255).
fn screenshot_test(mut commands: Commands, time: Res<Time>, log: Res<GameLog>, mut done: Local<bool>) {
    let Some(at) = std::env::var("BF1_SCREENSHOT_AT").ok().and_then(|s| s.parse::<f32>().ok()) else { return };
    if *done || time.elapsed_secs() < at {
        return;
    }
    *done = true;
    use bevy::render::view::screenshot::{save_to_disk, Screenshot, ScreenshotCaptured};
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = format!("logs/screenshot-{stamp}.png");
    log.line(format!("screenshot requested: {path}"));
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path))
        .observe(|shot: On<ScreenshotCaptured>, log: Res<GameLog>, hits: Option<Res<view_probe::ProbeHits>>| {
            let img = &shot.image;
            let (w, h) = (img.width() as usize, img.height() as usize);
            let Some(data) = img.data.as_ref() else { return };
            if data.len() < w * h * 4 {
                return;
            }
            if let Some(hits) = hits {
                let bgr = matches!(img.texture_descriptor.format, bevy::render::render_resource::TextureFormat::Bgra8UnormSrgb | bevy::render::render_resource::TextureFormat::Bgra8Unorm);
                view_probe::log_colours(&hits, w, h, data, bgr, &log);
            }
            let lum = |x: usize, y: usize| {
                let i = (y * w + x) * 4;
                (data[i] as f64 + data[i + 1] as f64 + data[i + 2] as f64) / 3.0
            };
            let mut all = 0f64;
            for y in 0..h {
                for x in 0..w {
                    all += lum(x, y);
                }
            }
            let (mut detail, mut n) = (0f64, 0f64);
            let mut sum = [0f64; 3];
            for y in h * 6 / 10..h {
                for x in 0..w - 1 {
                    detail += (lum(x, y) - lum(x + 1, y)).abs();
                    let i = (y * w + x) * 4;
                    for c in 0..3 {
                        sum[c] += data[i + c] as f64;
                    }
                    n += 1.0;
                }
            }
            log.line(format!(
                "screenshot {}x{} {:?}: whole frame mean brightness {:.0}; lower 40% mean bytes {:.0?} (channel order of the format), fine detail {:.2}",
                w,
                h,
                img.texture_descriptor.format,
                all / (w * h) as f64,
                sum.map(|s| s / n.max(1.0)),
                detail / n.max(1.0)
            ));
        });
}

/// The material of a drawn mesh part.
#[derive(Clone)]
enum PartMaterial {
    Standard(Handle<StandardMaterial>),
    /// Painted (guns): colour texture plus paint mask (`paint.rs`).
    Paint(Handle<paint::PaintMaterial>),
    /// Buildings: base texture plus detail texture (`detail.rs`).
    Detail(Handle<detail::DetailMaterial>),
    /// Decals: blended over the surface under them, casting no shadow
    /// (DESIGN.md "decal variants", step 7).
    Decal(Handle<StandardMaterial>),
}

/// Window size: `BF1_WINDOW=<width>x<height>` (for screenshots at the size of
/// the user's reference screenshots), else Bevy's default.
fn window_size() -> bevy::window::WindowResolution {
    let parsed = std::env::var("BF1_WINDOW").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?))
    });
    match parsed {
        Some((w, h)) => bevy::window::WindowResolution::new(w, h).with_scale_factor_override(1.0),
        None => default(),
    }
}

/// For test runs: if BF1_EXIT_AFTER=<seconds> is set, close the program after that time.
fn exit_after_env(time: Res<Time>, log: Res<GameLog>, mut exit: MessageWriter<AppExit>) {
    let Some(limit) = std::env::var("BF1_EXIT_AFTER").ok().and_then(|s| s.parse::<f32>().ok()) else { return };
    if time.elapsed_secs() > limit {
        log.line(format!("BF1_EXIT_AFTER={limit}: closing"));
        exit.write(AppExit::Success);
    }
}

/// Decode a 260 x 260 BC3 terrain colour tile to RGBA8 and add a mip chain
/// (2 x 2 box filter), so the ground does not flicker in the distance.
/// Decode a terrain colour tile, replace the game's marker pixels with the
/// ground colour (see `fb_terrain::fill_markers`), and build its mip chain.
/// Returns the image and the number of marker pixels replaced.
fn terrain_colour_image(data: &[u8], ground: [u8; 3]) -> (Image, usize) {
    let size = fb_terrain::COLOUR_SIZE;
    let mut px = fb_terrain::decode_colour(data);
    let markers = fb_terrain::fill_markers(&mut px, ground);
    let mut level: Vec<[u8; 4]> = px.iter().map(|p| [p[0], p[1], p[2], 255]).collect();
    let (mut w, mut h) = (size, size);
    let mut bytes: Vec<u8> = level.iter().flatten().copied().collect();
    let mut mips = 1u32;
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![[0u8; 4]; nw * nh];
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = level[(y * 2 + dy).min(h - 1) * w + (x * 2 + dx).min(w - 1)];
                    for c in 0..4 {
                        acc[c] += p[c] as u32;
                    }
                }
                next[y * nw + x] = acc.map(|v| (v / 4) as u8);
            }
        }
        bytes.extend(next.iter().flatten());
        level = next;
        (w, h) = (nw, nh);
        mips += 1;
    }
    let mut image = Image::default();
    image.texture_descriptor.size = Extent3d { width: size as u32, height: size as u32, depth_or_array_layers: 1 };
    image.texture_descriptor.dimension = TextureDimension::D2;
    image.texture_descriptor.format = TextureFormat::Rgba8UnormSrgb;
    image.texture_descriptor.mip_level_count = mips;
    image.data = Some(bytes);
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    (image, markers)
}
