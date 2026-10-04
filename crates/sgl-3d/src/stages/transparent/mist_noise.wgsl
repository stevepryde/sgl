// MaterialX 2D gradient noise, translated from Three.js mx_noise.js (MIT).
fn rotate(x:u32,k:u32)->u32 {return (x<<k)|(x>>(32u-k));}
fn hash_final(aa:u32,bb:u32,cc:u32)->u32 {
 var a=aa;var b=bb;var c=cc;
 c=(c^b)-rotate(b,14u);a=(a^c)-rotate(c,11u);b=(b^a)-rotate(a,25u);
 c=(c^b)-rotate(b,16u);a=(a^c)-rotate(c,4u);b=(b^a)-rotate(a,14u);
 return (c^b)-rotate(b,24u);
}
fn gradient2(p:vec2<i32>,f:vec2<f32>)->f32 {
 let seed=0xdeadbeefu+21u;let h=hash_final(seed+bitcast<u32>(p.x),seed+bitcast<u32>(p.y),seed)&7u;
 let u=select(f.y,f.x,h<4u);let v=2.*select(f.x,f.y,h<4u);
 return select(u,-u,(h&1u)!=0u)+select(v,-v,(h&2u)!=0u);
}
fn noise2(p:vec2<f32>)->f32 {
 let i=vec2<i32>(floor(p));let f=fract(p);let u=f*f*f*(f*(f*6.-vec2(15.))+vec2(10.));
 return .6616*mix(mix(gradient2(i,f),gradient2(i+vec2(1,0),f-vec2(1.,0.)),u.x),mix(gradient2(i+vec2(0,1),f-vec2(0.,1.)),gradient2(i+vec2(1,1),f-vec2(1.,1.)),u.x),u.y);
}

/*
The MIT License

Copyright © 2010-2026 three.js authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
*/
