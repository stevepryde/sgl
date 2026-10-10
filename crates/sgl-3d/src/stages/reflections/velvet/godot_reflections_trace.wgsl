// WGSL port of Godot Engine's
// servers/rendering/renderer_rd/shaders/effects/screen_space_reflection.glsl
// (https://github.com/godotengine/godot, revision
// ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable), MIT licensed
// (LICENSE-godot.txt). Modified: translated to WGSL for one view (no
// multiview); push constants are a uniform; the normal-roughness buffer holds
// perceptual roughness directly, without Godot's dynamic-object encoding; the
// mip level is stored in R32F; source_last_frame is this frame's radiance, so
// the host's reprojection is the identity. Godot's near-origin normal rejection
// is retained. Godot validates a ray that runs out of steps before confirming
// a hit at the finest level by its endpoint's depth proximity, which accepts
// unfinished endpoints beside a surface; here such a ray is a miss, as AR-12
// (specs/sgl3d-architecture.md) requires and Crystal does (sgl-post-fx
// PROVENANCE.md, DFX-38).
// Missing depth derivatives fall back to the receiver's stored normal, the
// normal source used by AMD SSSR (ffx-sssr/ffx_sssr.h, revision
// 34dcacd1feefcfab2855b82e76c7d711f2020a75). Exact zero-depth-gradient rays
// are rejected because this Godot/Supnik Z-parameterization cannot represent them.
// SGL3D also writes the post-projection depth of each receiver's reflected
// point, the input of godot_reflections_temporal.wgsl's hit reprojection.

// The most steps a trace takes, the top of Godot's `ssr_max_steps` range
// (scene/resources/environment.cpp at the revision above), so a trace ends
// whatever its parameters say.
const MOST_STEPS: i32 = 512;

@group(0) @binding(0) var source_last_frame: texture_2d<f32>;
@group(0) @binding(1) var source_hiz: texture_2d<f32>;
@group(0) @binding(2) var source_normal_roughness: texture_2d<f32>;
@group(0) @binding(3) var output_color: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var output_mip_level: texture_storage_2d<r32float, write>;
@group(0) @binding(8) var output_reprojection: texture_storage_2d<r32float, write>;

struct SceneData {
	projection: mat4x4<f32>,
	inv_projection: mat4x4<f32>,
	reprojection: mat4x4<f32>,
	eye_offset: vec4<f32>,
}
@group(0) @binding(5) var<uniform> scene_data: SceneData;

struct Params {
	screen_size: vec2<i32>,
	mipmaps: i32,
	num_steps: i32,
	distance_fade: f32,
	curve_fade_in: f32,
	depth_tolerance: f32,
	orthogonal: u32,
}
@group(0) @binding(6) var<uniform> params: Params;
@group(0) @binding(7) var linear_sampler: sampler;

fn compute_cell_count(level: i32) -> vec2<f32> {
	let cell_count_x = max(1, params.screen_size.x >> u32(level));
	let cell_count_y = max(1, params.screen_size.y >> u32(level));
	return vec2<f32>(f32(cell_count_x), f32(cell_count_y));
}

fn linearize_depth(depth: f32) -> f32 {
	var pos = vec4<f32>(0.0, 0.0, depth, 1.0);
	pos = scene_data.inv_projection * pos;
	return pos.z / pos.w;
}

fn compute_view_pos(screen_pos: vec3<f32>) -> vec3<f32> {
	var pos: vec4<f32>;
	pos = vec4<f32>(screen_pos.xy * 2.0 - 1.0, screen_pos.z, 1.0);
	pos = scene_data.inv_projection * pos;
	return pos.xyz / pos.w;
}

fn compute_screen_pos(pos: vec3<f32>) -> vec3<f32> {
	var screen_pos = scene_data.projection * vec4<f32>(pos, 1.0);
	screen_pos = vec4<f32>(screen_pos.xyz / screen_pos.w, screen_pos.w);
	return vec3<f32>(screen_pos.xy * 0.5 + 0.5, screen_pos.z);
}

