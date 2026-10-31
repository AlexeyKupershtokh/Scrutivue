struct Params { dims: vec4<u32> } // width, height, x radius, y radius
@group(0) @binding(0) var<storage, read> image: array<u32>;
@group(0) @binding(1) var<storage, read_write> temporary: array<f32>;
@group(0) @binding(2) var<storage, read_write> filtered: array<f32>;
@group(0) @binding(3) var<storage, read> wx: array<f32>;
@group(0) @binding(4) var<storage, read> wy: array<f32>;
@group(0) @binding(5) var<uniform> p: Params;
fn original(x: i32, y: i32) -> vec3<f32> {
    let i = (u32(clamp(y, 0, i32(p.dims.y) - 1)) * p.dims.x + u32(clamp(x, 0, i32(p.dims.x) - 1))) * 3u;
    return vec3(f32((image[i / 2u] >> ((i % 2u) * 16u)) & 65535u),
        f32((image[(i + 1u) / 2u] >> (((i + 1u) % 2u) * 16u)) & 65535u),
        f32((image[(i + 2u) / 2u] >> (((i + 2u) % 2u) * 16u)) & 65535u)) / 65535.0;
}
@compute @workgroup_size(16, 16)
fn horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.dims.x || id.y >= p.dims.y { return; }
    var total = vec3(0.0);
    for (var k = -i32(p.dims.z); k <= i32(p.dims.z); k++) {
        total += original(i32(id.x) + k, i32(id.y)) * wx[u32(k + i32(p.dims.z))];
    }
    let i = (id.y * p.dims.x + id.x) * 3u;
    temporary[i] = total.x; temporary[i + 1u] = total.y; temporary[i + 2u] = total.z;
}
@compute @workgroup_size(16, 16)
fn vertical(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.dims.x || id.y >= p.dims.y { return; }
    var total = vec3(0.0);
    for (var k = -i32(p.dims.w); k <= i32(p.dims.w); k++) {
        let i = (u32(clamp(i32(id.y) + k, 0, i32(p.dims.y) - 1)) * p.dims.x + id.x) * 3u;
        total += vec3(temporary[i], temporary[i + 1u], temporary[i + 2u]) * wy[u32(k + i32(p.dims.w))];
    }
    let result = original(i32(id.x), i32(id.y)) - total;
    let i = (id.y * p.dims.x + id.x) * 3u;
    filtered[i] = result.x; filtered[i + 1u] = result.y; filtered[i + 2u] = result.z;
}
