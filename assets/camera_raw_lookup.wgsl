// Compositor 1.5.3 Camera Raw lookup; original MIT attribution in LICENSES.md.
struct Parameters { count: u32, row_width: u32, unused0: u32, unused1: u32 }
@group(0) @binding(0) var<storage, read> source: array<u32>;
@group(0) @binding(1) var<storage, read> table: array<f32>;
@group(0) @binding(2) var<storage, read_write> result: array<u32>;
@group(0) @binding(3) var<uniform> parameters: Parameters;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x + id.y * parameters.row_width;
    if index >= parameters.count { return; }
    let pixel = source[index];
    let alpha = pixel >> 24u;
    if alpha == 0u { result[index] = pixel & 0x00ffffffu; return; }
    let rgb = vec3<u32>(pixel & 255u, (pixel >> 8u) & 255u, (pixel >> 16u) & 255u);
    let numerator = min(rgb, vec3<u32>(alpha)) * 32u;
    let cell = min(numerator / alpha, vec3<u32>(31u));
    let f = vec3<f32>(numerator - cell * alpha) / f32(alpha);
    var color = vec3<f32>(0.0);
    for (var corner = 0u; corner < 8u; corner += 1u) {
        let delta = vec3<u32>(corner >> 2u, (corner >> 1u) & 1u, corner & 1u);
        let weights = select(vec3<f32>(1.0) - f, f, delta != vec3<u32>(0u));
        let weight = weights.x * weights.y * weights.z;
        if weight == 0.0 { continue; }
        let node = cell + delta;
        let at = ((node.x * 33u + node.y) * 33u + node.z) * 3u;
        color += weight * vec3<f32>(table[at], table[at + 1u], table[at + 2u]);
    }
    let scaled = clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)) * f32(alpha);
    // Resolve rounding boundaries with the same double precision lookup as CPU.
    let repair = any(abs(scaled - (floor(scaled) + vec3<f32>(0.5))) <= vec3<f32>(0.01));
    let out = vec3<u32>(clamp(floor(scaled + vec3<f32>(0.5)), vec3<f32>(0.0), vec3<f32>(f32(alpha))));
    result[index] = out.x | (out.y << 8u) | (out.z << 16u) | select(0u, 0x01000000u, repair);
}
