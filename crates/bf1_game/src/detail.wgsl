// Building parts with a detail texture (DESIGN.md "flat grey walls"):
// Bevy's StandardMaterial (base texture on the first texture coordinate set
// of the mesh as drawn) plus a detail texture and its mask on the second set:
// colour = mix(base, detail, mask alpha). The mask is the `_NormalDetail`
// alpha (`..._DetailsOverlay`) or the detail's own alpha (`FortDetailBlend2UV`;
// the loader swaps the sets for those). Forward rendering only.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(101) var detail_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var detail_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var mask_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var mask_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
#ifdef VERTEX_UVS_B
    let d = textureSample(detail_texture, detail_sampler, in.uv_b);
    let m = textureSample(mask_texture, mask_sampler, in.uv_b).a;
    let rgb = mix(pbr_input.material.base_color.rgb, d.rgb, m);
    pbr_input.material.base_color = vec4(rgb, pbr_input.material.base_color.a);
#endif
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
