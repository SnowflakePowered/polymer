struct Uniforms {
    view_projection: mat4x4<f32>,
    light_directions: array<vec4<f32>, 3>,
    light_colors: array<vec4<f32>, 3>,
    ambient: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var icon_texture: texture_2d<f32>;
@group(0) @binding(2) var icon_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
}

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
) -> VertexOutput {
    var output: VertexOutput;
    output.position = uniforms.view_projection * vec4<f32>(position, 1.0);
    output.normal = normal;
    output.uv = uv;
    output.color = color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(icon_texture, icon_sampler, input.uv) * input.color;
    let normal = normalize(input.normal);
    var light = uniforms.ambient.rgb;
    for (var index = 0u; index < 3u; index++) {
        let lambert = max(dot(uniforms.light_directions[index].xyz, normal), 0.0);
        light += lambert * uniforms.light_colors[index].rgb;
    }
    return vec4<f32>(min(base.rgb * light, vec3<f32>(1.0)) * base.a, base.a);
}
