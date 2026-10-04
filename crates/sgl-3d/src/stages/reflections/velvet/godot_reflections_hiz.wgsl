// WGSL port of Godot Engine's
// servers/rendering/renderer_rd/shaders/effects/screen_space_reflection_hiz.glsl
// (https://github.com/godotengine/godot, revision
// ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable), MIT licensed
// (LICENSE-godot.txt). Modified: translated to WGSL; the odd-size shader
// variants are branches on the source size.

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var dest: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
	let pixel_pos = vec2<i32>(global_id.xy);
	let screen_size = vec2<i32>(textureDimensions(dest));

	if (any(pixel_pos >= screen_size)) {
		return;
	}

	let parent_size = textureDimensions(source);
	let odd_width = (parent_size.x % 2u) != 0u;
	let odd_height = (parent_size.y % 2u) != 0u;

	var depth = textureLoad(source, pixel_pos * 2 + vec2<i32>(0, 0), 0).x;
	depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(1, 0), 0).x);
	depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(0, 1), 0).x);
	depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(1, 1), 0).x);

	if (odd_width) {
		depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(2, 0), 0).x);
		depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(2, 1), 0).x);
	}

	if (odd_height) {
		depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(0, 2), 0).x);
		depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(1, 2), 0).x);
	}

	if (odd_width && odd_height) {
		depth = max(depth, textureLoad(source, pixel_pos * 2 + vec2<i32>(2, 2), 0).x);
	}

	textureStore(dest, pixel_pos, vec4<f32>(depth, 0.0, 0.0, 0.0));
}
