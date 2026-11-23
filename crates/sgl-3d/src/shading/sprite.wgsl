// Sprites (content/sprite.rs): a batch's records and look, and where a
// sprite's corners stand in a view. The one owner of billboard facing:
// sprite batches (stages/transparent/sprites.wgsl) and the mist
// (stages/transparent/mist.wgsl) both take their corners from it. Rust
// layout: shading::sprite.
//
// Facing in the view's image plane, the pivot, the rotation about it and
// the size kept at one metre of view depth are three.js r187's sprite vertex
// stage (src/renderers/shaders/ShaderLib/sprite.glsl.js, revision 7e89746,
// MIT, see stages/post/smaa/LICENSE-three.txt), taken from the view's axes in
// the world instead of its model-view matrix. The axis-locked facing is
// Godot b130438's BILLBOARD_FIXED_Y (scene/resources/material.cpp 1263-1271,
// MIT, see LICENSE-godot.txt) about any axis: its right the cross product of
// the axis and the view's backward axis, so every card of a batch is
// parallel.

// One sprite (shading::sprite::SpriteRecord).
struct SpriteRecord {
 position:vec3<f32>,
 rotation:f32,
 size:vec2<f32>,
 pivot:vec2<f32>,
 // Its image region's top left (xy) and bottom right (zw) in UV.
 region:vec4<f32>,
 tint:vec4<f32>,
}
// A batch's look (shading::sprite::SpriteBatchUniform).
struct SpriteBatch {
 // Its facing's world axis; zero for SpriteFacing::Camera.
 axis:vec3<f32>,
 // SPRITE_FIXED_SIZE.
 flags:u32,
 // A masked batch's alpha cutoff; 0 for a blended one.
 cutoff:f32,
}
const SPRITE_FIXED_SIZE:u32=1u;
// A sprite's two axes in the world for a view whose world-to-view matrix is
// `view_matrix`: right and up.
struct SpriteAxes {
 right:vec3<f32>,
 up:vec3<f32>,
}
// The view's own right, up and backward axes are its matrix's rows, which
// map to the view's +x and +y whatever its handedness: a view that mirrors
// (a negative determinant) shows a sprite unmirrored, as three.js does, and
// an axis-locked card's right is turned to match.
fn sprite_axes(view_matrix:mat4x4<f32>,axis:vec3<f32>)->SpriteAxes {
 let right=vec3(view_matrix[0].x,view_matrix[1].x,view_matrix[2].x);
 let up=vec3(view_matrix[0].y,view_matrix[1].y,view_matrix[2].y);
 if dot(axis,axis)==0. {
  return SpriteAxes(right,up);
 }
 let back=vec3(view_matrix[0].z,view_matrix[1].z,view_matrix[2].z);
 let a=normalize(axis);
 let handed=sign(dot(cross(right,up),back));
 var turned=cross(a,back)*handed;
 // Looking along the axis, the card is edge-on: its right is the view's,
 // which is then across the axis.
 if dot(turned,turned)<1e-12 {
  turned=right-a*dot(a,right);
 }
 return SpriteAxes(normalize(turned),a);
}
// Corner `corner` (each in -0.5..0.5, y up) of `sprite` of `batch` in the
// world, as the view whose world-to-view matrix is `view_matrix` and
// projection is `projection` faces it: offset from the pivot, scaled to its
// size (times its view depth under a perspective projection with
// SPRITE_FIXED_SIZE), turned by its rotation anticlockwise, and laid along
// the view's sprite axes.
fn sprite_corner(sprite:SpriteRecord,batch:SpriteBatch,view_matrix:mat4x4<f32>,projection:mat4x4<f32>,corner:vec2<f32>)->vec3<f32> {
 var scale=sprite.size;
 if (batch.flags&SPRITE_FIXED_SIZE)!=0u && projection[2][3]!=0. {
  scale*=-(view_matrix*vec4(sprite.position,1.)).z;
 }
 let aligned=(corner-(sprite.pivot-vec2(.5)))*scale;
 let c=cos(sprite.rotation);
 let s=sin(sprite.rotation);
 let turned=vec2(c*aligned.x-s*aligned.y,s*aligned.x+c*aligned.y);
 let axes=sprite_axes(view_matrix,batch.axis);
 return sprite.position+axes.right*turned.x+axes.up*turned.y;
}
// The quad's six corners, two triangles, y up.
fn sprite_quad_corner(vertex:u32)->vec2<f32> {
 let corners=array<vec2<f32>,6>(vec2(-.5,-.5),vec2(.5,-.5),vec2(.5,.5),vec2(-.5,-.5),vec2(.5,.5),vec2(-.5,.5));
 return corners[vertex];
}
// The image coordinate at `corner` of `sprite`: its region's top left at
// the quad's top left.
fn sprite_uv(sprite:SpriteRecord,corner:vec2<f32>)->vec2<f32> {
 return mix(sprite.region.xy,sprite.region.zw,vec2(corner.x+.5,.5-corner.y));
}
