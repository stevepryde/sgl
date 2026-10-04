// WGSL port of Godot Engine's
// servers/rendering/renderer_rd/shaders/effects/screen_space_reflection_filter.glsl
// (https://github.com/godotengine/godot, revision
// ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable), MIT licensed
// (LICENSE-godot.txt). Modified: translated to WGSL; push constants are a
// uniform; the destination is RGBA16F.

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var dest: texture_storage_2d<rgba16float, write>;

struct Params {
	screen_size: vec2<i32>,
	mip_level: u32,
	pad: i32,
}
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var source_sampler: sampler;

var<workgroup> cache: array<array<vec4<f32>, 16>, 16>;

// WGSL: private so the loops can index it at run time.
var<private> WEIGHTS: array<f32, 7> = array<f32, 7>(
		0.07130343198685299,
		0.1315141208431224,
		0.18987923288883812,
		0.21460642856237303,
		0.18987923288883812,
		0.1315141208431224,
		0.07130343198685299);

fn get_weight(c: vec4<f32>) -> f32 {
	return mix(clamp(f32(params.mip_level) * 0.2, 0.0, 1.0), 1.0, c.a);
}

fn apply_gaus_horz(local: vec2<i32>) -> vec4<f32> {
	var c = vec4<f32>(0.0);
	var w = 0.0;
	for (var i = 0; i < 7; i++) {
		let ci = cache[local.x - 3 + i][local.y];
		let wi = WEIGHTS[i] * get_weight(ci);
		c += ci * wi;
		w += wi;
	}

	if (w > 0.0) {
		c /= w;
	} else {
		c = vec4<f32>(0.0);
	}

	return c;
}

var<workgroup> temp_cache: array<array<vec4<f32>, 16>, 8>;

fn apply_gaus_vert(local: vec2<i32>) -> vec4<f32> {
	var c = vec4<f32>(0.0);
	var w = 0.0;
	for (var i = 0; i < 7; i++) {
		let ci = temp_cache[local.x][local.y - 3 + i];
		let wi = WEIGHTS[i] * get_weight(ci);
		c += ci * wi;
		w += wi;
	}

	if (w > 0.0) {
		c /= w;
	} else {
		c = vec4<f32>(0.0);
	}

	return c;
}

fn get_sample(pixel_pos: vec2<i32>) -> vec4<f32> {
	return textureSampleLevel(source, source_sampler, (vec2<f32>(pixel_pos) + 0.5) / vec2<f32>(params.screen_size), 0.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>, @builtin(local_invocation_id) local_id: vec3<u32>) {
	let global = vec2<i32>(global_id.xy);
	let local = vec2<i32>(local_id.xy);

	cache[local.x * 2 + 0][local.y * 2 + 0] = get_sample(global + local - 4 + vec2<i32>(0, 0));
	cache[local.x * 2 + 1][local.y * 2 + 0] = get_sample(global + local - 4 + vec2<i32>(1, 0));
	cache[local.x * 2 + 0][local.y * 2 + 1] = get_sample(global + local - 4 + vec2<i32>(0, 1));
	cache[local.x * 2 + 1][local.y * 2 + 1] = get_sample(global + local - 4 + vec2<i32>(1, 1));

	workgroupBarrier();

	temp_cache[local.x][local.y * 2 + 0] = apply_gaus_horz(vec2<i32>(local.x + 4, local.y * 2 + 0));
	temp_cache[local.x][local.y * 2 + 1] = apply_gaus_horz(vec2<i32>(local.x + 4, local.y * 2 + 1));

	workgroupBarrier();

	if (any(global >= params.screen_size)) {
		return;
	}

	textureStore(dest, global, apply_gaus_vert(local + vec2<i32>(0, 4)));
}
