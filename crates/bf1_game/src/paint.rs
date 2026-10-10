//! Painted parts (DESIGN.md "material tints", part 1b). The game's paint
//! shader colours a neutral grey texture with a paint colour, only where the
//! material's `Masks` texture (green channel) says "painted". This material
//! is Bevy's StandardMaterial (the colour texture, base colour white) plus
//! the mask and the paint factor; `paint.wgsl` mixes them per pixel.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

pub type PaintMaterial = ExtendedMaterial<StandardMaterial, PaintMask>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct PaintMask {
    /// rgb: paint factor (linear; multiplies the colour texture where painted).
    #[uniform(100)]
    pub paint: Vec4,
    #[texture(101)]
    #[sampler(102)]
    pub mask: Handle<Image>,
}

impl MaterialExtension for PaintMask {
    fn fragment_shader() -> ShaderRef {
        "embedded://bf1_game/paint.wgsl".into()
    }
}

/// Register the paint material and its embedded shader.
pub fn plugin(app: &mut App) {
    bevy::asset::embedded_asset!(app, "paint.wgsl");
    app.add_plugins(MaterialPlugin::<PaintMaterial>::default());
}
