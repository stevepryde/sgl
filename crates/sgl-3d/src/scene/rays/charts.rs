//! A model's lightmap chart tables, one a mesh, which its mesh records name
//! (the architecture's Vertex encoding): the distinct chart bounds each
//! mesh's vertices name, and each vertex's 16-bit index into its mesh's
//! table, which `shading::packed_vertex` packs.
use super::RayMesh;
use crate::scene::SceneError;
use std::collections::HashMap;

/// A chart table's entry: a lightmap chart's normalized atlas bounds, min
/// then max, as `asset::Vertex::lightmap_bounds`.
pub(super) type Chart = [f32; 4];

/// A chart table entry's words.
pub(super) const CHART_WORDS: usize = std::mem::size_of::<Chart>() / 4;

/// A model's lightmap chart tables, one a mesh (`chart_tables`).
pub(super) struct ChartTables {
    /// Each mesh's table of the distinct chart bounds its vertices name, in
    /// the order they first appear, the tables one after another in mesh
    /// order. A chart that two meshes name is in both tables.
    pub entries: Vec<Chart>,
    /// The entry where each mesh's table starts.
    pub starts: Vec<usize>,
    /// Each vertex's index into its mesh's table, mesh by mesh.
    pub indices: Vec<Vec<u16>>,
}

/// `meshes`' chart tables. Refuses a mesh past 65,536 bounds, naming it, as
/// a packed vertex holds its chart's index in 16 bits.
pub(super) fn chart_tables(meshes: &[RayMesh<'_>]) -> Result<ChartTables, SceneError> {
    let mut entries = Vec::new();
    let mut starts = Vec::with_capacity(meshes.len());
    let mut index: HashMap<[u32; 4], u16> = HashMap::new();
    let mut indices = Vec::with_capacity(meshes.len());
    for (mesh_index, mesh) in meshes.iter().enumerate() {
        let start = entries.len();
        starts.push(start);
        index.clear();
        let mut mesh_charts = Vec::with_capacity(mesh.vertices.len());
        for vertex in mesh.vertices {
            let key = vertex.lightmap_bounds.map(f32::to_bits);
            let chart = match index.get(&key) {
                Some(&chart) => chart,
                None => {
                    let chart = u16::try_from(entries.len() - start)
                        .map_err(|_| SceneError::TooManyLightmapCharts { mesh: mesh_index })?;
                    entries.push(vertex.lightmap_bounds);
                    index.insert(key, chart);
                    chart
                }
            };
            mesh_charts.push(chart);
        }
        indices.push(mesh_charts);
    }
    Ok(ChartTables {
        entries,
        starts,
        indices,
    })
}
