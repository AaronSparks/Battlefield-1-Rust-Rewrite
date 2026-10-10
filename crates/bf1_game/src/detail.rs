//! Building parts with a detail texture (DESIGN.md "flat grey walls"). The
//! game's building shaders draw a repeating base texture on the first texture
//! coordinate set and a detail texture on the second; faces that should show
//! only one of them have the other set collapsed to one point. This material
//! is Bevy's StandardMaterial (base texture) plus the detail texture and its
//! mask; `detail.wgsl` mixes them per pixel: mix(base, detail, mask alpha).

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

pub type DetailMaterial = ExtendedMaterial<StandardMaterial, DetailTexture>;

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct DetailTexture {
    #[texture(101)]
    #[sampler(102)]
    pub texture: Handle<Image>,
    /// Its alpha is the blend mask.
    #[texture(103)]
    #[sampler(104)]
    pub mask: Handle<Image>,
}

impl MaterialExtension for DetailTexture {
    fn fragment_shader() -> ShaderRef {
        "embedded://bf1_game/detail.wgsl".into()
    }
}

/// Register the detail material and its embedded shader.
pub fn plugin(app: &mut App) {
    bevy::asset::embedded_asset!(app, "detail.wgsl");
    app.add_plugins(MaterialPlugin::<DetailMaterial>::default());
}
