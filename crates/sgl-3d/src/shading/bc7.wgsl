// One texel of a BC7 block, decoded as the GPU decodes a BC7 texture's
// texels: `bc7_texel(block, texel)`, the block's 16 bytes as four
// little-endian words and the texel's index in it, row by row.
//
// Ported from bcdec's bcdec_bc7 (bcdec.h, as Godot b130438 vendors it in
// thirdparty/misc/bcdec.h: bcdec 3b29f8f), MIT (src/LICENSE-bcdec.txt).
// Changes: it decodes one texel, reading the bits that texel needs at their
// place in the block instead of the whole block's bitstream in order: the
// endpoints of its subset, and its indices after the fix-up texels before
// it, which store one bit fewer. partition_sets is packed per partition
// (BC7_PARTITION_SETS_2 and _3).

// bcdec's actual_bits_count: each mode's endpoint colour and alpha bits.
const BC7_COLOR_BITS=array<u32,8>(4u,6u,5u,7u,5u,7u,7u,5u);
const BC7_ALPHA_BITS=array<u32,8>(0u,0u,0u,0u,6u,8u,7u,5u);
// bcdec's sModeHasPBits: the modes whose endpoints have P-bits.
const BC7_MODE_HAS_P_BITS:u32=0xcbu;
// bcdec's aWeight2, aWeight3 and aWeight4.
const BC7_WEIGHTS_2=array<u32,4>(0u,21u,43u,64u);
const BC7_WEIGHTS_3=array<u32,8>(0u,9u,18u,27u,37u,46u,55u,64u);
const BC7_WEIGHTS_4=array<u32,16>(0u,4u,9u,13u,17u,21u,26u,30u,34u,38u,43u,47u,51u,55u,60u,64u);
// bcdec's partition_sets for two and three subsets, one entry per
// partition: x holds each texel's subset in two bits, texel 0 lowest; y has
// a bit set for each fix-up texel (bcdec's 0x80), whose index has one bit
// fewer.
const BC7_PARTITION_SETS_2=array<vec2<u32>,64>(
 vec2(0x50505050u,0x8001u),vec2(0x40404040u,0x8001u),vec2(0x54545454u,0x8001u),vec2(0x54505040u,0x8001u),
 vec2(0x50404000u,0x8001u),vec2(0x55545450u,0x8001u),vec2(0x55545040u,0x8001u),vec2(0x54504000u,0x8001u),
 vec2(0x50400000u,0x8001u),vec2(0x55555450u,0x8001u),vec2(0x55544000u,0x8001u),vec2(0x54400000u,0x8001u),
 vec2(0x55555440u,0x8001u),vec2(0x55550000u,0x8001u),vec2(0x55555500u,0x8001u),vec2(0x55000000u,0x8001u),
 vec2(0x55150100u,0x8001u),vec2(0x00004054u,0x0005u),vec2(0x15010000u,0x0101u),vec2(0x00405054u,0x0005u),
 vec2(0x00004050u,0x0005u),vec2(0x15050100u,0x0101u),vec2(0x05010000u,0x0101u),vec2(0x40505054u,0x8001u),
 vec2(0x00404050u,0x0005u),vec2(0x05010100u,0x0101u),vec2(0x14141414u,0x0005u),vec2(0x05141450u,0x0005u),
 vec2(0x01155440u,0x0101u),vec2(0x00555500u,0x0101u),vec2(0x15014054u,0x0005u),vec2(0x05414150u,0x0005u),
 vec2(0x44444444u,0x8001u),vec2(0x55005500u,0x8001u),vec2(0x11441144u,0x0041u),vec2(0x05055050u,0x0101u),
 vec2(0x05500550u,0x0005u),vec2(0x11114444u,0x0101u),vec2(0x41144114u,0x8001u),vec2(0x44111144u,0x8001u),
 vec2(0x15055054u,0x0005u),vec2(0x01055040u,0x0101u),vec2(0x05041050u,0x0005u),vec2(0x05455150u,0x0005u),
 vec2(0x14414114u,0x0005u),vec2(0x50050550u,0x8001u),vec2(0x41411414u,0x8001u),vec2(0x00141400u,0x0041u),
 vec2(0x00041504u,0x0041u),vec2(0x00105410u,0x0005u),vec2(0x10541000u,0x0041u),vec2(0x04150400u,0x0101u),
 vec2(0x50410514u,0x8001u),vec2(0x41051450u,0x8001u),vec2(0x05415014u,0x0005u),vec2(0x14054150u,0x0005u),
 vec2(0x41050514u,0x8001u),vec2(0x41505014u,0x8001u),vec2(0x40011554u,0x8001u),vec2(0x54150140u,0x8001u),
 vec2(0x50505500u,0x8001u),vec2(0x00555050u,0x0005u),vec2(0x15151010u,0x0005u),vec2(0x54540404u,0x8001u)
);
const BC7_PARTITION_SETS_3=array<vec2<u32>,64>(
 vec2(0xaa685050u,0x8009u),vec2(0x6a5a5040u,0x0109u),vec2(0x5a5a4200u,0x8101u),vec2(0x5450a0a8u,0x8009u),
 vec2(0xa5a50000u,0x8101u),vec2(0xa0a05050u,0x8009u),vec2(0x5555a0a0u,0x8009u),vec2(0x5a5a5050u,0x8101u),
 vec2(0xaa550000u,0x8101u),vec2(0xaa555500u,0x8101u),vec2(0xaaaa5500u,0x8041u),vec2(0x90909090u,0x8041u),
 vec2(0x94949494u,0x8041u),vec2(0xa4a4a4a4u,0x8021u),vec2(0xa9a59450u,0x8009u),vec2(0x2a0a4250u,0x0109u),
 vec2(0xa5945040u,0x8009u),vec2(0x0a425054u,0x0109u),vec2(0xa5a5a500u,0x8101u),vec2(0x55a0a0a0u,0x8009u),
 vec2(0xa8a85454u,0x8009u),vec2(0x6a6a4040u,0x0109u),vec2(0xa4a45000u,0x8041u),vec2(0x1a1a0500u,0x0501u),
 vec2(0x0050a4a4u,0x0029u),vec2(0xaaa59090u,0x8101u),vec2(0x14696914u,0x0141u),vec2(0x69691400u,0x0441u),
 vec2(0xa08585a0u,0x8101u),vec2(0xaa821414u,0x8021u),vec2(0x50a4a450u,0x8401u),vec2(0x6a5a0200u,0x8101u),
 vec2(0xa9a58000u,0x8101u),vec2(0x5090a0a8u,0x8009u),vec2(0xa8a09050u,0x8009u),vec2(0x24242424u,0x0421u),
 vec2(0x00aa5500u,0x0441u),vec2(0x24924924u,0x0501u),vec2(0x24499224u,0x0301u),vec2(0x50a50a50u,0x8401u),
 vec2(0x500aa550u,0x8041u),vec2(0xaaaa4444u,0x8009u),vec2(0x66660000u,0x8101u),vec2(0xa5a0a5a0u,0x8021u),
 vec2(0x50a050a0u,0x8009u),vec2(0x69286928u,0x8041u),vec2(0x44aaaa44u,0x8041u),vec2(0x66666600u,0x8101u),
 vec2(0xaa444444u,0x8009u),vec2(0x54a854a8u,0x8009u),vec2(0x95809580u,0x8021u),vec2(0x96969600u,0x8021u),
 vec2(0xa85454a8u,0x8021u),vec2(0x80959580u,0x8101u),vec2(0xaa141414u,0x8021u),vec2(0x96960000u,0x8401u),
 vec2(0xaaaa1414u,0x8021u),vec2(0xa05050a0u,0x8401u),vec2(0xa0a5a5a0u,0x8101u),vec2(0x96000000u,0xa001u),
 vec2(0x40804080u,0x8009u),vec2(0xa9a8a9a8u,0x9001u),vec2(0xaaaaaa44u,0x8009u),vec2(0x2a4a5254u,0x0109u)
);
// `count` bits of `block` from bit `first`, lowest first, as bcdec's
// bitstream reads them there.
fn bc7_read_bits(block:vec4<u32>,first:u32,count:u32)->u32 {
 let word=first/32u;
 let shift=first%32u;
 var bits=block[word]>>shift;
 if shift+count>32u {
  bits|=block[word+1u]<<(32u-shift);
 }
 return extractBits(bits,0u,count);
}
fn bc7_weight(index_bits:u32,index:u32)->u32 {
 if index_bits==2u {
  return BC7_WEIGHTS_2[index];
 }
 if index_bits==3u {
  return BC7_WEIGHTS_3[index];
 }
 return BC7_WEIGHTS_4[index];
}
// bcdec__interpolate, for each channel of two endpoints.
fn bc7_interpolate(a:vec4<u32>,b:vec4<u32>,weight:u32)->vec4<u32> {
 return (a*(64u-weight)+b*weight+vec4(32u))>>vec4(6u);
}
// An endpoint's channels widened to 8 bits: its MSBs replicated into the
// LSBs below them, from `bits` bits.
fn bc7_unquantize(endpoint:vec4<u32>,bits:vec4<u32>)->vec4<u32> {
 let shifted=endpoint<<(vec4(8u)-bits);
 return shifted|(shifted>>bits);
}
fn bc7_texel(block:vec4<u32>,texel:u32)->vec4<f32> {
 let mode=countTrailingZeros(block.x&0xffu);
 // unexpected mode, clear the block (transparent black)
 if mode>=8u {
  return vec4(0.);
 }
 var at=mode+1u;
 var partition_index=0u;
 var num_partitions=1u;
 var rotation=0u;
 var index_selection_bit=0u;
 if mode==0u||mode==1u||mode==2u||mode==3u||mode==7u {
  num_partitions=select(2u,3u,mode==0u||mode==2u);
  let partition_bits=select(6u,4u,mode==0u);
  partition_index=bc7_read_bits(block,at,partition_bits);
  at+=partition_bits;
 }
 let num_endpoints=num_partitions*2u;
 if mode==4u||mode==5u {
  rotation=bc7_read_bits(block,at,2u);
  at+=2u;
  if mode==4u {
   index_selection_bit=bc7_read_bits(block,at,1u);
   at+=1u;
  }
 }
 // The texel's subset and the fix-up texels; texel 0 is always one.
 var partition_set=vec2(0u,1u);
 if num_partitions==2u {
  partition_set=BC7_PARTITION_SETS_2[partition_index];
 } else if num_partitions==3u {
  partition_set=BC7_PARTITION_SETS_3[partition_index];
 }
 let subset=extractBits(partition_set.x,texel*2u,2u);
 // Extract the subset's endpoints: each RGB channel's endpoints, then
 // alpha's (if any).
 let color_bits=BC7_COLOR_BITS[mode];
 let alpha_bits=BC7_ALPHA_BITS[mode];
 var endpoint_0=vec4(0u);
 var endpoint_1=vec4(0u);
 for(var channel=0u;channel<3u;channel++) {
  let channel_at=at+(channel*num_endpoints+subset*2u)*color_bits;
  endpoint_0[channel]=bc7_read_bits(block,channel_at,color_bits);
  endpoint_1[channel]=bc7_read_bits(block,channel_at+color_bits,color_bits);
 }
 at+=3u*num_endpoints*color_bits;
 endpoint_0.a=bc7_read_bits(block,at+subset*2u*alpha_bits,alpha_bits);
 endpoint_1.a=bc7_read_bits(block,at+(subset*2u+1u)*alpha_bits,alpha_bits);
 at+=num_endpoints*alpha_bits;
 // Fully decode endpoints: first the modes that have P-bits.
 let has_p_bits=(BC7_MODE_HAS_P_BITS>>mode)&1u;
 if has_p_bits!=0u {
  endpoint_0<<=vec4(1u);
  endpoint_1<<=vec4(1u);
  if mode==1u {
   // A P-bit shared by the subset's endpoints, in RGB.
   let p_bit=bc7_read_bits(block,at+subset,1u);
   endpoint_0|=vec4(vec3(p_bit),0u);
   endpoint_1|=vec4(vec3(p_bit),0u);
   at+=2u;
  } else {
   // A P-bit per endpoint.
   endpoint_0|=vec4(bc7_read_bits(block,at+subset*2u,1u));
   endpoint_1|=vec4(bc7_read_bits(block,at+subset*2u+1u,1u));
   at+=num_endpoints;
  }
 }
 let endpoint_bits=vec4(vec3(color_bits+has_p_bits),alpha_bits+has_p_bits);
 endpoint_0=bc7_unquantize(endpoint_0,endpoint_bits);
 endpoint_1=bc7_unquantize(endpoint_1,endpoint_bits);
 // If this mode does not explicitly define the alpha component, set alpha
 // equal to 1.0.
 if alpha_bits==0u {
  endpoint_0.a=0xffu;
  endpoint_1.a=0xffu;
 }
 let index_bits=select(select(2u,4u,mode==6u),3u,mode==0u||mode==1u);
 let index_bits2=select(select(0u,2u,mode==5u),3u,mode==4u);
 // The texel's index follows every texel's before it, a fix-up texel's
 // with one bit fewer.
 let fix_ups_before=countOneBits(extractBits(partition_set.y,0u,texel));
 let fix_up=extractBits(partition_set.y,texel,1u);
 let index=bc7_read_bits(block,at+texel*index_bits-fix_ups_before,index_bits-fix_up);
 var color:vec4<u32>;
 if index_bits2==0u {
  color=bc7_interpolate(endpoint_0,endpoint_1,bc7_weight(index_bits,index));
 } else {
  // The secondary indices follow the primary ones, texel 0's alone with
  // one bit fewer.
  at+=16u*index_bits-1u;
  let after_first=select(1u,0u,texel==0u);
  let index2=bc7_read_bits(block,at+texel*index_bits2-after_first,index_bits2-1u+after_first);
  // The colour index comes from the secondary index bits if the mode has
  // an index selection bit and its value is one, and from the primary
  // index bits otherwise; alpha takes the other.
  let primary=bc7_interpolate(endpoint_0,endpoint_1,bc7_weight(index_bits,index));
  let secondary=bc7_interpolate(endpoint_0,endpoint_1,bc7_weight(index_bits2,index2));
  if index_selection_bit==0u {
   color=vec4(primary.rgb,secondary.a);
  } else {
   color=vec4(secondary.rgb,primary.a);
  }
 }
 switch rotation {
  // Scalar(R) Vector(AGB): swap A and R.
  case 1u: {
   color=color.agbr;
  }
  // Scalar(G) Vector(RAB): swap A and G.
  case 2u: {
   color=color.rabg;
  }
  // Scalar(B) Vector(RGA): swap A and B.
  case 3u: {
   color=color.rgab;
  }
  default: {
  }
 }
 return vec4<f32>(color)/255.;
}
