// PBR-lite / Blinn-Phong 3D scene shader for tinyjump `tj-gfx` (wgpu WGSL).

struct Light {
    // xyz = direction (directional, w = 0) or world position (point, w = 1)
    pos_or_dir: vec4<f32>,
    // rgb = linear color, w = intensity
    color_intensity: vec4<f32>,
}

struct SceneUniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // rgb = ambient linear color, w = active light count
    ambient_and_count: vec4<f32>,
    lights: array<Light, 8>,
}

struct MeshUniform {
    model: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    // rgb = linear base color, a = 1.0
    base_color: vec4<f32>,
    // x = metallic, y = roughness
    material: vec4<f32>,
}

@group(0) @binding(0) var<uniform> scene: SceneUniform;
@group(1) @binding(0) var<uniform> mesh: MeshUniform;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
}

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
}

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    let world = mesh.model * vec4<f32>(v.pos, 1.0);
    out.clip_pos = scene.view_proj * world;
    out.world_pos = world.xyz;
    let n = (mesh.normal_mat * vec4<f32>(v.normal, 0.0)).xyz;
    out.world_normal = normalize(n);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let view_dir = normalize(scene.camera_pos.xyz - in.world_pos);
    let albedo = mesh.base_color.rgb;
    let metallic = clamp(mesh.material.x, 0.0, 1.0);
    let roughness = clamp(mesh.material.y, 0.05, 1.0);
    let shininess = mix(96.0, 4.0, roughness * roughness);
    let spec_color = mix(vec3<f32>(0.04, 0.04, 0.04), albedo, metallic);
    let diff_weight = 1.0 - metallic * 0.7;

    var color = albedo * scene.ambient_and_count.rgb;
    let count = u32(scene.ambient_and_count.w);

    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let light = scene.lights[i];
        var l_dir: vec3<f32>;
        var atten: f32 = 1.0;
        if (light.pos_or_dir.w > 0.5) {
            let delta = light.pos_or_dir.xyz - in.world_pos;
            let dist = max(length(delta), 0.001);
            l_dir = delta / dist;
            atten = 1.0 / (1.0 + 0.09 * dist + 0.032 * dist * dist);
        } else {
            l_dir = normalize(-light.pos_or_dir.xyz);
        }
        let ndotl = max(dot(n, l_dir), 0.0);
        if (ndotl > 0.0) {
            let h = normalize(l_dir + view_dir);
            let spec = pow(max(dot(n, h), 0.0), shininess) * (1.0 - roughness * 0.6);
            let radiance = light.color_intensity.rgb * light.color_intensity.w * atten;
            color = color + (albedo * diff_weight * ndotl + spec_color * spec) * radiance;
        }
    }

    // Gentle Reinhard tone mapping (target texture is sRGB so hardware applies gamma).
    let mapped = color / (color + vec3<f32>(1.0, 1.0, 1.0));
    return vec4<f32>(mapped, 1.0);
}
