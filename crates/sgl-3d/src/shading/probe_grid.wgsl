// Environment specular for receivers without culled tiles (probe captures
// and ray hits): the probes listed in the receiver's cell of the collection's
// world grid (scene/probe_grid.rs), as Wicked Engine's surfel_raytraceCS.hlsl
// reads a ray hit's SurfelGridCell. A cell lists, in ascending order, every
// probe whose influence may reach it, so its walk adds what walking every
// probe in order adds. Reads `collection`, whose `grid` holds it, and `baked`.

// Each cell's record in `collection.grid`, in words (scene/probe_grid.rs
// GridCell): its list's first index into the grid, and its probe count.
// Records run x fastest, then y, then z; the lists follow them.
const PROBE_GRID_CELL_WORDS:u32=2u;
const PROBE_GRID_CELL_FIRST:u32=0u;
const PROBE_GRID_CELL_COUNT:u32=1u;
// The receiver's cell record: its first word in the grid. None (-1)
// outside the grid.
fn probe_grid_cell(world:vec3<f32>)->i32 {
 let size=collection.grid_size;
 let cell=floor((world-collection.grid_origin)*collection.grid_scale);
 if !(all(cell>=vec3(0.)) && all(cell<vec3<f32>(size))) {
  return -1;
 }
 let c=vec3<u32>(cell);
 return i32((c.x+size.x*(c.y+size.y*c.z))*PROBE_GRID_CELL_WORDS);
}
fn collection_environment(world:vec3<f32>,direction:vec3<f32>,rough:f32,scale:f32,sky:texture_2d_array<f32>,filter_sampler:sampler,rotation:f32,strength:f32)->EnvironmentSpecular {
 var sum=ProbeSum(vec3(0.),0.);
 let cell=probe_grid_cell(world);
 if cell>=0 {
  let record=u32(cell);
  let first=collection.grid[record+PROBE_GRID_CELL_FIRST];
  // The cell's probes lie within the grid, so the loop ends whatever the
  // cell says.
  let length=arrayLength(&collection.grid);
  let count=min(collection.grid[record+PROBE_GRID_CELL_COUNT],select(0u,length-first,first<length));
  for(var i=0u;i<count;i++) {
   sum=collection_add(sum,collection.grid[first+i],world,direction,rough,filter_sampler);
  }
 }
 return collection_resolve(sum,direction,rough,scale,sky,filter_sampler,rotation,strength);
}
