@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var sampling: sampler;

struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Vertex {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return Vertex(vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0), uv);
}

@fragment
fn premultiply(vertex: Vertex) -> @location(0) vec4<f32> {
    let color = textureLoad(source, vec2<i32>(vertex.position.xy), 0);
    return vec4<f32>(color.rgb * color.a, color.a);
}

@fragment
fn reduce(vertex: Vertex) -> @location(0) vec4<f32> {
    let limit = vec2<i32>(textureDimensions(source)) - 1;
    let origin = vec2<i32>(vertex.position.xy) * 2;
    return (textureLoad(source, min(origin, limit), 0)
        + textureLoad(source, min(origin + vec2<i32>(1, 0), limit), 0)
        + textureLoad(source, min(origin + vec2<i32>(0, 1), limit), 0)
        + textureLoad(source, min(origin + vec2<i32>(1, 1), limit), 0)) * 0.25;
}

@fragment
fn resolve(vertex: Vertex) -> @location(0) vec4<f32> {
    return textureSample(source, sampling, vertex.uv);
}
