struct Params {
    dims: vec4<u32>, // A width/height, B width/height
    view: vec4<u32>, // viewport width/height, mode, highpass
    camera: vec4<f32>, // scale, center x/y, divider
    options: vec4<f32>, // gain, show B
}
@group(0) @binding(0) var<storage, read> a: array<u32>;
@group(0) @binding(1) var<storage, read> b: array<u32>;
@group(0) @binding(2) var<storage, read> fa: array<f32>;
@group(0) @binding(3) var<storage, read> fb: array<f32>;
@group(0) @binding(4) var<storage, read_write> output: array<u32>;
@group(0) @binding(5) var<uniform> p: Params;

fn read_a(x: i32, y: i32) -> vec3<f32> {
    let i = (u32(clamp(y, 0, i32(p.dims.y) - 1)) * p.dims.x + u32(clamp(x, 0, i32(p.dims.x) - 1))) * 3u;
    if p.view.w != 0u { return vec3(fa[i], fa[i + 1u], fa[i + 2u]); }
    return vec3(f32((a[i / 2u] >> ((i % 2u) * 16u)) & 65535u),
        f32((a[(i + 1u) / 2u] >> (((i + 1u) % 2u) * 16u)) & 65535u),
        f32((a[(i + 2u) / 2u] >> (((i + 2u) % 2u) * 16u)) & 65535u)) / 65535.0;
}
fn read_b(x: i32, y: i32) -> vec3<f32> {
    let i = (u32(clamp(y, 0, i32(p.dims.w) - 1)) * p.dims.z + u32(clamp(x, 0, i32(p.dims.z) - 1))) * 3u;
    if p.view.w != 0u { return vec3(fb[i], fb[i + 1u], fb[i + 2u]); }
    return vec3(f32((b[i / 2u] >> ((i % 2u) * 16u)) & 65535u),
        f32((b[(i + 1u) / 2u] >> (((i + 1u) % 2u) * 16u)) & 65535u),
        f32((b[(i + 2u) / 2u] >> (((i + 2u) % 2u) * 16u)) & 65535u)) / 65535.0;
}
fn sample_a(pos: vec2<f32>) -> vec3<f32> {
    let base = vec2<i32>(floor(pos)); let f = fract(pos);
    return mix(mix(read_a(base.x, base.y), read_a(base.x + 1, base.y), f.x),
        mix(read_a(base.x, base.y + 1), read_a(base.x + 1, base.y + 1), f.x), f.y);
}
fn sample_b(pos: vec2<f32>) -> vec3<f32> {
    let base = vec2<i32>(floor(pos)); let f = fract(pos);
    return mix(mix(read_b(base.x, base.y), read_b(base.x + 1, base.y), f.x),
        mix(read_b(base.x, base.y + 1), read_b(base.x + 1, base.y + 1), f.x), f.y);
}
@compute @workgroup_size(16, 16)
fn render(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.view.x || id.y >= p.view.y { return; }
    let pos = (vec2<f32>(id.xy) + 0.5 - vec2<f32>(p.view.xy) * 0.5) / p.camera.x +
        p.camera.yz * vec2<f32>(p.dims.xy) - 0.5;
    var color: vec3<f32>;
    if any(pos < vec2(-0.5)) || any(pos >= vec2<f32>(p.dims.xy) - 0.5) {
        color = vec3(select(0.085, 0.075, ((id.x / 16u + id.y / 16u) & 1u) != 0u));
    } else {
        let bpos = (pos + 0.5) * vec2<f32>(p.dims.zw) / vec2<f32>(p.dims.xy) - 0.5;
        if p.view.z == 1u { color = abs(sample_a(pos) - sample_b(bpos)) * p.options.x; }
        else {
            let second = select(p.options.y > 0.5, f32(id.x) >= p.camera.w * f32(p.view.x), p.view.z == 2u);
            if second { color = sample_b(bpos); } else { color = sample_a(pos); }
            if p.view.w != 0u { color += vec3(0.5); }
        }
    }
    // GPUI consumes BGRA; avoid a CPU channel-swizzle pass over the viewport.
    output[id.y * p.view.x + id.x] = pack4x8unorm(vec4(clamp(color.bgr, vec3(0.0), vec3(1.0)), 1.0));
}
