// WGSL port of Godot Engine's
// servers/rendering/renderer_rd/shaders/effects/screen_space_reflection_resolve.glsl
// (resolve_half) and of the SSR sampling in
// servers/rendering/renderer_rd/shaders/forward_clustered/scene_forward_clustered.glsl
// (resolve_full) (https://github.com/godotengine/godot, revision
// ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable), MIT licensed
// (LICENSE-godot.txt). Modified: translated to WGSL; the normal-roughness
// buffers hold perceptual roughness directly, without Godot's dynamic-object
// encoding; the mip level is R32F; at full size, Godot's forward pass samples
// the mip chain where resolve_full writes it once for SGL3D's composition.
// Half-resolution UVs use the allocated texture dimensions, including odd sizes.
// Resolve neighborhoods use floor-based texel centers at the top/left borders,
// retaining interior weights, and clamp color and metadata together at all borders.
// The inverse tone map's denominator is at least binary16's step below 1.0, so
// a trace value that rounded to 1.0 in RGBA16F stays finite (#322).

@group(0) @binding(0) var source_depth: texture_2d<f32>;
@group(0) @binding(1) var source_normal_roughness: texture_2d<f32>;
@group(0) @binding(2) var source_depth_half: texture_2d<f32>;
@group(0) @binding(3) var source_normal_roughness_half: texture_2d<f32>;
@group(0) @binding(4) var source_color: texture_2d<f32>;
@group(0) @binding(5) var source_mip_level: texture_2d<f32>;
@group(0) @binding(6) var output_color: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var linear_sampler: sampler;

// Invert the tone mapping we applied in the main trace pass.
fn inverse_tone_map(color: vec4<f32>) -> vec4<f32> {
	let rec709_luminance_weights = vec3<f32>(0.2126, 0.7152, 0.0722);
	return vec4<f32>(color.rgb / max(1.0 - dot(color.rgb, rec709_luminance_weights), 1.0 / 2048.0), color.a);
}

fn get_sample(depth: f32, normal: vec3<f32>, roughness: f32, requested_pixel_pos: vec2<i32>, color: ptr<function, vec4<f32>>, weight: ptr<function, f32>) {
	let half_size = vec2<i32>(textureDimensions(source_color));
	let pixel_pos = clamp(requested_pixel_pos, vec2<i32>(0), half_size - 1);
	let sample_depth = textureLoad(source_depth_half, pixel_pos, 0).x;
	let sample_normal_roughness = textureLoad(source_normal_roughness_half, pixel_pos, 0);
	let sample_normal = normalize(sample_normal_roughness.xyz * 2.0 - 1.0);
	let sample_roughness = sample_normal_roughness.w;

	let uv = (vec2<f32>(pixel_pos) + 0.5) / vec2<f32>(half_size);

	let mip_level = textureLoad(source_mip_level, pixel_pos, 0).x * 14.0;
	*color = inverse_tone_map(textureSampleLevel(source_color, linear_sampler, uv, mip_level));

	const DEPTH_FACTOR = 2048.0;
	const NORMAL_FACTOR = 32.0;
	const ROUGHNESS_FACTOR = 16.0;

	let depth_diff = abs(depth - sample_depth);
	let weight_depth = exp(-depth_diff * DEPTH_FACTOR);

	let normal_diff = clamp(1.0 - dot(normal, sample_normal), 0.0, 1.0);
	let weight_normal = exp(-normal_diff * NORMAL_FACTOR);

	let roughness_diff = abs(roughness - sample_roughness);
	let weight_roughness = exp(-roughness_diff * ROUGHNESS_FACTOR);

	*weight = weight_depth * weight_normal * weight_roughness;
}

@compute @workgroup_size(8, 8, 1)
fn resolve_half(@builtin(global_invocation_id) global_id: vec3<u32>) {
	let pixel_pos = vec2<i32>(global_id.xy);
	let screen_size = vec2<f32>(textureDimensions(output_color));

	if (any(vec2<f32>(pixel_pos) >= screen_size)) {
		return;
	}

	let depth = textureLoad(source_depth, pixel_pos, 0).x;
	let normal_roughness = textureLoad(source_normal_roughness, pixel_pos, 0);
	let normal = normalize(normal_roughness.xyz * 2.0 - 1.0);
	let roughness = normal_roughness.w;

	// Convert the full-resolution pixel center to half-resolution texel centers.
	let half_tex_coord = (vec2<f32>(pixel_pos) + 0.5) * 0.5 - 0.5;
	let base_pixel = vec2<i32>(floor(half_tex_coord));
	let bilinear_weights = fract(half_tex_coord);

	var color0: vec4<f32>;
	var color1: vec4<f32>;
	var color2: vec4<f32>;
	var color3: vec4<f32>;
	var weight0: f32;
	var weight1: f32;
	var weight2: f32;
	var weight3: f32;

	get_sample(depth, normal, roughness, base_pixel + vec2<i32>(0, 0), &color0, &weight0);
	get_sample(depth, normal, roughness, base_pixel + vec2<i32>(1, 0), &color1, &weight1);
	get_sample(depth, normal, roughness, base_pixel + vec2<i32>(0, 1), &color2, &weight2);
	get_sample(depth, normal, roughness, base_pixel + vec2<i32>(1, 1), &color3, &weight3);

	weight0 *= (1.0 - bilinear_weights.x) * (1.0 - bilinear_weights.y);
	weight1 *= bilinear_weights.x * (1.0 - bilinear_weights.y);
	weight2 *= (1.0 - bilinear_weights.x) * bilinear_weights.y;
	weight3 *= bilinear_weights.x * bilinear_weights.y;

	var result_color = color0 * weight0 + color1 * weight1 + color2 * weight2 + color3 * weight3;
	let result_weight = weight0 + weight1 + weight2 + weight3;
	if (result_weight > 0.0) {
		result_color /= result_weight;
	} else {
		result_color = vec4<f32>(0.0);
	}

	textureStore(output_color, pixel_pos, result_color);
}

// scene_forward_clustered.glsl's "process ssr" at full size
// (SCREEN_SPACE_EFFECTS_FLAGS_RESOLVE_SSR): the mip chain at the pixel's mip
// level, with the trace pass's tone mapping inverted. Premultiplied alpha.
@compute @workgroup_size(8, 8, 1)
fn resolve_full(@builtin(global_invocation_id) global_id: vec3<u32>) {
	let pixel_pos = vec2<i32>(global_id.xy);
	let screen_size = vec2<f32>(textureDimensions(output_color));

	if (any(vec2<f32>(pixel_pos) >= screen_size)) {
		return;
	}

	let screen_uv = (vec2<f32>(pixel_pos) + 0.5) / screen_size;
	var ssr_mip_level = textureLoad(source_mip_level, pixel_pos, 0).x;
	ssr_mip_level *= 14.0;
	let ssr = inverse_tone_map(textureSampleLevel(source_color, linear_sampler, screen_uv, ssr_mip_level));

	textureStore(output_color, pixel_pos, ssr);
}
