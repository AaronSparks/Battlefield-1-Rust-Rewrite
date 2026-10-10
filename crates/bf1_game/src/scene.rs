//! Camera, sun, sky dome and post-processing, set up from the level's
//! visual environment.

use crate::camera::FlyCam;
use crate::convert;
use crate::environment::{rgb9e5, Environment};
use crate::log::GameLog;
use bevy::asset::RenderAssetUsages;
use bevy::camera::Exposure;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::CascadeShadowConfigBuilder;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{DistanceFog, FogFalloff, ScreenSpaceAmbientOcclusion};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// The weather variants (index 0 = the level's base setup) and which one is shown.
#[derive(Resource)]
pub struct Env {
    pub list: Vec<Environment>,
    pub current: usize,
    /// Mean ground colour of the map (linear RGB), for the bounce light.
    pub ground_albedo: [f32; 3],
}

/// Sky dome material of each weather variant (same order as `Env::list`).
#[derive(Resource)]
pub struct SkyMaterials(pub Vec<Handle<StandardMaterial>>);

/// Fog density of the base setup: the level's own values look far too thick in our
/// renderer (user feedback, twice), so the base is light haze, 50% at 2 km.
/// Variants scale it by how much thicker their own fog curve is than the base's.
const BASE_FOG_DENSITY: f32 = std::f32::consts::LN_2 / 2000.0;

fn fog_for(e: &Environment, base: &Environment) -> DistanceFog {
    let density = BASE_FOG_DENSITY * e.fog_thickness() / base.fog_thickness();
    let c = cam_fog_colour(e);
    let sun = Color::linear_rgb(e.sun_colour[0], e.sun_colour[1], e.sun_colour[2]);
    DistanceFog {
        // The sky colour at the horizon, so far objects fade into the sky.
        color: Color::linear_rgb(c[0], c[1], c[2]),
        directional_light_color: sun.with_alpha(0.5),
        directional_light_exponent: 30.0,
        falloff: FogFalloff::Exponential { density },
    }
}

/// The battle area centre in Bevy coordinates.
#[derive(Resource)]
pub struct Centre(pub Vec3);

/// The sky dome follows the camera.
#[derive(Component)]
pub struct SkyDome;

/// Camera exposure (EV100) if the environment is missing. Normally the
/// environment's `exposure_ev` is used (the level's MaxEV, 12.5 on Verdun).
/// Until 2026-10-06 this fixed 15 was used for the world while the sky used
/// the level's EV, so the world was about 10 times too dark against the sky.
const FALLBACK_EXPOSURE_EV100: f32 = 15.0;

