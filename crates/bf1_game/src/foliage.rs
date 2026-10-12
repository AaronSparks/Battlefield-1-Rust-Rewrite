//! Shiny leaves (DESIGN.md "shiny leaves and wet ground", step 1). Foliage
//! materials name a second texture (`NormalTranslucencySmoothnessMap`, or a
//! `..._NTS` / `..._NNRS` normal map) whose alpha is the smoothness. This
//! material is Bevy's StandardMaterial (colour texture, cut-out) plus that
//! texture; `foliage.wgsl` sets the roughness per pixel to 1 - alpha, so the
//! leaves can reflect the sun.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

pub type FoliageMaterial = ExtendedMaterial<StandardMaterial, Smoothness>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct Smoothness {
    /// The normal / translucency / smoothness texture (alpha = smoothness).
    #[texture(100)]
    #[sampler(101)]
    pub texture: Handle<Image>,
}

impl MaterialExtension for Smoothness {
    fn fragment_shader() -> ShaderRef {
        "embedded://bf1_game/foliage.wgsl".into()
    }
}

/// Register the foliage material and its embedded shader.
pub fn plugin(app: &mut App) {
    bevy::asset::embedded_asset!(app, "foliage.wgsl");
    app.add_plugins(MaterialPlugin::<FoliageMaterial>::default());
}
