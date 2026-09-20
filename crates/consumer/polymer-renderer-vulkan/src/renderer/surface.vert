#version 450

layout(location = 0) out vec2 texture_position;

layout(push_constant) uniform Push {
    // Normalized crop in post-presentation-transform coordinates.
    vec4 source;
    // Valid top-left portion of the imported allocation.
    vec2 active_uv;
    float opacity;
    // Low byte: counterclockwise quarter turns. Bit 8: vertical flip.
    uint transform;
} push;

const vec2 positions[6] = vec2[](
    vec2(-1.0, -1.0),
    vec2( 1.0, -1.0),
    vec2( 1.0,  1.0),
    vec2(-1.0, -1.0),
    vec2( 1.0,  1.0),
    vec2(-1.0,  1.0)
);

const vec2 corners[6] = vec2[](
    vec2(0.0, 0.0),
    vec2(1.0, 0.0),
    vec2(1.0, 1.0),
    vec2(0.0, 0.0),
    vec2(1.0, 1.0),
    vec2(0.0, 1.0)
);

void main() {
    vec2 corner = corners[gl_VertexIndex];
    vec2 oriented = mix(push.source.xy, push.source.zw, corner);
    float top = (push.transform & 0x100u) != 0u ? push.active_uv.y : 0.0;
    float bottom = (push.transform & 0x100u) != 0u ? 0.0 : push.active_uv.y;
    vec2 raw[4] = vec2[](
        vec2(0.0, top),
        vec2(push.active_uv.x, top),
        vec2(push.active_uv.x, bottom),
        vec2(0.0, bottom)
    );
    uint turns = push.transform & 3u;
    vec2 upper_left = raw[turns];
    vec2 upper_right = raw[(turns + 1u) & 3u];
    vec2 lower_right = raw[(turns + 2u) & 3u];
    vec2 lower_left = raw[(turns + 3u) & 3u];
    texture_position = mix(
        mix(upper_left, upper_right, oriented.x),
        mix(lower_left, lower_right, oriented.x),
        oriented.y
    );
    gl_Position = vec4(positions[gl_VertexIndex], 0.0, 1.0);
}