pub fn spawn_scene(
    mut commands: Commands,
    centre: Res<Centre>,
    env: Res<Env>,
    log: Res<GameLog>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    // Start in env.current (main.rs sets it from BF1_WEATHER); fog is relative to the base.
    let base = env.list.first();
    let e = env.list.get(env.current).or(base);
    let sun_colour = e.map(|e| Color::linear_rgb(e.sun_colour[0], e.sun_colour[1], e.sun_colour[2])).unwrap_or(Color::WHITE);

    // Start pose: 2 m above the battle area centre, or BF1_CAMERA=x,y,z,yaw,pitch
    // (Bevy coordinates, angles in degrees) for repeatable tests.
    let (start, yaw, pitch) = match std::env::var("BF1_CAMERA").ok().map(|s| s.split(',').filter_map(|v| v.trim().parse::<f32>().ok()).collect::<Vec<_>>()) {
        Some(v) if v.len() == 5 => (Vec3::new(v[0], v[1], v[2]), v[3].to_radians(), v[4].to_radians()),
        _ => (centre.0 + Vec3::new(0.0, 2.0, 0.0), 0.0, 0.0),
    };
    log.line(format!("camera start {start:.1}, yaw {:.0} deg, pitch {:.0} deg", yaw.to_degrees(), pitch.to_degrees()));

    // Camera with fog, bloom, tone mapping, ambient occlusion and anti-aliasing.
    let mut cam = commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection { far: 60_000.0, fov: fov(), ..default() }),
        Transform::from_translation(start),
        FlyCam { yaw, pitch, speed: 12.0 },
        Exposure { ev100: e.map(|e| e.exposure_ev()).unwrap_or(FALLBACK_EXPOSURE_EV100) },
        Tonemapping::TonyMcMapface,
        Bloom { intensity: e.map(|e| e.bloom_scale * 2.0).unwrap_or(0.08), ..Bloom::NATURAL },
        Msaa::Off,
        bevy::anti_alias::smaa::Smaa::default(),
        ScreenSpaceAmbientOcclusion::default(),
        ambient_for(e, env.ground_albedo),
    ));
    if let Some(e) = e {
        let density = BASE_FOG_DENSITY;
        cam.insert(fog_for(e, base.unwrap_or(e)));
        log.line(format!("fog colour (sky at the horizon): {:?}", cam_fog_colour(e)));
        log.line(format!(
            "fog: exponential density {density:.5} (amount {:.2} at 150 m, {:.2} at 500 m, {:.2} at {} m)",
            1.0 - (-density * 150.0).exp(),
            1.0 - (-density * 500.0).exp(),
            1.0 - (-density * e.fog_colour_end).exp(),
            e.fog_colour_end
        ));
    }

    // Sun: shines from the sun direction (converted to Bevy), with shadows near the camera.
    let to_sun = e.map(|e| Vec3::from(convert::point(e.sun_direction()))).unwrap_or(Vec3::new(0.3, 0.8, 0.2)).normalize();
    commands.spawn((
        DirectionalLight { color: sun_colour, illuminance: e.map(|e| e.sun_intensity).unwrap_or(100_000.0) * sun_scale(), shadow_maps_enabled: true, ..default() },
        Transform::default().looking_to(-to_sun, Vec3::Y),
        CascadeShadowConfigBuilder { maximum_distance: 200.0, first_cascade_far_bound: 15.0, ..default() }.build(),
    ));
    log.line(format!("sun: direction to sun (Bevy) {to_sun:.3}"));
    log.line(format!(
        "sky light (ambient, cd/m2 per channel): {:?}; bounce light {:?} (ground colour {:.3?})",
        e.and_then(|e| e.sky_ambient()).map(|c| c.map(|v| v.round())),
        e.map(|e| bounce_light(e, env.ground_albedo).map(|v| v.round())),
        env.ground_albedo
    ));

    // Sky dome: one material per weather variant (their panoramic textures differ).
    let mut sky_materials = Vec::new();
    for v in &env.list {
        let Some((w, h, data)) = v.sky.as_ref() else {
            sky_materials.push(materials.add(StandardMaterial { base_color: Color::linear_rgb(0.3, 0.35, 0.4), unlit: true, fog_enabled: false, cull_mode: None, ..default() }));
            continue;
        };
        let mut image = Image::default();
        image.texture_descriptor.size = Extent3d { width: *w, height: *h, depth_or_array_layers: 1 };
        image.texture_descriptor.dimension = TextureDimension::D2;
        image.texture_descriptor.format = TextureFormat::Rgb9e5Ufloat;
        image.data = Some(data.clone());
        // Repeat around the horizon (u), clamp at the top and bottom (v).
        image.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
            address_mode_u: bevy::image::ImageAddressMode::Repeat,
            address_mode_v: bevy::image::ImageAddressMode::ClampToEdge,
            mag_filter: bevy::image::ImageFilterMode::Linear,
            min_filter: bevy::image::ImageFilterMode::Linear,
            ..default()
        });
        image.asset_usage = RenderAssetUsages::RENDER_WORLD;
        let k = v.sky_brightness();
        sky_materials.push(materials.add(StandardMaterial {
            base_color: Color::linear_rgb(k, k, k),
            base_color_texture: Some(images.add(image)),
            unlit: true,
            fog_enabled: false,
            cull_mode: None,
            ..default()
        }));
        log.line(format!("sky for {}: {w}x{h} RGB9E5, average colour {:.3?}, brightness factor {k:.2}", v.name, average_rgb9e5(data)));
        log.line(format!("  sun check for {}: {}", v.name, sun_check(v, *w, *h, data)));
    }
    if let (Some(e), Some(material)) = (e, sky_materials.get(env.current).or(sky_materials.first())) {
        let mesh = sky_dome_mesh(e.panoramic_rotation, e.panoramic_uv_min_y, e.panoramic_uv_max_y);
        // The sky must not cast shadows: a 45 km sphere around the camera would shade everything.
        commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.clone()), Transform::from_translation(centre.0), SkyDome, bevy::light::NotShadowCaster, bevy::light::NotShadowReceiver));
    }
    commands.insert_resource(SkyMaterials(sky_materials));
}

