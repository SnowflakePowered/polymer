// GX TEV emulation for Wii Menu channel layouts. Each draw is one pane quad
// whose material configures up to sixteen combiner stages.

struct Stage {
    // Color inputs a, b, c, d.
    color: vec4<u32>,
    // Alpha inputs a, b, c, d.
    alpha: vec4<u32>,
    // x: texture coordinate | raster channel << 8 | raster swap << 16 | texture swap << 24
    // y: texture map (0xFF for none)
    // z, w: color and alpha operations, packed by `Stage::operation`
    config: vec4<u32>,
    // x: indirect stage | format << 4 | bias << 8 | matrix << 12 | wrap s << 16
    //    | wrap t << 20 | add previous << 24
    indirect: vec4<u32>,
}

struct Material {
    // PREV, REG0, REG1, REG2.
    registers: array<vec4<f32>, 4>,
    constants: array<vec4<f32>, 4>,
    swap_tables: array<vec4<u32>, 4>,
    // x: stage count, y: alpha compare functions and operation, z: alpha
    // compare references, w: whether alpha compare applies.
    info: vec4<u32>,
    // Per indirect stage: texture coordinate | texture map << 8 | scale s << 16 | scale t << 24.
    indirect_orders: vec4<u32>,
    // Three 2x3 indirect matrices as row pairs.
    indirect_matrices: array<vec4<f32>, 6>,
    stages: array<Stage, 16>,
}

@group(0) @binding(0) var<uniform> material: Material;
@group(1) @binding(0) var texture0: texture_2d<f32>;
@group(1) @binding(1) var texture1: texture_2d<f32>;
@group(1) @binding(2) var texture2: texture_2d<f32>;
@group(1) @binding(3) var texture3: texture_2d<f32>;
@group(1) @binding(4) var texture4: texture_2d<f32>;
@group(1) @binding(5) var texture5: texture_2d<f32>;
@group(1) @binding(6) var texture6: texture_2d<f32>;
@group(1) @binding(7) var texture7: texture_2d<f32>;
@group(1) @binding(8) var sampler0: sampler;
@group(1) @binding(9) var sampler1: sampler;
@group(1) @binding(10) var sampler2: sampler;
@group(1) @binding(11) var sampler3: sampler;
@group(1) @binding(12) var sampler4: sampler;
@group(1) @binding(13) var sampler5: sampler;
@group(1) @binding(14) var sampler6: sampler;
@group(1) @binding(15) var sampler7: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv01: vec4<f32>,
    @location(2) uv23: vec4<f32>,
    @location(3) uv45: vec4<f32>,
    @location(4) uv67: vec4<f32>,
}

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv01: vec4<f32>,
    @location(3) uv23: vec4<f32>,
    @location(4) uv45: vec4<f32>,
    @location(5) uv67: vec4<f32>,
) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.color = color;
    output.uv01 = uv01;
    output.uv23 = uv23;
    output.uv45 = uv45;
    output.uv67 = uv67;
    return output;
}

fn coordinate(input: VertexOutput, index: u32) -> vec2<f32> {
    switch index {
        case 0u: { return input.uv01.xy; }
        case 1u: { return input.uv01.zw; }
        case 2u: { return input.uv23.xy; }
        case 3u: { return input.uv23.zw; }
        case 4u: { return input.uv45.xy; }
        case 5u: { return input.uv45.zw; }
        case 6u: { return input.uv67.xy; }
        default: { return input.uv67.zw; }
    }
}

fn sample(map: u32, uv: vec2<f32>) -> vec4<f32> {
    switch map {
        case 0u: { return textureSampleLevel(texture0, sampler0, uv, 0.0); }
        case 1u: { return textureSampleLevel(texture1, sampler1, uv, 0.0); }
        case 2u: { return textureSampleLevel(texture2, sampler2, uv, 0.0); }
        case 3u: { return textureSampleLevel(texture3, sampler3, uv, 0.0); }
        case 4u: { return textureSampleLevel(texture4, sampler4, uv, 0.0); }
        case 5u: { return textureSampleLevel(texture5, sampler5, uv, 0.0); }
        case 6u: { return textureSampleLevel(texture6, sampler6, uv, 0.0); }
        case 7u: { return textureSampleLevel(texture7, sampler7, uv, 0.0); }
        default: { return vec4<f32>(0.0); }
    }
}

