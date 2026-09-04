#version 450

layout(set = 0, binding = 0) uniform sampler2D surface_texture;
layout(location = 0) in vec2 texture_position;
layout(location = 0) out vec4 color;

layout(push_constant) uniform Push {
    vec4 source;
    vec2 active_uv;
    float opacity;
    uint transform;
} push;

void main() {
    // Captured game surfaces are premultiplied (and normally opaque), so
    // scaling the whole sample preserves the blend invariant.
    color = texture(surface_texture, texture_position) * push.opacity;
}
