//! The 3D simplex and its solvers.
//!
//! Ported from `Box3D` v0.1.0 (commit
//! `8441b4a06d6d09dcfb0b0f704df4d847d1437b92`), `src/distance.c`
//! (`b3BarycentricCoordsEdge`, `b3BarycentricCoordsTri`,
//! `b3BarycentricCoordsTet`, `b3SolveSimplex2`, `b3SolveSimplex3`,
//! `b3SolveSimplex4` and `b3ComputeWitnessPoints`). Copyright 2026 Erin
//! Catto, MIT; the source credits Dirk Gregorius with portions of the file.
//! `LICENSE-box3d.txt` is beside this crate's manifest.
//!
//! A solver finds the part of the simplex nearest the origin (a vertex, an
//! edge, a face or the inside) by Voronoi regions: the origin belongs to a
//! feature when its barycentric weights on that feature are positive and
//! the weights that would move it onto a neighbouring, larger feature are
//! not. It then keeps only that feature's vertices and gives each its
//! weight in the closest point. Weights come unnormalised with their
//! divisor last, so only the feature chosen pays for a division.

use sgl_core::math::Vec3;

use super::{cross, dot};

/// One vertex of the simplex (`b3SimplexVertex`).
#[derive(Clone, Copy)]
pub(super) struct SimplexVertex {
    /// Support point on A.
    w_a: Vec3,
    /// Support point on B.
    w_b: Vec3,
    /// `w_b - w_a`.
    pub w: Vec3,
    /// Barycentric weight of this vertex in the closest point.
    a: f32,
    pub index_a: usize,
    pub index_b: usize,
}

impl SimplexVertex {
    pub(super) fn new(a: &[Vec3], b: &[Vec3], index_a: usize, index_b: usize) -> Self {
        Self {
            w_a: a[index_a],
            w_b: b[index_b],
            w: b[index_b] - a[index_a],
            a: 0.0,
            index_a,
            index_b,
        }
    }
}

/// One to four vertices of the Minkowski difference (`b3Simplex`).
#[derive(Clone, Copy)]
pub(super) struct Simplex {
    pub v: [SimplexVertex; 4],
    pub count: usize,
}

fn blend2(s: f32, a: Vec3, t: f32, b: Vec3) -> Vec3 {
    Vec3::new(s * a.x + t * b.x, s * a.y + t * b.y, s * a.z + t * b.z)
}

fn blend3(s: f32, a: Vec3, t: f32, b: Vec3, u: f32, c: Vec3) -> Vec3 {
    Vec3::new(
        s * a.x + t * b.x + u * c.x,
        s * a.y + t * b.y + u * c.y,
        s * a.z + t * b.z + u * c.z,
    )
}

/// `a · (b × c)`: six times the signed volume of the tetrahedron the three
/// make with the origin (`b3ScalarTripleProduct`).
fn scalar_triple_product(a: Vec3, b: Vec3, c: Vec3) -> f32 {
    dot(a, cross(b, c))
}

/// The origin's weights on the segment `a b`, unnormalised, then their
/// divisor (`b3BarycentricCoordsEdge`).
fn barycentric_edge(a: Vec3, b: Vec3) -> [f32; 3] {
    let ab = b - a;
    [dot(b, ab), -dot(a, ab), dot(ab, ab)]
}

/// The origin's weights on the triangle `a b c`, unnormalised, then their
/// divisor (`b3BarycentricCoordsTri`).
fn barycentric_triangle(a: Vec3, b: Vec3, c: Vec3) -> [f32; 4] {
    let ab = b - a;
    let ac = c - a;

    let b_x_c = cross(b, c);
    let c_x_a = cross(c, a);
    let a_x_b = cross(a, b);

    let ab_x_ac = cross(ab, ac);

    [
        dot(b_x_c, ab_x_ac),
        dot(c_x_a, ab_x_ac),
        dot(a_x_b, ab_x_ac),
        dot(ab_x_ac, ab_x_ac),
    ]
}

