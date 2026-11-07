// Instanced rounded-box quads: one instance per QuadInstance (scene.rs),
// expanded to cover its drop shadow; the fragment evaluates the same signed
// distance as cpu.rs (sd_rounded_box, shade), in device pixels, so edges
// ramp over one pixel at any scale.

struct Globals {
    size: vec2<f32>,  // target size, pixels
    scale: f32,       // pixels per point
    _pad: f32,
};

@group(0) @binding(0) var<uniform> g: Globals;

struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) bg: vec4<f32>,
    @location(2) border_color: vec4<f32>,
    @location(3) radii: vec4<f32>,
    @location(4) border_width: f32,
    @location(5) shadow_offset: vec2<f32>,
    @location(6) shadow_params: vec2<f32>,
    @location(7) shadow_color: vec4<f32>,
    @location(8) clip: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) rect: vec4<f32>,
    @location(1) @interpolate(flat) bg: vec4<f32>,
    @location(2) @interpolate(flat) border_color: vec4<f32>,
    @location(3) @interpolate(flat) radii: vec4<f32>,
    @location(4) @interpolate(flat) border_width: f32,
    @location(5) @interpolate(flat) shadow_offset: vec2<f32>,
    @location(6) @interpolate(flat) shadow_params: vec2<f32>,
    @location(7) @interpolate(flat) shadow_color: vec4<f32>,
    @location(8) @interpolate(flat) clip: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    let s = g.scale;
    let rect = inst.rect * s;
    // The quad grown by the shadow's reach (QuadInstance::extent).
    var lo = rect.xy;
    var hi = rect.xy + rect.zw;
    if (inst.shadow_color.a > 0.0) {
        let m = (inst.shadow_params.x * 1.5 + max(inst.shadow_params.y, 0.0)) * s;
        let off = inst.shadow_offset * s;
        lo = min(lo, lo + off - vec2<f32>(m));
        hi = max(hi, hi + off + vec2<f32>(m));
    }
    // One pixel more so the anti-aliased edge is never cut.
    lo = floor(lo) - vec2<f32>(1.0);
    hi = ceil(hi) + vec2<f32>(1.0);
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let p = mix(lo, hi, corners[vi]);
    var out: VsOut;
    out.pos = vec4<f32>(p.x / g.size.x * 2.0 - 1.0, 1.0 - p.y / g.size.y * 2.0, 0.0, 1.0);
    out.rect = rect;
    out.bg = inst.bg;
    out.border_color = inst.border_color;
    out.radii = inst.radii * s;
    out.border_width = inst.border_width * s;
    out.shadow_offset = inst.shadow_offset * s;
    out.shadow_params = inst.shadow_params * s;
    out.shadow_color = inst.shadow_color;
    out.clip = inst.clip * s;
    return out;
}

fn sd_rounded_box(p: vec2<f32>, b: vec2<f32>, radii: vec4<f32>) -> f32 {
    var r: f32;
    if (p.x >= 0.0) {
        r = select(radii.y, radii.z, p.y >= 0.0);
    } else {
        r = select(radii.x, radii.w, p.y >= 0.0);
    }
    r = max(min(r, min(b.x, b.y)), 0.0);
    let q = abs(p) - b + vec2<f32>(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - r;
}

fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy; // pixel centre
    let c = in.clip;
    if (p.x < c.x || p.y < c.y || p.x > c.x + c.z || p.y > c.y + c.w) {
        discard;
    }
    let half = in.rect.zw * 0.5;
    let local = p - (in.rect.xy + half);
    let d = sd_rounded_box(local, half, in.radii);
    let outer = coverage(d);
    var inner = outer;
    if (in.border_width > 0.0) {
        inner = coverage(d + in.border_width);
    }
    // Premultiplied fill and border.
    var rgb = in.bg.rgb * in.bg.a * inner + in.border_color.rgb * in.border_color.a * (outer - inner);
    var a = in.bg.a * inner + in.border_color.a * (outer - inner);
    if (in.shadow_color.a > 0.0) {
        let blur = max(in.shadow_params.x, 0.5);
        let spread = in.shadow_params.y;
        let ds = sd_rounded_box(local - in.shadow_offset, half + vec2<f32>(spread), in.radii + vec4<f32>(max(spread, 0.0)));
        let sa = in.shadow_color.a * (1.0 - smoothstep(-blur, blur, ds)) * (1.0 - outer);
        rgb = rgb + in.shadow_color.rgb * sa * (1.0 - a);
        a = a + sa * (1.0 - a);
    }
    if (a <= 0.0) {
        discard;
    }
    return vec4<f32>(rgb, a);
}
