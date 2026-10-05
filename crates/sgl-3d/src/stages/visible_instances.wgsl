// Diagnostic only (stages/visible_instances.rs): marks each object record
// that has a pixel in the source identity target, one bit an object.
@group(0) @binding(0) var source_identity: texture_2d<u32>;
@group(0) @binding(1) var<storage, read_write> visible: array<atomic<u32>>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(source_identity)) {
        return;
    }
    let source = textureLoad(source_identity, vec2<i32>(id.xy), 0).x;
    if source == 0u {
        return;
    }
    let object = source - 1u;
    let word = object / 32u;
    if word >= arrayLength(&visible) {
        return;
    }
    let bit = 1u << (object % 32u);
    // Most pixels find their object marked already: the load spares them
    // the contended write.
    if (atomicLoad(&visible[word]) & bit) == 0u {
        atomicOr(&visible[word], bit);
    }
}
