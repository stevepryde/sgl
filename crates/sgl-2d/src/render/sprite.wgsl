struct Camera {
    view_projection: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var sprite_texture: texture_2d<f32>;
@group(1) @binding(1) var sprite_sampler: sampler;

struct VertexInput {
    @location(0) corner: vec2<f32>,
    @location(1) model_x: vec2<f32>,
    @location(2) model_y: vec2<f32>,
    @location(3) translation: vec2<f32>,
    @location(4) uv_min: vec2<f32>,
    @location(5) uv_max: vec2<f32>,
    @location(6) tint: vec4<f32>,
    @location(7) flips: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
};

@vertex
fn vertex(input: VertexInput) -> VertexOutput {
    let world = input.model_x * input.corner.x
        + input.model_y * input.corner.y
        + input.translation;
    let u = select(input.uv_min.x, input.uv_max.x, input.corner.x > 0.5);
    let v = select(input.uv_min.y, input.uv_max.y, input.corner.y > 0.5);
    let flipped_u = select(u, input.uv_min.x + input.uv_max.x - u, input.flips.x > 0.5);
    let flipped_v = select(v, input.uv_min.y + input.uv_max.y - v, input.flips.y > 0.5);

    var output: VertexOutput;
    output.clip_position = camera.view_projection * vec4<f32>(world, 0.0, 1.0);
    output.uv = vec2<f32>(flipped_u, flipped_v);
    output.tint = input.tint;
    return output;
}

@fragment
fn fragment(input: VertexOutput) -> @location(0) vec4<f32> {
    var color = textureSample(sprite_texture, sprite_sampler, input.uv) * input.tint;
    color = vec4<f32>(color.rgb * color.a, color.a);
    return color;
}
