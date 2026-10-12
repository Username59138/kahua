struct Globals {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    sun_dir: vec4<f32>,
    sky: vec4<f32>,
    ground: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

struct PartIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) m0: vec4<f32>,
    @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>,
    @location(5) m3: vec4<f32>,
    @location(6) n0: vec4<f32>,
    @location(7) n1: vec4<f32>,
    @location(8) n2: vec4<f32>,
    @location(9) color: vec4<f32>,
};

struct PartOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_part(v: PartIn) -> PartOut {
    let model = mat4x4<f32>(v.m0, v.m1, v.m2, v.m3);
    let normal_matrix = mat3x3<f32>(v.n0.xyz, v.n1.xyz, v.n2.xyz);
    let world = model * vec4<f32>(v.position, 1.0);
    var out: PartOut;
    out.clip = g.view_proj * world;
    out.world = world.xyz;
    out.normal = normalize(normal_matrix * v.normal);
    out.color = v.color;
    return out;
}

@fragment
fn fs_part(v: PartOut) -> @location(0) vec4<f32> {
    let n = normalize(v.normal);
    let sun = normalize(g.sun_dir.xyz);
    let hemi = mix(g.ground.rgb, g.sky.rgb, n.y * 0.5 + 0.5);
    let diffuse = max(dot(n, sun), 0.0);
    let view_dir = normalize(g.camera_pos.xyz - v.world);
    let half_dir = normalize(view_dir + sun);
    let spec = pow(max(dot(n, half_dir), 0.0), 48.0) * 0.12;
    var lit = v.color.rgb * (hemi * 0.55 + vec3<f32>(1.0, 0.97, 0.92) * diffuse * 0.65) + spec;
    // Distance fog toward the sky color.
    let dist = length(g.camera_pos.xyz - v.world);
    let fog = clamp((dist - 600.0) / 3000.0, 0.0, 0.85);
    lit = mix(lit, g.sky.rgb, fog);
    return vec4<f32>(lit, v.color.a);
}

struct LineIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(v: LineIn) -> LineOut {
    var out: LineOut;
    out.clip = g.view_proj * vec4<f32>(v.position, 1.0);
    out.color = v.color;
    return out;
}

@fragment
fn fs_line(v: LineOut) -> @location(0) vec4<f32> {
    return v.color;
}

// Full-screen sky gradient.
struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_sky(@builtin(vertex_index) i: u32) -> SkyOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: SkyOut;
    out.clip = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = p;
    return out;
}

@fragment
fn fs_sky(v: SkyOut) -> @location(0) vec4<f32> {
    let zenith = g.sky.rgb * vec3<f32>(0.42, 0.62, 0.92);
    return vec4<f32>(mix(g.sky.rgb, zenith, clamp(v.uv.y, 0.0, 1.0)), 1.0);
}
