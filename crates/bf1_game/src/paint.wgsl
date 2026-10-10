// Painted parts (guns, howitzers, mortars): Bevy's StandardMaterial (colour
// texture in base_color_texture, base colour white) plus the paint. Where the
// `Masks` texture's green channel is set, the colour is multiplied by the
// paint factor; elsewhere the plain texture shows (see paint.rs).
// Forward rendering only.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}

// rgb: paint factor (linear), a: unused.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> paint: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var paint_mask: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var paint_mask_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
#ifdef VERTEX_UVS_A
    let painted = textureSample(paint_mask, paint_mask_sampler, in.uv).g;
#else
    let painted = 1.0;
#endif
    let factor = mix(vec3(1.0), paint.rgb, painted);
    pbr_input.material.base_color = vec4(pbr_input.material.base_color.rgb * factor, pbr_input.material.base_color.a);
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
