@group(0) @binding(0) var over_black: texture_2d<f32>;
@group(0) @binding(1) var over_white: texture_2d<f32>;
@group(0) @binding(2) var filtering: sampler;

struct Fragment {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Fragment {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return Fragment(vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0), uv);
}

@fragment
fn fs_main(fragment: Fragment) -> @location(0) vec4<f32> {
    let black = textureSample(over_black, filtering, fragment.uv).rgb;
    let white = textureSample(over_white, filtering, fragment.uv).rgb;
    let alpha = clamp(1.0 - dot(white - black, vec3<f32>(1.0 / 3.0)), 0.0, 1.0);
    return vec4<f32>(min(black, vec3<f32>(alpha)), alpha);
}
