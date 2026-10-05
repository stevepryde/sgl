// Pseudo-random numbers from a pixel and a frame, where Wicked Engine reads
// its blue-noise texture: Wicked Engine 2ff1d9e's ssr_resolveCS.hlsl
// baseHash and hash33 (MIT, src/LICENSE-wicked.txt).
fn base_hash(p0:vec3<u32>)->u32 {
 let p=1103515245u*((p0>>vec3(1u))^p0.yzx);
 let h32=1103515245u*((p.x^p.z)^(p.y>>3u));
 return h32^(h32>>16u);
}
fn hash33(x:vec3<u32>)->vec3<u32> {
 let n=base_hash(x);
 return vec3(n,n*16807u,n*48271u);
}
// Three numbers in [0, 1] from hash33: a word near 2^32 rounds up to 1 in
// f32.
fn hash33_unit(x:vec3<u32>)->vec3<f32> {
 return vec3<f32>(hash33(x))*(1./4294967296.);
}
