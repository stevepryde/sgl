// WGSL port of Godot Engine's
// servers/rendering/renderer_rd/shaders/effects/screen_space_reflection_downsample.glsl
// (https://github.com/godotengine/godot, revision
// ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable), MIT licensed
// (LICENSE-godot.txt). Modified: translated to WGSL; the odd-size shader
// variants are branches on the source size; the normal-roughness buffer is
// RGBA16F.

@group(0) @binding(0) var source_depth: texture_2d<f32>;
@group(0) @binding(1) var source_normal_roughness: texture_2d<f32>;
@group(0) @binding(2) var dest_depth: texture_storage_2d<r32float, write>;
@group(0) @binding(3) var dest_normal_roughness: texture_storage_2d<rgba16float, write>;

fn get_sample(sample_pos: vec2<i32>, depth: ptr<function, f32>, winner_sample_pos: ptr<function, vec2<i32>>) {
	let sample_depth = textureLoad(source_depth, sample_pos, 0).x;

	if (*depth < sample_depth) {
		*depth = sample_depth;
		*winner_sample_pos = sample_pos;
	}
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
	let pixel_pos = vec2<i32>(global_id.xy);
	let screen_size = vec2<i32>(textureDimensions(dest_depth));

	if (any(pixel_pos >= screen_size)) {
		return;
	}

	let parent_size = textureDimensions(source_depth);
	let odd_width = (parent_size.x % 2u) != 0u;
	let odd_height = (parent_size.y % 2u) != 0u;

	var sample_pos = pixel_pos * 2 + vec2<i32>(0, 0);
	var depth = textureLoad(source_depth, sample_pos, 0).x;

	get_sample(pixel_pos * 2 + vec2<i32>(1, 0), &depth, &sample_pos);
	get_sample(pixel_pos * 2 + vec2<i32>(0, 1), &depth, &sample_pos);
	get_sample(pixel_pos * 2 + vec2<i32>(1, 1), &depth, &sample_pos);

	if (odd_width) {
		get_sample(pixel_pos * 2 + vec2<i32>(2, 0), &depth, &sample_pos);
		get_sample(pixel_pos * 2 + vec2<i32>(2, 1), &depth, &sample_pos);
	}

	if (odd_height) {
		get_sample(pixel_pos * 2 + vec2<i32>(0, 2), &depth, &sample_pos);
		get_sample(pixel_pos * 2 + vec2<i32>(1, 2), &depth, &sample_pos);
	}

	if (odd_width && odd_height) {
		get_sample(pixel_pos * 2 + vec2<i32>(2, 2), &depth, &sample_pos);
	}

	textureStore(dest_depth, pixel_pos, vec4<f32>(depth, 0.0, 0.0, 0.0));
	textureStore(dest_normal_roughness, pixel_pos, textureLoad(source_normal_roughness, sample_pos, 0));
}
