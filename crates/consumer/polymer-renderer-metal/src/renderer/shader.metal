#include <metal_stdlib>
using namespace metal;
struct Vertex { packed_float2 position; packed_float2 uv; packed_float4 color; packed_float2 local; packed_float4 clip; };
struct Out { float4 position [[position]]; float2 uv; float4 color; float2 local; float4 clip; };
vertex Out vertex_main(uint id [[vertex_id]], constant Vertex *v [[buffer(0)]], constant float2 &extent [[buffer(1)]]) {
    Vertex p = v[id]; Out o;
    o.position = float4(float2(p.position) / extent * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = p.uv; o.color = p.color; o.local = p.local; o.clip = p.clip; return o;
}
fragment float4 fragment_main(Out v [[stage_in]], texture2d<float> image [[texture(0)]]) {
    constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::linear);
    float coverage = 1;
    if (v.clip.z > 0) {
        float2 q = abs(v.local - v.clip.xy * 0.5) - (v.clip.xy * 0.5 - v.clip.z);
        float distance = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - v.clip.z;
        coverage = clamp(0.5 - distance, 0.0, 1.0);
    }
    return image.sample(s, v.uv) * v.color * coverage;
}
