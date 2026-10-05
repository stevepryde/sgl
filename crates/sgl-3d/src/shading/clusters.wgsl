// A view's clusters: which scene lights and decals reach each part of it
// (view::clusters), and the lookup of the cluster that holds a point. Reads
// `view` and `clusters`. Rust mirror of ClusterGrid: shading/clusters.rs.
//
// The lookup is Bevy 9d12036's
// crates/bevy_pbr/src/render/clustered_forward.wesl (view_z_to_z_slice,
// view_fragment_cluster_index, fragment_cluster_index,
// unpack_clusterable_object_index_ranges, get_clusterable_object_id), MIT OR
// Apache-2.0 (src/LICENSE-bevy.txt), with one range for point, spot and
// rectangle lights, split into live and baked lights, then the decals, as
// Bevy's clusterable objects share one list, and the grid read from the
// bound clusters.
// A view's cluster grid: Bevy's cluster_dimensions and cluster_factors.
struct ClusterGrid {
 // Clusters along x, y and z.
 dimensions:vec3<u32>,
 // 1 for an orthographic view, 0 for a perspective one.
 orthographic:u32,
 // Clusters per pixel along x and y, then the z slicing's factors
 // (Bevy's calculate_cluster_factors).
 factors:vec4<f32>,
}
// A view's clusters. `data` holds, for each cluster in grid order, a header
// of CLUSTER_HEADER_WORDS: the offset in `data` of its first item, then its
// live lights', baked lights' and decals' counts; then the clusters' items,
// each cluster's live lights, its baked lights, then its decals. A probe
// capture's or a ray hit's grid is one cluster holding its culled lists.
struct Clusters {
 grid:ClusterGrid,
 data:array<u32>,
}
const CLUSTER_HEADER_WORDS:u32=4u;
// The most lights and decals a point shades from its cluster (AR-12): the
// top of Godot's `rendering/limits/cluster_builder/max_clustered_elements`
// range, the most elements its cluster builder holds a view
// (servers/rendering/rendering_server.cpp, revision
// ed1daf0bf001b61586d9930840f2f1394092c079; its default is 512). Lights
// first, then decals, so a cluster past it loses its last decals, then its
// last baked lights.
const CLUSTER_MOST_ITEMS:u32=8192u;
// What reaches one cluster: items `first` onwards in clusters.data, `live`
// lights that every receiver takes, then `baked` lights that only receivers
// without baked lighting take (takes_baked_lights in baked_lighting.wgsl),
// as Godot's BAKE_STATIC lights skip lightmapped instances, then `decals`.
struct ClusterRange {
 first:u32,
 live:u32,
 baked:u32,
 decals:u32,
}
fn cluster_view_z_to_z_slice(view_z:f32)->u32 {
 let grid=clusters.grid;
 var z_slice=0u;
 if grid.orthographic!=0u {
  z_slice=u32(floor((view_z-grid.factors.z)*grid.factors.w));
 } else {
  // -view_z keeps the logarithm's argument positive.
  z_slice=u32(log(-view_z)*grid.factors.z-grid.factors.w+1.);
 }
 // The last slice ends at the farthest light or decal; anything beyond is
 // in it.
 return min(z_slice,grid.dimensions.z-1u);
}
// The cluster of a point at `position` seen at the view's `pixel`.
fn cluster_index(position:vec3<f32>,pixel:vec2<f32>)->u32 {
 let grid=clusters.grid;
 let view_z=(view.view*vec4(position,1.)).z;
 let xy=vec2<u32>(floor(pixel*grid.factors.xy));
 let z_slice=cluster_view_z_to_z_slice(view_z);
 let index=(xy.y*grid.dimensions.x+xy.x)*grid.dimensions.z+z_slice;
 // Bounds the read for a point outside the grid.
 return min(index,grid.dimensions.x*grid.dimensions.y*grid.dimensions.z-1u);
}
// The lights and decals that reach a point at `position`, seen at the
// view's `pixel`: at most CLUSTER_MOST_ITEMS of them, all within
// clusters.data, so a loop over them ends whatever the header says.
fn cluster_range(position:vec3<f32>,pixel:vec2<f32>)->ClusterRange {
 let at=cluster_index(position,pixel)*CLUSTER_HEADER_WORDS;
 let length=arrayLength(&clusters.data);
 let first=min(clusters.data[at],length);
 let room=min(length-first,CLUSTER_MOST_ITEMS);
 let live=min(clusters.data[at+1u],room);
 let baked=min(clusters.data[at+2u],room-live);
 return ClusterRange(first,live,baked,min(clusters.data[at+3u],room-live-baked));
}
// The light or decal index at `at` in clusters.data.
fn cluster_item(at:u32)->u32 {
 return clusters.data[at];
}