fn dimensions(map: u32) -> vec2<f32> {
    switch map {
        case 0u: { return vec2<f32>(textureDimensions(texture0)); }
        case 1u: { return vec2<f32>(textureDimensions(texture1)); }
        case 2u: { return vec2<f32>(textureDimensions(texture2)); }
        case 3u: { return vec2<f32>(textureDimensions(texture3)); }
        case 4u: { return vec2<f32>(textureDimensions(texture4)); }
        case 5u: { return vec2<f32>(textureDimensions(texture5)); }
        case 6u: { return vec2<f32>(textureDimensions(texture6)); }
        default: { return vec2<f32>(textureDimensions(texture7)); }
    }
}

// GX_ITW: off, wrap at 256, 128, 64, 32 or 16 texels, or zero.
fn indirect_wrap(value: f32, mode: u32) -> f32 {
    if mode == 0u {
        return value;
    }
    if mode >= 6u {
        return 0.0;
    }
    let period = f32(256u >> (mode - 1u));
    return value - floor(value / period) * period;
}

// The texel offset an indirect stage adds: the indirect texture's alpha, blue
// and green components as (s, t, u), biased and multiplied by a 2x3 matrix.
fn indirect_offset(input: VertexOutput, indirect: u32) -> vec2<f32> {
    let matrix = (indirect >> 12u) & 0xFu;
    if matrix == 0u || matrix > 3u {
        return vec2<f32>(0.0);
    }
    let order = (material.indirect_orders[indirect & 3u]);
    let scale = vec2<f32>(1.0 / f32(1u << ((order >> 16u) & 0xFu)), 1.0 / f32(1u << ((order >> 24u) & 0xFu)));
    let components = vec3<u32>(round(sample((order >> 8u) & 0xFFu, coordinate(input, order & 0xFFu) * scale).abg * 255.0));
    let format = (indirect >> 4u) & 3u;
    let mask = 0xFFu >> select(0u, format + 2u, format != 0u);
    var stu = vec3<f32>(components & vec3<u32>(mask));
    let bias = (indirect >> 8u) & 7u;
    let offset = select(1.0, -128.0, format == 0u);
    stu += vec3<f32>(select(0.0, offset, (bias & 1u) != 0u), select(0.0, offset, (bias & 2u) != 0u), select(0.0, offset, (bias & 4u) != 0u));
    let row = (matrix - 1u) * 2u;
    return vec2<f32>(dot(material.indirect_matrices[row].xyz, stu), dot(material.indirect_matrices[row + 1u].xyz, stu));
}

fn swap(value: vec4<f32>, table: u32) -> vec4<f32> {
    let selectors = material.swap_tables[table & 3u];
    return vec4<f32>(value[selectors.x & 3u], value[selectors.y & 3u], value[selectors.z & 3u], value[selectors.w & 3u]);
}

// GX_TEV_KCSEL / GX_TEV_KASEL: fixed fractions, whole constants, or one component.
fn constant_component(selection: u32) -> f32 {
    if selection < 8u {
        return f32(8u - selection) / 8.0;
    }
    let component = (selection - 16u) / 4u;
    return material.constants[(selection - 16u) % 4u][component];
}

fn constant_color(selection: u32) -> vec3<f32> {
    if selection >= 12u && selection <= 15u {
        return material.constants[selection - 12u].rgb;
    }
    return vec3<f32>(constant_component(selection));
}

