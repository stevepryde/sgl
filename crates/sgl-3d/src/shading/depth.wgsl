// Linear view depth in metres of a device depth `z` under `perspective`'s
// infinite reversed-Z projection with near plane `near`: very far where
// nothing was drawn (depth 0). The mapping is its own inverse.
fn linear_depth(near:f32,z:f32)->f32 {
 return near/max(z,1e-30);
}