// https://habr.com/ru/articles/744336/
fn compute_geometric_normal(pixel_pos: vec2<i32>, depth_c: f32, view_c: vec3<f32>, pixel_offset: f32) -> vec3<f32> {
	let H = vec4<f32>(
			textureLoad(source_hiz, pixel_pos + vec2<i32>(-1, 0), 0).x,
			textureLoad(source_hiz, pixel_pos + vec2<i32>(-2, 0), 0).x,
			textureLoad(source_hiz, pixel_pos + vec2<i32>(1, 0), 0).x,
			textureLoad(source_hiz, pixel_pos + vec2<i32>(2, 0), 0).x);

	let V = vec4<f32>(
			textureLoad(source_hiz, pixel_pos + vec2<i32>(0, -1), 0).x,
			textureLoad(source_hiz, pixel_pos + vec2<i32>(0, -2), 0).x,
			textureLoad(source_hiz, pixel_pos + vec2<i32>(0, 1), 0).x,
			textureLoad(source_hiz, pixel_pos + vec2<i32>(0, 2), 0).x);

	let he = abs((2.0 * H.xz - H.yw) - depth_c);
	let ve = abs((2.0 * V.xz - V.yw) - depth_c);

	// Sky has no finite view position with infinite reversed-Z. A derivative
	// needs a geometry sample on each axis; otherwise use the receiver normal
	// rather than reconstructing a surface from infinity.
	if ((H.x == 0.0 && H.z == 0.0) || (V.x == 0.0 && V.z == 0.0)) {
		return textureLoad(source_normal_roughness, pixel_pos, 0).xyz * 2.0 - 1.0;
	}
	let h_sign = select(1, -1, H.x != 0.0 && (H.z == 0.0 || he.x < he.y));
	let v_sign = select(1, -1, V.x != 0.0 && (V.z == 0.0 || ve.x < ve.y));

	let screen_size = vec2<f32>(params.screen_size);
	let view_h = compute_view_pos(vec3<f32>((vec2<f32>(pixel_pos) + vec2<f32>(f32(h_sign), 0.0) + pixel_offset) / screen_size, H[1 + h_sign]));
	let view_v = compute_view_pos(vec3<f32>((vec2<f32>(pixel_pos) + vec2<f32>(0.0, f32(v_sign)) + pixel_offset) / screen_size, V[1 + v_sign]));

	let h_der = f32(h_sign) * (view_h - view_c);
	let v_der = f32(v_sign) * (view_v - view_c);

	let geometric_normal = cross(v_der, h_der);
	if (dot(geometric_normal, geometric_normal) == 0.0) {
		return textureLoad(source_normal_roughness, pixel_pos, 0).xyz * 2.0 - 1.0;
	}
	return geometric_normal;
}

const M_PI = 3.14159265359;

