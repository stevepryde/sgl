// Accumulates Godot's traced reflections over frames before its roughness
// filter. Godot's trace is deterministic, but TAA's jitter moves its depth,
// normal and radiance inputs every frame, so a thin bright hit appears at
// different pixels and the Gaussian mips spread each into a blob larger than
// TAA's neighbourhood. Wicked Engine's (revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT, see LICENSE-wicked.txt)
// shaders/ssr_temporalCS.hlsl, through temporal_reprojection.wgsl, as world
// rays accumulate. Modified: at Godot's traced size, on its tone-mapped
// premultiplied colour and confidence; no variance output; receiver depth is
// Godot's hi-z mip 0 and the pass keeps its own depth history; pixels Godot
// does not trace (sky, roughness >= 0.7) pass through.
struct TemporalParams {
	inverse_view_projection: mat4x4<f32>,
	previous_view_projection: mat4x4<f32>,
	// Traced width, height, 1/width, 1/height.
	size: vec4<f32>,
	// The reversed-Z infinite projection's near plane, this frame's and the
	// previous frame's.
	near: f32,
	previous_near: f32,
	// TEMPORAL_CONTINUES while history continues; clear, it resets.
	flags: u32,
	padding: u32,
}
const TEMPORAL_CONTINUES: u32 = 1u;
@group(0) @binding(0) var temporal_current: texture_2d<f32>;
@group(0) @binding(1) var temporal_history: texture_2d<f32>;
@group(0) @binding(2) var reprojection: texture_2d<f32>;
@group(0) @binding(3) var motion: texture_2d<f32>;
@group(0) @binding(4) var depth: texture_2d<f32>;
@group(0) @binding(5) var temporal_depth_history: texture_2d<f32>;
@group(0) @binding(6) var normal_roughness: texture_2d<f32>;
@group(0) @binding(7) var linear_sampler: sampler;
@group(0) @binding(8) var<uniform> params: TemporalParams;
@group(0) @binding(9) var output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var history_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(11) var depth_output: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
	let p = vec2<i32>(id.xy);
	if (any(vec2<f32>(id.xy) >= params.size.xy)) {
		return;
	}
	let z = textureLoad(depth, p, 0).x;
	textureStore(depth_output, p, vec4<f32>(z));
	let c = textureLoad(temporal_current, p, 0);
	let traced = z > 0.0 && textureLoad(normal_roughness, p, 0).w < 0.7;
	if ((params.flags & TEMPORAL_CONTINUES) == 0u || !traced) {
		textureStore(output, p, c);
		textureStore(history_output, p, c);
		return;
	}
	let view = TemporalView(params.inverse_view_projection, params.previous_view_projection, params.size, params.near, params.previous_near);
	// SGL3D motion (current minus previous) at full size, negated into Wicked's.
	let motion_scale = vec2<f32>(textureDimensions(motion)) * params.size.zw;
	let velocity = -textureLoad(motion, vec2<i32>((vec2<f32>(p) + 0.5) * motion_scale), 0).xy;
	let accumulated = temporal_accumulate(view, p, c, velocity, textureLoad(reprojection, p, 0).x, z);
	let result = max(accumulated.color, vec4<f32>(0.0));
	textureStore(output, p, result);
	textureStore(history_output, p, result);
}
