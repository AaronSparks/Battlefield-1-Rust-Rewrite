// Shiny leaves: Bevy's StandardMaterial (colour texture, cut-out by its
// alpha) plus the foliage's normal / translucency / smoothness texture, whose
// alpha sets the roughness (1 - smoothness) per pixel (see foliage.rs).
// Forward rendering only.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var smoothness_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var smoothness_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
#ifdef VERTEX_UVS_A
    let smoothness = textureSample(smoothness_texture, smoothness_sampler, in.uv).a;
    pbr_input.material.perceptual_roughness = clamp(1.0 - smoothness, 0.089, 1.0);
#endif
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