fn color_input(selection: u32, registers: array<vec4<f32>, 4>, texel: vec4<f32>, raster: vec4<f32>, constant: vec3<f32>) -> vec3<f32> {
    switch selection {
        case 0u: { return registers[0].rgb; }
        case 1u: { return vec3<f32>(registers[0].a); }
        case 2u: { return registers[1].rgb; }
        case 3u: { return vec3<f32>(registers[1].a); }
        case 4u: { return registers[2].rgb; }
        case 5u: { return vec3<f32>(registers[2].a); }
        case 6u: { return registers[3].rgb; }
        case 7u: { return vec3<f32>(registers[3].a); }
        case 8u: { return texel.rgb; }
        case 9u: { return vec3<f32>(texel.a); }
        case 10u: { return raster.rgb; }
        case 11u: { return vec3<f32>(raster.a); }
        case 12u: { return vec3<f32>(1.0); }
        case 13u: { return vec3<f32>(0.5); }
        case 14u: { return constant; }
        default: { return vec3<f32>(0.0); }
    }
}

fn alpha_input(selection: u32, registers: array<vec4<f32>, 4>, texel: vec4<f32>, raster: vec4<f32>, constant: f32) -> f32 {
    switch selection {
        case 0u: { return registers[0].a; }
        case 1u: { return registers[1].a; }
        case 2u: { return registers[2].a; }
        case 3u: { return registers[3].a; }
        case 4u: { return texel.a; }
        case 5u: { return raster.a; }
        case 6u: { return constant; }
        default: { return 0.0; }
    }
}

fn bias(operation: u32) -> f32 {
    switch (operation >> 4u) & 3u {
        case 1u: { return 0.5; }
        case 2u: { return -0.5; }
        default: { return 0.0; }
    }
}

fn scale(operation: u32) -> f32 {
    switch (operation >> 6u) & 3u {
        case 1u: { return 2.0; }
        case 2u: { return 4.0; }
        case 3u: { return 0.5; }
        default: { return 1.0; }
    }
}

fn byte(value: f32) -> u32 {
    return u32(round(clamp(value, 0.0, 1.0) * 255.0));
}

// Compare operations 8..15 pass `c` where `a` compares greater or equal to `b`.
fn color_compare(operation: u32, a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    let equal = (operation & 1u) == 1u;
    let packed_a = vec3<u32>(byte(a.r), byte(a.g), byte(a.b));
    let packed_b = vec3<u32>(byte(b.r), byte(b.g), byte(b.b));
    var left = packed_a.r;
    var right = packed_b.r;
    switch (operation - 8u) >> 1u {
        case 1u: {
            left = packed_a.r | (packed_a.g << 8u);
            right = packed_b.r | (packed_b.g << 8u);
        }
        case 2u: {
            left = packed_a.r | (packed_a.g << 8u) | (packed_a.b << 16u);
            right = packed_b.r | (packed_b.g << 8u) | (packed_b.b << 16u);
        }
        case 3u: {
            let passed = select(packed_a > packed_b, packed_a == packed_b, equal);
            return select(vec3<f32>(0.0), vec3<f32>(1.0), passed);
        }
        default: {}
    }
    let passed = select(left > right, left == right, equal);
    return vec3<f32>(select(0.0, 1.0, passed));
}

