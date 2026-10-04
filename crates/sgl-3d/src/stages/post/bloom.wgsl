// Bloom. Ports Bevy 9d12036 crates/bevy_post_process/src/bloom/bloom.wesl
// (karis_average, sample_input_13_tap at uniform scale,
// sample_input_3x3_tent, downsample_first, downsample, upsample), MIT OR
// Apache-2.0 (src/LICENSE-bevy.txt): its energy-conserving form, without the
// threshold prefilter, over the whole target. References: Jimenez, "Next
// Generation Post Processing in Call of Duty: Advanced Warfare" (SIGGRAPH
// 2014), and LearnOpenGL's "Physically Based Bloom". Changes: the input is a
// parameter, sampled at level 0; luminance is the shared Rec. 709
// `luminance`; the Karis average and the first downsample's floor take the
// scene exposed by `bloom_settings.exposure`, as Bevy's main pass has
// exposed it; the last upsample is `composite`, which mixes the completed
// scene with it into a new target by `bloom_settings.composite_blend` instead
// of blending into the scene in place.
fn karis_average(color:vec3<f32>)->f32 {
 let luma=luminance(color)*bloom_settings.exposure/4.;
 return 1./(1.+luma);
}
// [COD] slide 153, with the first downsample's Karis average (slide 168).
fn sample_input_13_tap(input_texture:texture_2d<f32>,uv:vec2<f32>,first_downsample:bool)->vec3<f32> {
 let a=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(-2,2)).rgb;
 let b=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(0,2)).rgb;
 let c=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(2,2)).rgb;
 let d=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(-2,0)).rgb;
 let e=textureSampleLevel(input_texture,linear_sampler,uv,0.).rgb;
 let f=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(2,0)).rgb;
 let g=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(-2,-2)).rgb;
 let h=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(0,-2)).rgb;
 let i=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(2,-2)).rgb;
 let j=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(-1,1)).rgb;
 let k=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(1,1)).rgb;
 let l=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(-1,-1)).rgb;
 let m=textureSampleLevel(input_texture,linear_sampler,uv,0.,vec2<i32>(1,-1)).rgb;
 if first_downsample {
  // A weighted average of the groups against fireflies.
  var group0=(a+b+d+e)*(.125/4.);
  var group1=(b+c+e+f)*(.125/4.);
  var group2=(d+e+g+h)*(.125/4.);
  var group3=(e+f+h+i)*(.125/4.);
  var group4=(j+k+l+m)*(.5/4.);
  group0*=karis_average(group0);
  group1*=karis_average(group1);
  group2*=karis_average(group2);
  group3*=karis_average(group3);
  group4*=karis_average(group4);
  return group0+group1+group2+group3+group4;
 }
 var sample=(a+c+g+i)*.03125;
 sample+=(b+d+f+h)*.0625;
 sample+=(e+j+k+l+m)*.125;
 return sample;
}
// [COD] slide 162.
fn sample_input_3x3_tent(input_texture:texture_2d<f32>,uv:vec2<f32>)->vec3<f32> {
 let frag_size=1./vec2<f32>(textureDimensions(input_texture));
 let x=frag_size.x;
 let y=frag_size.y;
 let a=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x-x,uv.y+y),0.).rgb;
 let b=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x,uv.y+y),0.).rgb;
 let c=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x+x,uv.y+y),0.).rgb;
 let d=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x-x,uv.y),0.).rgb;
 let e=textureSampleLevel(input_texture,linear_sampler,uv,0.).rgb;
 let f=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x+x,uv.y),0.).rgb;
 let g=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x-x,uv.y-y),0.).rgb;
 let h=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x,uv.y-y),0.).rgb;
 let i=textureSampleLevel(input_texture,linear_sampler,vec2(uv.x+x,uv.y-y),0.).rgb;
 var sample=e*.25;
 sample+=(b+d+f+h)*.125;
 sample+=(a+c+g+i)*.0625;
 return sample;
}
// The completed scene into mip 0.
@fragment fn downsample_first(i:Output)->@location(0) vec4<f32> {
 var sample=sample_input_13_tap(scene,i.uv,true);
 // The lower bound, in exposed light, keeps zeros from spreading black boxes
 // through the chain; the upper bound keeps NaNs out.
 sample=clamp(sample,vec3(.0001/bloom_settings.exposure),vec3(3.40282347e37));
 return vec4(sample,1.);
}
// Mip n - 1 into mip n.
@fragment fn downsample(i:Output)->@location(0) vec4<f32> {
 return vec4(sample_input_13_tap(scene,i.uv,false),1.);
}
// Mip n into mip n - 1, blended by the pass's constant.
@fragment fn upsample(i:Output)->@location(0) vec4<f32> {
 return vec4(sample_input_3x3_tent(scene,i.uv),1.);
}
// Mip 0 upsampled and mixed into the completed scene.
@fragment fn composite(i:Output)->@location(0) vec4<f32> {
 let completed=textureSampleLevel(scene,linear_sampler,i.uv,0.);
 let bloomed=sample_input_3x3_tent(bloom,i.uv);
 return vec4(mix(completed.rgb,bloomed,bloom_settings.composite_blend),completed.a);
}
