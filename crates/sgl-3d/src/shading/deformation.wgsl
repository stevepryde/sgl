// Deformation's records in the scene source (scene::deformation), in words.
// A skinned vertex's influences: its four joints, then their weights.
const INFLUENCE_WORDS:u32=8u;
const INFLUENCE_WEIGHTS:u32=4u;
// A morph target's displacement of one vertex: position, normal, tangent.
const MORPH_DELTA_WORDS:u32=9u;
const MORPH_DELTA_NORMAL:u32=3u;
const MORPH_DELTA_TANGENT:u32=6u;
// A joint matrix, column-major.
const JOINT_WORDS:u32=16u;
// A deforming instance's vertex: its position, in one of two slots so that
// the last submitted frame's stays for motion; and its normal and tangent
// with handedness, in one.
const DEFORMED_POSITION_WORDS:u32=3u;
const DEFORMED_NORMAL_WORDS:u32=7u;
const DEFORMED_TANGENT:u32=3u;
// One mesh of one deforming instance for the deform stage: the mesh's vertex
// records, influences (zero when unskinned) and morph targets (their weight
// indices, then each target's displacement of every vertex), the instance's
// joint matrices and morph weights, and where its deformed positions and
// normals of the mesh go.
struct DeformDispatch {
 vertices:u32,
 vertex_count:u32,
 influences:u32,
 morph_targets:u32,
 morph_target_count:u32,
 joints:u32,
 weights:u32,
 positions:u32,
 normals:u32,
}
