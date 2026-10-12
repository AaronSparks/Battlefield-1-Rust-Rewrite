// Terrain blend on mounds and dirt piles (DESIGN.md "terrain blend on
// mounds"): Bevy's StandardMaterial (the mesh's own texture and normal map)
// plus the terrain's ground layers. Faces that point up more than the
// material's threshold take the ground layers at the same world x, z (our
// reading of the game's terrain blend shaders; not confirmed). The ground
// sampling is the same as in terrain.wgsl.
// Forward rendering only.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    mesh_view_bindings::view,
}

struct GroundParams {
    // x: texture count, y: metres per repeat, z: weight atlas planes, w: normal strength.
    info: vec4<f32>,
    // x, y: detail fade start and end distance (m).
    fade: vec4<f32>,
    // Fine and coarse atlas: world x0, z0, size (m), present (1 or 0).
    fine: vec4<f32>,
    coarse: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> ground: GroundParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var ground_layers: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var ground_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var fine_weights: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var fine_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var coarse_weights: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var coarse_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var ground_normals: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var normals_sampler: sampler;
// x: threshold (up component of the world normal), y: half width of the soft edge.
@group(#{MATERIAL_BIND_GROUP}) @binding(109) var<uniform> blend: vec4<f32>;

// Weights of up to 16 ground textures at a world point (as in terrain.wgsl).
fn weights_at(xz: vec2<f32>) -> array<f32, 16> {
    var w: array<f32, 16>;
    let planes = i32(ground.info.z);
    let fuv = (xz - ground.fine.xy) / ground.fine.z;
    let in_fine = ground.fine.w > 0.5 && all(fuv >= vec2(0.0)) && all(fuv <= vec2(1.0));
    let cuv = (xz - ground.coarse.xy) / ground.coarse.z;
    for (var p = 0; p < planes; p++) {
        var v = vec4(0.0);
        if (in_fine) {
            v = textureSampleLevel(fine_weights, fine_sampler, fuv, p, 0.0);
        } else if (ground.coarse.w > 0.5) {
            v = textureSampleLevel(coarse_weights, coarse_sampler, cuv, p, 0.0);
        }
        w[p * 4] = v.x;
        w[p * 4 + 1] = v.y;
        w[p * 4 + 2] = v.z;
        w[p * 4 + 3] = v.w;
    }
    return w;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    // Derivatives must be taken in uniform control flow, before any branch.
    let uv = in.world_position.xz / ground.info.y;
    let ddx = dpdx(uv);
    let ddy = dpdy(uv);
    let dist = distance(view.world_position.xyz, in.world_position.xyz);
    let near = 1.0 - smoothstep(ground.fade.x, ground.fade.y, dist);
    // The up mask uses the normal after the mesh's normal map, so the tops
    // of the bumps take the ground first.
    let n0 = normalize(pbr_input.N);
    let up = smoothstep(blend.x - blend.y, blend.x + blend.y, n0.y);
    let k0 = near * up;
    if (k0 > 0.0) {
        let w = weights_at(in.world_position.xz);
        let count = i32(ground.info.x);
        var sum = vec3(0.0);
        var smoothness = 0.0;
        var slope = vec2(0.0);
        var total = 0.0;
        for (var i = 0; i < count; i++) {
            if (w[i] < 0.02) {
                continue;
            }
            let c = textureSampleGrad(ground_layers, ground_sampler, uv, i, ddx, ddy);
            let n = textureSampleGrad(ground_normals, normals_sampler, uv, i, ddx, ddy);
            sum += c.rgb * w[i];
            smoothness += c.a * w[i];
            slope += (n.rg * 2.0 - 1.0) * w[i];
            total += w[i];
        }
        if (total > 0.05) {
            let k = k0 * min(total, 1.0);
            let own = pbr_input.material.base_color;
            pbr_input.material.base_color = vec4(mix(own.rgb, sum / total, k), own.a);
            pbr_input.material.perceptual_roughness = mix(pbr_input.material.perceptual_roughness, 1.0 - smoothness / total, k);
            // Bend the normal by the ground's normal maps, as on the terrain
            // (tangent along world +X, +green along world -Z).
            let t2 = slope / total * ground.info.w * k;
            let nz = sqrt(max(1.0 - dot(t2, t2), 0.0));
            let tangent = normalize(vec3(1.0, 0.0, 0.0) - n0 * n0.x);
            let bitangent = cross(n0, tangent);
            pbr_input.N = normalize(tangent * t2.x + bitangent * t2.y + n0 * nz);
        }
    }
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