/// The origin's weights in the tetrahedron `a b c d`, unnormalised, then
/// their divisor, signed so the divisor is positive
/// (`b3BarycentricCoordsTet`).
fn barycentric_tetrahedron(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> [f32; 5] {
    let divisor = scalar_triple_product(b - a, c - a, d - a);
    let sign = if divisor < 0.0 { -1.0 } else { 1.0 };
    [
        sign * scalar_triple_product(b, c, d),
        sign * scalar_triple_product(a, d, c),
        sign * scalar_triple_product(a, b, d),
        sign * scalar_triple_product(a, c, b),
        sign * divisor,
    ]
}

impl Simplex {
    /// A simplex of one vertex.
    pub(super) fn new(vertex: SimplexVertex) -> Self {
        Self {
            v: [vertex; 4],
            count: 1,
        }
    }

    /// Adds a vertex to a simplex of fewer than four.
    pub(super) fn push(&mut self, vertex: SimplexVertex) {
        self.v[self.count] = vertex;
        self.count += 1;
    }

    /// Reduces the simplex to the feature nearest the origin and weighs its
    /// vertices. `false` when the feature has no extent to divide by, which
    /// rounding can do to a sliver: the caller keeps its last simplex.
    pub(super) fn solve(&mut self) -> bool {
        match self.count {
            1 => {
                self.v[0].a = 1.0;
                true
            }
            2 => self.solve2(),
            3 => self.solve3(),
            _ => self.solve4(),
        }
    }

    /// The point of the solved simplex nearest the origin, for one to
    /// three vertices.
    pub(super) fn closest_point(&self) -> Vec3 {
        let v = &self.v;
        match self.count {
            1 => v[0].w,
            2 => blend2(v[0].a, v[0].w, v[1].a, v[1].w),
            _ => blend3(v[0].a, v[0].w, v[1].a, v[1].w, v[2].a, v[2].w),
        }
    }

    /// The closest points on A and B for a solved simplex
    /// (`b3ComputeWitnessPoints`). A full simplex holds the origin, so the
    /// two are forced to one point.
    pub(super) fn witness_points(&self) -> (Vec3, Vec3) {
        let v = &self.v;
        match self.count {
            1 => (v[0].w_a, v[0].w_b),
            2 => (
                blend2(v[0].a, v[0].w_a, v[1].a, v[1].w_a),
                blend2(v[0].a, v[0].w_b, v[1].a, v[1].w_b),
            ),
            3 => (
                blend3(v[0].a, v[0].w_a, v[1].a, v[1].w_a, v[2].a, v[2].w_a),
                blend3(v[0].a, v[0].w_b, v[1].a, v[1].w_b, v[2].a, v[2].w_b),
            ),
            _ => {
                let sum = blend2(v[0].a, v[0].w_a, v[1].a, v[1].w_a)
                    + blend2(v[2].a, v[2].w_a, v[3].a, v[3].w_a);
                (sum, sum)
            }
        }
    }

    /// Reduces the simplex to one vertex.
    fn keep_vertex(&mut self, p: SimplexVertex) -> bool {
        self.count = 1;
        self.v[0] = p;
        self.v[0].a = 1.0;
        true
    }

    /// Reduces the simplex to the edge `p q` with the weights `w`.
    fn keep_edge(&mut self, p: SimplexVertex, q: SimplexVertex, w: [f32; 3]) -> bool {
        self.count = 2;
        self.v[0] = p;
        self.v[1] = q;

        let divisor = w[2];
        if divisor <= 0.0 {
            return false;
        }

        self.v[0].a = w[0] / divisor;
        self.v[1].a = w[1] / divisor;
        true
    }

    /// Reduces the simplex to the face `p q r` with the weights `w`.
    fn keep_face(
        &mut self,
        p: SimplexVertex,
        q: SimplexVertex,
        r: SimplexVertex,
        w: [f32; 4],
    ) -> bool {
        self.count = 3;
        self.v[0] = p;
        self.v[1] = q;
        self.v[2] = r;

        let divisor = w[3];
        if divisor <= 0.0 {
            return false;
        }

        self.v[0].a = w[0] / divisor;
        self.v[1].a = w[1] / divisor;
        self.v[2].a = w[2] / divisor;
        true
    }

    /// Solves a segment (`b3SolveSimplex2`).
    fn solve2(&mut self) -> bool {
        let a = self.v[0].w;
        let b = self.v[1].w;
        let ab = b - a;

        let divisor = dot(ab, ab);
        let u = dot(b, ab);
        let v = -dot(a, ab);

        // V( A )
        if v <= 0.0 {
            self.count = 1;
            self.v[0].a = 1.0;
            return true;
        }

        // V( B )
        if u <= 0.0 {
            self.count = 1;
            self.v[0] = self.v[1];
            self.v[0].a = 1.0;
            return true;
        }

        // Edge region
        if divisor <= 0.0 {
            return false;
        }

        // VR( AB )
        let denominator = 1.0 / divisor;
        self.v[0].a = denominator * u;
        self.v[1].a = denominator * v;
        true
    }

    /// Solves a triangle (`b3SolveSimplex3`).
    fn solve3(&mut self) -> bool {
        let [v1, v2, v3, _] = self.v;

        // Vertex regions
        let w_ab = barycentric_edge(v1.w, v2.w);
        let w_bc = barycentric_edge(v2.w, v3.w);
        let w_ca = barycentric_edge(v3.w, v1.w);

        // VR( A )
        if w_ab[1] <= 0.0 && w_ca[0] <= 0.0 {
            return self.keep_vertex(v1);
        }

        // VR( B )
        if w_bc[1] <= 0.0 && w_ab[0] <= 0.0 {
            return self.keep_vertex(v2);
        }

        // VR( C )
        if w_ca[1] <= 0.0 && w_bc[0] <= 0.0 {
            return self.keep_vertex(v3);
        }

        // Edge regions
        let w_abc = barycentric_triangle(v1.w, v2.w, v3.w);

        // VR( AB )
        if w_abc[2] <= 0.0 && w_ab[0] > 0.0 && w_ab[1] > 0.0 {
            return self.keep_edge(v1, v2, w_ab);
        }

        // VR( BC )
        if w_abc[0] <= 0.0 && w_bc[0] > 0.0 && w_bc[1] > 0.0 {
            return self.keep_edge(v2, v3, w_bc);
        }

        // VR( CA )
        if w_abc[1] <= 0.0 && w_ca[0] > 0.0 && w_ca[1] > 0.0 {
            return self.keep_edge(v3, v1, w_ca);
        }

        // Face region
        let divisor = w_abc[3];
        if divisor <= 0.0 {
            return false;
        }

        // VR( ABC )
        self.v[0].a = w_abc[0] / divisor;
        self.v[1].a = w_abc[1] / divisor;
        self.v[2].a = w_abc[2] / divisor;
        true
    }

    /// Solves a tetrahedron (`b3SolveSimplex4`).
    fn solve4(&mut self) -> bool {
        let [a, b, c, d] = self.v;

        // Vertex regions
        let w_ab = barycentric_edge(a.w, b.w);
        let w_ac = barycentric_edge(a.w, c.w);
        let w_ad = barycentric_edge(a.w, d.w);
        let w_bc = barycentric_edge(b.w, c.w);
        let w_cd = barycentric_edge(c.w, d.w);
        let w_db = barycentric_edge(d.w, b.w);

        // VR( A )
        if w_ab[1] <= 0.0 && w_ac[1] <= 0.0 && w_ad[1] <= 0.0 {
            return self.keep_vertex(a);
        }

        // VR( B )
        if w_ab[0] <= 0.0 && w_db[0] <= 0.0 && w_bc[1] <= 0.0 {
            return self.keep_vertex(b);
        }

        // VR( C )
        if w_ac[0] <= 0.0 && w_bc[0] <= 0.0 && w_cd[1] <= 0.0 {
            return self.keep_vertex(c);
        }

        // VR( D )
        if w_ad[0] <= 0.0 && w_cd[0] <= 0.0 && w_db[1] <= 0.0 {
            return self.keep_vertex(d);
        }

        // Edge regions
        let w_acb = barycentric_triangle(a.w, c.w, b.w);
        let w_abd = barycentric_triangle(a.w, b.w, d.w);
        let w_adc = barycentric_triangle(a.w, d.w, c.w);
        let w_bcd = barycentric_triangle(b.w, c.w, d.w);

        // VR( AB )
        if w_abd[2] <= 0.0 && w_acb[1] <= 0.0 && w_ab[0] > 0.0 && w_ab[1] > 0.0 {
            return self.keep_edge(a, b, w_ab);
        }

        // VR( AC )
        if w_acb[2] <= 0.0 && w_adc[1] <= 0.0 && w_ac[0] > 0.0 && w_ac[1] > 0.0 {
            return self.keep_edge(a, c, w_ac);
        }

        // VR( AD )
        if w_adc[2] <= 0.0 && w_abd[1] <= 0.0 && w_ad[0] > 0.0 && w_ad[1] > 0.0 {
            return self.keep_edge(a, d, w_ad);
        }

        // VR( BC )
        if w_acb[0] <= 0.0 && w_bcd[2] <= 0.0 && w_bc[0] > 0.0 && w_bc[1] > 0.0 {
            return self.keep_edge(b, c, w_bc);
        }

        // VR( CD )
        if w_adc[0] <= 0.0 && w_bcd[0] <= 0.0 && w_cd[0] > 0.0 && w_cd[1] > 0.0 {
            return self.keep_edge(c, d, w_cd);
        }

        // VR( DB )
        if w_abd[0] <= 0.0 && w_bcd[1] <= 0.0 && w_db[0] > 0.0 && w_db[1] > 0.0 {
            return self.keep_edge(d, b, w_db);
        }

        // Face regions
        let w_abcd = barycentric_tetrahedron(a.w, b.w, c.w, d.w);

        // VR( ACB )
        if w_abcd[3] < 0.0 && w_acb[0] > 0.0 && w_acb[1] > 0.0 && w_acb[2] > 0.0 {
            return self.keep_face(a, c, b, w_acb);
        }

        // VR( ABD )
        if w_abcd[2] < 0.0 && w_abd[0] > 0.0 && w_abd[1] > 0.0 && w_abd[2] > 0.0 {
            return self.keep_face(a, b, d, w_abd);
        }

        // VR( ADC )
        if w_abcd[1] < 0.0 && w_adc[0] > 0.0 && w_adc[1] > 0.0 && w_adc[2] > 0.0 {
            return self.keep_face(a, d, c, w_adc);
        }

        // VR( BCD )
        if w_abcd[0] < 0.0 && w_bcd[0] > 0.0 && w_bcd[1] > 0.0 && w_bcd[2] > 0.0 {
            return self.keep_face(b, c, d, w_bcd);
        }

        // Inside the tetrahedron
        let divisor = w_abcd[4];
        if divisor <= 0.0 {
            return false;
        }

        // VR( ABCD )
        self.v[0].a = w_abcd[0] / divisor;
        self.v[1].a = w_abcd[1] / divisor;
        self.v[2].a = w_abcd[2] / divisor;
        self.v[3].a = w_abcd[3] / divisor;
        true
    }
}
