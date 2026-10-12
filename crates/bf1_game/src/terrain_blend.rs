//! Terrain blend on mounds and dirt piles (DESIGN.md "terrain blend on
//! mounds"). This material is Bevy's StandardMaterial (the part's texture and
//! normal map) plus the terrain's ground textures and weight atlases: the
//! same images as the terrain material, so it needs no new memory.
//! `terrain_blend.wgsl` mixes in the ground layers on faces that point up
//! more than the material's threshold (our reading of the game's terrain
//! blend shaders; not confirmed).

use crate::ground::{GroundLayers, GroundParams};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

pub type TerrainBlendMaterial = ExtendedMaterial<StandardMaterial, GroundBlend>;

/// Half width of the soft edge around the threshold (a guess; the game's
/// `UpNormalMaskOffset` may set it, but its meaning is not known).
pub const EDGE: f32 = 0.1;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct GroundBlend {
    /// Bindings 100 to 108 as in the terrain material (`ground.rs`).
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
    #[texture(107, dimension = "2d_array")]
    #[sampler(108)]
    pub normals: Handle<Image>,
    /// x: threshold (up component of the world normal), y: half width of the
    /// soft edge; z, w: unused.
    #[uniform(109)]
    pub blend: Vec4,
}

impl GroundBlend {
    /// The terrain's ground layers with the blend threshold of one material.
    pub fn new(ground: &GroundLayers, threshold: f32) -> GroundBlend {
        GroundBlend {
            params: ground.params.clone(),
            layers: ground.layers.clone(),
            fine_weights: ground.fine_weights.clone(),
            coarse_weights: ground.coarse_weights.clone(),
            normals: ground.normals.clone(),
            blend: Vec4::new(threshold, EDGE, 0.0, 0.0),
        }
    }
}

impl MaterialExtension for GroundBlend {
    fn fragment_shader() -> ShaderRef {
        "embedded://bf1_game/terrain_blend.wgsl".into()
    }
}

/// Register the terrain blend material and its embedded shader.
pub fn plugin(app: &mut App) {
    bevy::asset::embedded_asset!(app, "terrain_blend.wgsl");
    app.add_plugins(MaterialPlugin::<TerrainBlendMaterial>::default());
}