/// G: switch to the next weather variant (fog, sun, sky).
pub fn cycle_weather(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut next_auto: Local<f32>,
    mut env: ResMut<Env>,
    skies: Option<Res<SkyMaterials>>,
    log: Res<GameLog>,
    mut cams: Query<(&mut DistanceFog, &mut AmbientLight, &mut Exposure)>,
    mut suns: Query<(&mut DirectionalLight, &mut Transform)>,
    mut domes: Query<&mut MeshMaterial3d<StandardMaterial>, With<SkyDome>>,
) {
    // Test mode: BF1_WEATHER_CYCLE=<seconds> switches automatically.
    let auto = std::env::var("BF1_WEATHER_CYCLE").ok().and_then(|s| s.parse::<f32>().ok());
    let auto_due = auto.is_some_and(|every| {
        if *next_auto == 0.0 {
            *next_auto = every;
        }
        time.elapsed_secs() > *next_auto
    });
    if auto_due {
        *next_auto += auto.unwrap();
    }
    if (!keys.just_pressed(KeyCode::KeyG) && !auto_due) || env.list.len() < 2 {
        return;
    }
    env.current = (env.current + 1) % env.list.len();
    let (base, e) = (&env.list[0], &env.list[env.current]);
    for (mut fog, mut ambient, mut exposure) in &mut cams {
        *fog = fog_for(e, base);
        exposure.ev100 = e.exposure_ev();
        *ambient = ambient_for(Some(e), env.ground_albedo);
    }
    let to_sun = Vec3::from(convert::point(e.sun_direction())).normalize();
    for (mut light, mut tf) in &mut suns {
        light.color = Color::linear_rgb(e.sun_colour[0], e.sun_colour[1], e.sun_colour[2]);
        light.illuminance = e.sun_intensity * sun_scale();
        *tf = Transform::default().looking_to(-to_sun, Vec3::Y);
    }
    if let Some(skies) = skies {
        for mut m in &mut domes {
            m.0 = skies.0[env.current].clone();
        }
    }
    let density = BASE_FOG_DENSITY * e.fog_thickness() / base.fog_thickness();
    log.line(format!(
        "weather: {} (fog density {density:.5}, {:.0}% at 500 m; sun {:.0} lux)",
        e.name,
        (1.0 - (-density * 500.0).exp()) * 100.0,
        e.sun_intensity
    ));
}

/// Vertical field of view: Bevy's default (45 degrees), or
/// `BF1_FOV=<degrees>` (to match a reference screenshot).
fn fov() -> f32 {
    std::env::var("BF1_FOV").ok().and_then(|v| v.parse::<f32>().ok()).map(f32::to_radians).unwrap_or(std::f32::consts::FRAC_PI_4)
}

/// The level's SunIntensity (100,000 on Verdun) used as lux makes the sunlit
/// ground about 2.5 times brighter than in the game at the level's own exposure.
/// With 1/pi of it (a common factor between lighting conventions; we do not know
/// Frostbite's), the user's 1:1 screenshot of the same view matches: whole frame
/// (56, 53, 56) against (57, 62, 65), and the fire rain screenshots stay in range
/// (MODLOG 2026-10-07). A calibration, not a known rule.
pub const SUN_UNITS: f32 = 1.0 / std::f32::consts::PI;