fn alpha_compare(function: u32, value: u32, reference: u32) -> bool {
    switch function {
        case 0u: { return false; }
        case 1u: { return value < reference; }
        case 2u: { return value == reference; }
        case 3u: { return value <= reference; }
        case 4u: { return value > reference; }
        case 5u: { return value != reference; }
        case 6u: { return value >= reference; }
        default: { return true; }
    }
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var registers = material.registers;
    var color_register = 0u;
    var alpha_register = 0u;
    var previous_offset = vec2<f32>(0.0);
    for (var index = 0u; index < material.info.x; index++) {
        let stage = material.stages[index];
        let config = stage.config.x;
        let texture_map = stage.config.y;
        let indirect = stage.indirect.x;
        var offset = indirect_offset(input, indirect);
        if ((indirect >> 24u) & 1u) == 1u {
            offset += previous_offset;
        }
        previous_offset = offset;
        var texel = vec4<f32>(0.0);
        if texture_map != 0xFFu {
            let size = dimensions(texture_map);
            let uv = coordinate(input, config & 0xFFu) * size;
            let wrapped = vec2<f32>(indirect_wrap(uv.x, (indirect >> 16u) & 0xFu), indirect_wrap(uv.y, (indirect >> 20u) & 0xFu));
            texel = swap(sample(texture_map, (wrapped + offset) / size), config >> 24u);
        }
        let channel = (config >> 8u) & 0xFFu;
        var raster = vec4<f32>(0.0);
        if channel != 0xFFu && channel != 6u {
            raster = swap(input.color, (config >> 16u) & 0xFFu);
        }

        let color_operation = stage.config.z;
        let color_constant = constant_color((color_operation >> 11u) & 0x1Fu);
        let ca = color_input(stage.color.x, registers, texel, raster, color_constant);
        let cb = color_input(stage.color.y, registers, texel, raster, color_constant);
        let cc = color_input(stage.color.z, registers, texel, raster, color_constant);
        let cd = color_input(stage.color.w, registers, texel, raster, color_constant);
        let color_op = color_operation & 0xFu;
        var color: vec3<f32>;
        if color_op <= 1u {
            let blend = mix(ca, cb, cc);
            color = (select(cd + blend, cd - blend, color_op == 1u) + bias(color_operation)) * scale(color_operation);
        } else {
            color = cd + color_compare(color_op, ca, cb) * cc;
        }
        if ((color_operation >> 8u) & 1u) == 1u {
            color = clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
        } else {
            color = clamp(color, vec3<f32>(-4.0), vec3<f32>(4.0));
        }

        let alpha_operation = stage.config.w;
        let alpha_constant = constant_component((alpha_operation >> 11u) & 0x1Fu);
        let aa = alpha_input(stage.alpha.x, registers, texel, raster, alpha_constant);
        let ab = alpha_input(stage.alpha.y, registers, texel, raster, alpha_constant);
        let ac = alpha_input(stage.alpha.z, registers, texel, raster, alpha_constant);
        let ad = alpha_input(stage.alpha.w, registers, texel, raster, alpha_constant);
        let alpha_op = alpha_operation & 0xFu;
        var alpha: f32;
        if alpha_op <= 1u {
            let blend = mix(aa, ab, ac);
            alpha = (select(ad + blend, ad - blend, alpha_op == 1u) + bias(alpha_operation)) * scale(alpha_operation);
        } else {
            let passed = select(byte(aa) > byte(ab), byte(aa) == byte(ab), (alpha_op & 1u) == 1u);
            alpha = ad + select(0.0, ac, passed);
        }
        if ((alpha_operation >> 8u) & 1u) == 1u {
            alpha = clamp(alpha, 0.0, 1.0);
        } else {
            alpha = clamp(alpha, -4.0, 4.0);
        }

        color_register = (color_operation >> 9u) & 3u;
        alpha_register = (alpha_operation >> 9u) & 3u;
        registers[color_register] = vec4<f32>(color, registers[color_register].a);
        registers[alpha_register].a = alpha;
    }
    let output = clamp(vec4<f32>(registers[color_register].rgb, registers[alpha_register].a), vec4<f32>(0.0), vec4<f32>(1.0));

    if material.info.w == 1u {
        let value = byte(output.a);
        let first = alpha_compare(material.info.y & 0xFFu, value, material.info.z & 0xFFu);
        let second = alpha_compare((material.info.y >> 8u) & 0xFFu, value, (material.info.z >> 8u) & 0xFFu);
        var passed: bool;
        switch (material.info.y >> 16u) & 3u {
            case 0u: { passed = first && second; }
            case 1u: { passed = first || second; }
            case 2u: { passed = first != second; }
            default: { passed = first == second; }
        }
        if !passed {
            discard;
        }
    }
    return output;
}