// SGL3D: the depth of the point `distance` behind the receiver along its view
// ray, where its reflection appears (Wicked Engine ssr_resolveCS.hlsl).
fn reflected_depth(receiver: vec3<f32>, distance: f32) -> f32 {
	let clip = scene_data.projection * vec4<f32>(0.0, 0.0, receiver.z - distance, 1.0);
	return clip.z / clip.w;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
	let pixel_pos = vec2<i32>(global_id.xy);

	if (any(pixel_pos >= params.screen_size)) {
		return;
	}

	let screen_size = vec2<f32>(params.screen_size);
	var color = vec4<f32>(0.0);
	var mip_level = 0.0;
	var reprojection = 0.0;

	var screen_pos: vec3<f32>;
	screen_pos = vec3<f32>((vec2<f32>(pixel_pos) + 0.5) / screen_size, textureLoad(source_hiz, pixel_pos, 0).x);

	let should_trace = screen_pos.z != 0.0;
	if (should_trace) {
		var pos = compute_view_pos(screen_pos);
		let receiver_pos = pos;

		let normal_roughness = textureLoad(source_normal_roughness, pixel_pos, 0);
		let normal = normalize(normal_roughness.xyz * 2.0 - 1.0);
		// SGL3D: perceptual roughness as stored (Godot decodes its dynamic-object
		// flag and 127/255 scale here).
		let roughness = normal_roughness.w;

		// Do not compute SSR for rough materials to improve
		// performance at the cost of subtle artifacting.
		if (roughness >= 0.7) {
			textureStore(output_color, pixel_pos, vec4<f32>(0.0));
			textureStore(output_mip_level, pixel_pos, vec4<f32>(0.0));
			textureStore(output_reprojection, pixel_pos, vec4<f32>(0.0));
			return;
		}

		let geom_normal = normalize(compute_geometric_normal(pixel_pos, screen_pos.z, pos, 0.5));

		// Add a small bias towards the geometry normal to prevent self intersections.
		pos += geom_normal * (1.0 - pow(clamp(dot(normal, geom_normal), 0.0, 1.0), 8.0));
		screen_pos = compute_screen_pos(pos);

		let view_dir = select(normalize(pos + scene_data.eye_offset.xyz), vec3<f32>(0.0, 0.0, -1.0), params.orthogonal != 0u);
		var ray_dir = normalize(reflect(view_dir, normal));

		// Check if the ray is immediately intersecting with itself. If so, bounce!
		if (dot(ray_dir, geom_normal) < 0.0) {
			ray_dir = normalize(reflect(ray_dir, geom_normal));
		}

		var end_pos = pos + ray_dir;

		// Clip to near plane. Add a small bias so we don't go to infinity.
		if (end_pos.z > 0.0) {
			end_pos -= ray_dir / ray_dir.z * (end_pos.z + 0.00001);
		}

		let screen_end_pos = compute_screen_pos(end_pos);

		// Normalize Z to -1.0 or +1.0 and do parametric T tracing as suggested here:
		// https://hacksoflife.blogspot.com/2020/10/a-tip-for-hiz-ssr-parametric-t-tracing.html
		var screen_ray_dir = screen_end_pos - screen_pos;
		// No depth-plane parameter exists for a ray parallel to those planes.
		// Leave this ray to probe/sky fallback; do not perturb its direction.
		if (screen_ray_dir.z == 0.0) {
			textureStore(output_color, pixel_pos, vec4<f32>(0.0));
			textureStore(output_mip_level, pixel_pos, vec4<f32>(0.0));
			textureStore(output_reprojection, pixel_pos, vec4<f32>(reflected_depth(receiver_pos, 0.0)));
			return;
		}
		screen_ray_dir /= abs(screen_ray_dir.z);

		let facing_camera = screen_ray_dir.z >= 0.0;

		// Find the screen edge point where we will stop tracing.
		let t0 = (vec2<f32>(0.0) - screen_pos.xy) / screen_ray_dir.xy;
		let t1 = (vec2<f32>(1.0) - screen_pos.xy) / screen_ray_dir.xy;
		let t2 = max(t0, t1);
		let t_max = min(t2.x, t2.y);

		let cell_step = vec2<f32>(select(1.0, -1.0, screen_ray_dir.x < 0.0), select(1.0, -1.0, screen_ray_dir.y < 0.0));

		var cur_level = 0;
		var cur_iteration = min(params.num_steps, MOST_STEPS);

		// Advance the start point to the closest next cell to prevent immediate self intersection.
		var t: f32;
		{
			let cell_index = floor(screen_pos.xy * screen_size);
			let new_cell_index = cell_index + clamp(cell_step, vec2<f32>(0.0), vec2<f32>(1.0));
			let new_cell_pos = (new_cell_index / screen_size) + cell_step * 0.000001;
			let pos_t = (new_cell_pos - screen_pos.xy) / screen_ray_dir.xy;
			let edge_t = min(pos_t.x, pos_t.y);

			t = edge_t;
		}

		while (cur_level >= 0 && cur_iteration > 0 && t < t_max) {
			let cur_screen_pos = screen_pos + screen_ray_dir * t;

			let cell_count = compute_cell_count(cur_level);
			let cell_index = floor(cur_screen_pos.xy * cell_count);
			let cell_depth = textureLoad(source_hiz, vec2<i32>(cell_index), cur_level).x;
			let depth_t = (cell_depth - screen_pos.z) * screen_ray_dir.z; // Z is either -1.0 or 1.0 so we don't need to do a divide.

			let new_cell_index = cell_index + clamp(cell_step, vec2<f32>(0.0), vec2<f32>(1.0));
			let new_cell_pos = (new_cell_index / cell_count) + cell_step * 0.000001;
			let pos_t = (new_cell_pos - screen_pos.xy) / screen_ray_dir.xy;
			let edge_t = min(pos_t.x, pos_t.y);

			var hit = select(depth_t <= edge_t, t <= depth_t, facing_camera);
			var mip_offset = select(1, -1, hit);

			if (cur_level == 0) {
				let z0 = linearize_depth(cell_depth);
				let z1 = linearize_depth(cur_screen_pos.z);

				if ((z0 - z1) > params.depth_tolerance) {
					hit = false;
					mip_offset = 0; // Keep the mip index the same to prevent it from decreasing and increasing in repeat.
				}
			}

			if (hit) {
				if (!facing_camera) {
					t = max(t, depth_t);
				}
			} else {
				t = edge_t;
			}

			cur_level = min(cur_level + mip_offset, params.mipmaps - 1);
			cur_iteration -= 1;
		}

		let cur_screen_pos = screen_pos + screen_ray_dir * t;

		var reprojected_pos: vec4<f32>;
		reprojected_pos = vec4<f32>(cur_screen_pos.xy * 2.0 - 1.0, cur_screen_pos.z, 1.0);
		reprojected_pos = scene_data.reprojection * reprojected_pos;
		reprojected_pos = vec4<f32>(reprojected_pos.xy / reprojected_pos.w * 0.5 + 0.5, reprojected_pos.zw);

		// Instead of hard rejecting samples, write sample validity to the alpha channel.
		// This allows invalid samples to write mip levels to let valid samples have smoother roughness transitions.
		var validity = 1.0;

		// Hit validation logic is referenced from here:
		// https://github.com/GPUOpen-Effects/FidelityFX-SSSR/blob/master/ffx-sssr/ffx_sssr.h

		let cur_pixel_pos = vec2<i32>(cur_screen_pos.xy * screen_size);

		let hit_depth = textureLoad(source_hiz, cur_pixel_pos, 0).x;
		// SGL3D: a hit is confirmed only by descending below the finest level;
		// a trace that ran out of steps first is a miss.
		if (cur_level >= 0 || t >= t_max || hit_depth == 0.0) {
			validity = 0.0;
		}

		// Preserve Godot's near-origin self-intersection test. Shading normals can
		// face along a grazing ray even when it hits visible target geometry.
		if (all(abs(screen_ray_dir.xy * t) < 2.0 / screen_size)) {
			let hit_normal = textureLoad(source_normal_roughness, cur_pixel_pos, 0).xyz * 2.0 - 1.0;
			if (dot(ray_dir, hit_normal) >= 0.0) {
				validity = 0.0;
			}
		}

		let cur_pos = compute_view_pos(cur_screen_pos);
		let hit_pos = compute_view_pos(vec3<f32>(cur_screen_pos.xy, hit_depth));

		let delta = length(cur_pos - hit_pos);
		let confidence = 1.0 - smoothstep(0.0, params.depth_tolerance, delta);
		validity *= clamp(confidence * confidence, 0.0, 1.0);

		var margin_blend = 1.0;
		let reprojected_pixel_pos = reprojected_pos.xy * screen_size;

		let margin = vec2<f32>((screen_size.x + screen_size.y) * 0.05); // Make a uniform margin.
		{
			// Blend fading out towards inner margin.
			// 0.5 = midpoint of reflection
			let margin_grad = select(screen_size - reprojected_pixel_pos, reprojected_pixel_pos, reprojected_pixel_pos < screen_size * 0.5);
			margin_blend = smoothstep(0.0, margin.x * margin.y, margin_grad.x * margin_grad.y);
		}

		let ray_len = length(screen_ray_dir.xy * t);

		// Fade In / Fade Out
		let grad = ray_len;
		let fade_in = select(pow(clamp(grad, 0.0, 1.0), params.curve_fade_in), 1.0, params.curve_fade_in == 0.0);
		let fade_out = select(pow(clamp(1.0 - grad, 0.0, 1.0), params.distance_fade), 1.0, params.distance_fade == 0.0);
		var fade = fade_in * fade_out;

		// Ensure that precision errors do not introduce any fade. Even if it is just slightly below 1.0,
		// strong specular light can leak through the reflection.
		if (fade > 0.999) {
			fade = 1.0;
		}

		validity *= fade * margin_blend;

		// A miss reprojects with its receiver.
		reprojection = reflected_depth(receiver_pos, select(0.0, length(cur_pos - receiver_pos), validity > 0.0));

		if (validity > 0.0) {
			color = vec4<f32>(textureSampleLevel(source_last_frame, linear_sampler, reprojected_pos.xy, 0.0).xyz, 1.0) * validity;

			// Tone map the SSR color to have smoother roughness filtering across samples with varying luminance.
			let rec709_luminance_weights = vec3<f32>(0.2126, 0.7152, 0.0722);
			color = vec4<f32>(color.rgb / (1.0 + dot(color.rgb, rec709_luminance_weights)), color.a);
		}

		if (roughness > 0.001) {
			let cone_angle = min(roughness, 0.999) * M_PI * 0.5;
			let cone_len = ray_len;
			let op_len = 2.0 * tan(cone_angle) * cone_len; // Opposite side of iso triangle.
			var blur_radius: f32;
			{
				// Fit to sphere inside cone (sphere ends at end of cone), something like this:
				// ___
				// \O/
				//  V
				//
				// as it avoids bleeding from beyond the reflection as much as possible. As a plus
				// it also makes the rough reflection more elongated.
				let a = op_len;
				let h = cone_len;
				let a2 = a * a;
				let fh2 = 4.0 * h * h;
				blur_radius = (a * (sqrt(a2 + fh2) - a)) / (4.0 * h);
			}

			// We approximate the integration in world space with a blur in screen space,
			// and use a mip bias, `log2(/* screen_space_blur_radius */ + 1.0)`, to approximate the screen space blur.
			// This + 1.0 is needed because mip level is logarithmic to the diameter (in pixels) of sampling region,
			// which is 1 pixel when no blur is applied (log2(1) = 0).
			mip_level = clamp(log2(blur_radius * max(screen_size.x, screen_size.y) / 16.0 + 1.0), 0.0, f32(params.mipmaps - 1));
		}

		// Because we still write mip level for invalid pixels to allow for smooth roughness transitions,
		// this sometimes ends up creating a pyramid-like shape at very rough levels.
		// We can fade the mip level near the end to make it significantly less visible.
		mip_level *= pow(clamp(1.25 - ray_len, 0.0, 1.0), 0.2);
	}

	textureStore(output_color, pixel_pos, color);
	textureStore(output_mip_level, pixel_pos, vec4<f32>(mip_level / 14.0, 0.0, 0.0, 0.0));
	textureStore(output_reprojection, pixel_pos, vec4<f32>(reprojection, 0.0, 0.0, 0.0));
}