/// Scale for the level's sun intensity: SUN_UNITS, or BF1_SUN_SCALE=<factor> for tests.
fn sun_scale() -> f32 {
    std::env::var("BF1_SUN_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(SUN_UNITS)
}

/// Ambient (sky) light from the weather's sky texture (`Environment::sky_ambient`).
/// Until 2026-10-07 this was the level's SkyColor times 12,000, a value set by
/// eye: about twice the sky light the data gives. Fallback without a sky texture.
/// Ambient light: the sky light from the data plus a bounce term (sunlight
/// reflected by the ground, which the game's global illumination gives and we
/// do not compute): ground colour x sun on level ground / pi x `BOUNCE`.
pub fn ambient_for(e: Option<&Environment>, ground_albedo: [f32; 3]) -> AmbientLight {
    match e.and_then(|e| e.sky_ambient().map(|sky| (e, sky))) {
        Some((e, sky)) => {
            let b = bounce_light(e, ground_albedo);
            let c: [f32; 3] = std::array::from_fn(|k| sky[k] * sky_scale() + b[k]);
            let m = c[0].max(c[1]).max(c[2]).max(1e-6);
            AmbientLight { color: Color::linear_rgb(c[0] / m, c[1] / m, c[2] / m), brightness: m, ..default() }
        }
        None => {
            let c = e.map(|e| e.sky_colour).unwrap_or([0.32, 0.34, 0.4]);
            AmbientLight { color: Color::linear_rgb(c[0], c[1], c[2]), brightness: 12_000.0, ..default() }
        }
    }
}

/// Share of the bounce light (a fit to the user's screenshots, MODLOG
/// 2026-10-07; `BF1_BOUNCE` overrides it for tests).
pub const BOUNCE: f32 = 2.0;

/// Bounce light (cd/m2, linear RGB): sunlight on level ground, reflected by
/// the map's mean ground colour, spread evenly (divided by pi), times `BOUNCE`.
pub fn bounce_light(e: &Environment, ground_albedo: [f32; 3]) -> [f32; 3] {
    let share = std::env::var("BF1_BOUNCE").ok().and_then(|v| v.parse().ok()).unwrap_or(BOUNCE);
    let height = e.sun_direction()[1].max(0.0);
    let level = e.sun_intensity * sun_scale() * height / std::f32::consts::PI;
    std::array::from_fn(|k| ground_albedo[k] * e.sun_colour[k] * level * share)
}

/// Scale of the sky (ambient) light from the data: 1, or `BF1_SKY_SCALE`.
fn sky_scale() -> f32 {
    std::env::var("BF1_SKY_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0)
}

/// Keep the sky dome centred on the camera.
pub fn follow_sky(cams: Query<&Transform, (With<FlyCam>, Without<SkyDome>)>, mut sky: Query<&mut Transform, With<SkyDome>>) {
    if let (Ok(cam), Ok(mut s)) = (cams.single(), sky.single_mut()) {
        s.translation = cam.translation;
    }
}

/// Compare the sun direction with the brightest point of the sky texture,
/// using the same mapping as `sky_dome_mesh` (Frostbite directions). If the sky
/// has a sun disc, a small angle means the sun light and the sky agree. Also
/// gives the angle if the texture ran the other way round the horizon.
fn sun_check(e: &crate::environment::Environment, w: u32, h: u32, data: &[u8]) -> String {
    let lum = |i: usize| {
        let p = rgb9e5(u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()));
        0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
    };
    let n = (w * h) as usize;
    let Some(best) = (0..n).max_by(|&a, &b| lum(a).total_cmp(&lum(b))) else { return "empty sky".into() };
    let mut all: Vec<f32> = (0..n).step_by(97).map(lum).collect();
    all.sort_by(f32::total_cmp);
    let median = all[all.len() / 2].max(1e-6);
    let (x, y) = (best % w as usize, best / w as usize);
    let (u, v) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
    let span = (e.panoramic_uv_max_y - e.panoramic_uv_min_y).max(1e-6);
    let el = ((1.0 - (v - e.panoramic_uv_min_y) / span) * std::f32::consts::FRAC_PI_2).clamp(-0.5, std::f32::consts::FRAC_PI_2);
    let t = u - e.panoramic_rotation;
    let sun = Vec3::from(e.sun_direction()).normalize();
    let angle = |t: f32| Vec3::from(sky_direction(t, el)).angle_between(sun).to_degrees();
    format!(
        "brightest texel ({x}, {y}) is {:.0}x the median, elevation {:.0} deg; angle to the sun direction {:.0} deg (texture running the other way: {:.0} deg)",
        lum(best) / median,
        el.to_degrees(),
        angle(t),
        angle(-t)
    )
}

fn average_rgb9e5(data: &[u8]) -> [f32; 3] {
    let mut sum = [0f64; 3];
    let n = data.len() / 4;
    for px in data.chunks_exact(4) {
        let p = rgb9e5(u32::from_le_bytes(px.try_into().unwrap()));
        for c in 0..3 {
            sum[c] += p[c] as f64;
        }
    }
    sum.map(|s| (s / n as f64) as f32)
}

/// Frostbite direction for a point of the sky texture: `t` is the turn around
/// the horizon (texture u minus the panoramic rotation), `el` the elevation in
/// radians. As t grows the direction turns clockwise seen from above (to the
/// viewer's right), so the picture is not mirrored from inside. Checked against
/// the sun: see `sun_check` in the log.
fn sky_direction(t: f32, el: f32) -> [f32; 3] {
    let az = t * std::f32::consts::TAU;
    [-az.sin() * el.cos(), el.sin(), az.cos() * el.cos()]
}

/// A dome (sphere, radius 45 km) seen from inside. The texture's u goes
/// around the horizon (with the panoramic rotation, in turns); v goes from
/// the zenith (uv_min_y) to the horizon (uv_max_y). Below the horizon the
/// horizon row is repeated.
fn sky_dome_mesh(rotation: f32, uv_min_y: f32, uv_max_y: f32) -> Mesh {
    // Beyond the 32 km backdrop ring around the map.
    const R: f32 = 45_000.0;
    let (seg_u, seg_v) = (96usize, 48usize);
    let (mut pos, mut uv, mut idx, mut normals) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for j in 0..=seg_v {
        // Elevation from +90 (top) to -30 degrees.
        let el = (90.0 - 120.0 * j as f32 / seg_v as f32).to_radians();
        for i in 0..=seg_u {
            let t = i as f32 / seg_u as f32;
            // Frostbite direction, then converted to Bevy.
            let dir_fb = sky_direction(t, el);
            pos.push(convert::point(dir_fb.map(|c| c * R)));
            normals.push(convert::point(dir_fb.map(|c| -c)));
            let v = uv_min_y + (1.0 - el.max(0.0) / std::f32::consts::FRAC_PI_2) * (uv_max_y - uv_min_y);
            uv.push([t + rotation, v]);
        }
    }
    for j in 0..seg_v {
        for i in 0..seg_u {
            let a = (j * (seg_u + 1) + i) as u32;
            let b = a + seg_u as u32 + 1;
            idx.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_indices(Indices::U32(idx))
}

/// Average sky colour of the texture row at the horizon (v = uv_max_y), linear.
fn horizon_colour(w: u32, h: u32, data: &[u8], uv_max_y: f32) -> [f32; 3] {
    let (w, h) = (w as usize, h as usize);
    let row = ((uv_max_y.clamp(0.0, 1.0) * h as f32) as usize).min(h - 1).saturating_sub(4);
    let mut sum = [0f32; 3];
    for x in 0..w {
        let i = (row * w + x) * 4;
        let p = rgb9e5(u32::from_le_bytes(data[i..i + 4].try_into().unwrap()));
        for c in 0..3 {
            sum[c] += p[c];
        }
    }
    sum.map(|s| s / w as f32)
}

fn cam_fog_colour(e: &Environment) -> [f32; 3] {
    e.sky.as_ref().map(|(w, h, d)| horizon_colour(*w, *h, d, e.panoramic_uv_max_y).map(|x| x * e.sky_brightness())).unwrap_or([0.3, 0.33, 0.37])
}
