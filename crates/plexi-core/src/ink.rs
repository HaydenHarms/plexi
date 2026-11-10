//! Ink geometry: input filtering, the pressure-to-width curve, the shape of a pen stroke,
//! simplification, hit testing and erasing.
//!
//! A pen stroke is the union of a disc at every sample (radius from pressure) and the
//! tangent hull between each pair of neighbouring discs. That shape follows the pen
//! exactly, never overshoots corners and needs no smoothing pass after the fact, so what
//! you see while drawing is what gets saved and exported.

use crate::geom::{dist, dist_to_segment};
use crate::model::{InkKind, InkPoint, Stroke};

/// How much pressure thins the line: 0 ignores pressure, 1 lets light touches vanish.
pub const THINNING: f32 = 0.6;

/// Radius of a pen stroke at a sample with pressure `p`.
pub fn radius(width: f32, p: f32) -> f32 {
    (width * 0.5 * (1.0 - THINNING + 2.0 * THINNING * p.clamp(0.0, 1.0))).max(0.3)
}

/// Radius of `stroke` at point `i`: pressure-scaled for pens, constant for highlighters.
pub fn point_radius(stroke: &Stroke, i: usize) -> f32 {
    match stroke.kind {
        InkKind::Pen => radius(stroke.width, stroke.points[i].p),
        InkKind::Highlighter => stroke.width * 0.5,
    }
}

/// The One Euro filter (Casiez et al. 2012): smooths jitter at low speed and stays close
/// behind the pen at high speed, so fast writing does not lag or round off.
#[derive(Clone, Debug)]
pub struct OneEuro {
    min_cutoff: f32,
    beta: f32,
    d_cutoff: f32,
    prev: Option<(f64, [f32; 2], [f32; 2])>,
}

impl OneEuro {
    /// `smoothing` in 0..=1. Zero disables filtering entirely.
    pub fn new(smoothing: f32) -> Self {
        let s = smoothing.clamp(0.0, 1.0);
        let inv = 1.0 - s;
        Self { min_cutoff: 0.5 + 14.0 * inv * inv, beta: 0.03 * inv + 0.004, d_cutoff: 1.0, prev: None }
            .disabled_if(s <= 0.001)
    }

    fn disabled_if(mut self, off: bool) -> Self {
        if off {
            self.min_cutoff = f32::INFINITY;
        }
        self
    }

    fn alpha(cutoff: f32, dt: f32) -> f32 {
        if !cutoff.is_finite() {
            return 1.0;
        }
        let tau = 1.0 / (std::f32::consts::TAU * cutoff);
        1.0 / (1.0 + tau / dt)
    }

    /// Filter a sample taken at time `t` (seconds).
    pub fn filter(&mut self, t: f64, x: [f32; 2]) -> [f32; 2] {
        let Some((pt, px, pdx)) = self.prev else {
            self.prev = Some((t, x, [0.0, 0.0]));
            return x;
        };
        let dt = ((t - pt) as f32).max(1.0 / 2000.0);
        let raw_dx = [(x[0] - px[0]) / dt, (x[1] - px[1]) / dt];
        let ad = Self::alpha(self.d_cutoff, dt);
        let dx = [ad * raw_dx[0] + (1.0 - ad) * pdx[0], ad * raw_dx[1] + (1.0 - ad) * pdx[1]];
        let speed = (dx[0] * dx[0] + dx[1] * dx[1]).sqrt();
        let a = Self::alpha(self.min_cutoff + self.beta * speed, dt);
        let out = [a * x[0] + (1.0 - a) * px[0], a * x[1] + (1.0 - a) * px[1]];
        self.prev = Some((t, out, dx));
        out
    }
}

/// Collects filtered samples into a stroke while the pen is down.
#[derive(Clone, Debug)]
pub struct StrokeBuilder {
    pub stroke: Stroke,
    filter: OneEuro,
    pressure: Option<f32>,
    min_step: f32,
}

impl StrokeBuilder {
    /// `min_step` is the smallest distance (page units) between kept samples; pass about
    /// half a screen pixel converted to page units.
    pub fn new(stroke: Stroke, smoothing: f32, min_step: f32) -> Self {
        Self { stroke, filter: OneEuro::new(smoothing), pressure: None, min_step }
    }

    pub fn push(&mut self, t: f64, pos: [f32; 2], pressure: f32) {
        let pos = self.filter.filter(t, pos);
        // Light pressure smoothing: tablets report pressure in coarse steps.
        let p = match self.pressure {
            Some(prev) => prev * 0.4 + pressure * 0.6,
            None => pressure,
        };
        self.pressure = Some(p);
        if let Some(last) = self.stroke.points.last_mut()
            && dist(last.pos(), pos) < self.min_step
        {
            last.p = last.p.max(p);
            return;
        }
        self.stroke.points.push(InkPoint::new(pos[0], pos[1], p));
    }

    /// Finish the stroke, dropping redundant samples.
    pub fn finish(mut self) -> Stroke {
        let tol = (self.min_step * 0.5).max(0.05);
        self.stroke.points = simplify(&self.stroke.points, tol);
        self.stroke
    }
}

/// Douglas-Peucker simplification that also keeps samples where pressure changes.
pub fn simplify(pts: &[InkPoint], tol: f32) -> Vec<InkPoint> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (pa, pb) = (pts[a], pts[b]);
        let mut worst = (0.0f32, 0usize);
        for (i, q) in pts.iter().enumerate().take(b).skip(a + 1) {
            let d = dist_to_segment(q.pos(), pa.pos(), pb.pos());
            let t = (i - a) as f32 / (b - a) as f32;
            let dp = (q.p - (pa.p + (pb.p - pa.p) * t)).abs();
            let score = d.max(if dp > 0.06 { tol * 2.0 } else { 0.0 });
            if score > worst.0 {
                worst = (score, i);
            }
        }
        if worst.0 > tol {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// One convex piece of a pen stroke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PenPiece {
    Disc {
        c: [f32; 2],
        r: f32,
    },
    /// Tangent hull between two discs, wound clockwise on screen (positive shoelace
    /// area with y pointing down).
    Hull([[f32; 2]; 4]),
}

/// External tangent quad between two discs, or `None` if one disc contains the other.
pub fn tangent_hull(p0: [f32; 2], r0: f32, p1: [f32; 2], r1: f32) -> Option<[[f32; 2]; 4]> {
    let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= (r0 - r1).abs() + 1e-4 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let along = (r0 - r1) / len;
    let across = (1.0 - along * along).max(0.0).sqrt();
    let n = |s: f32| [ux * along - uy * across * s, uy * along + ux * across * s];
    let (na, nb) = (n(1.0), n(-1.0));
    let mut quad = [
        [p0[0] + na[0] * r0, p0[1] + na[1] * r0],
        [p1[0] + na[0] * r1, p1[1] + na[1] * r1],
        [p1[0] + nb[0] * r1, p1[1] + nb[1] * r1],
        [p0[0] + nb[0] * r0, p0[1] + nb[1] * r0],
    ];
    if shoelace(&quad) < 0.0 {
        quad.reverse();
    }
    Some(quad)
}

fn shoelace(poly: &[[f32; 2]]) -> f32 {
    let n = poly.len();
    (0..n).map(|i| poly[i][0] * poly[(i + 1) % n][1] - poly[(i + 1) % n][0] * poly[i][1]).sum::<f32>() * 0.5
}

/// Decompose a pen stroke into discs and hulls. Discs fully inside a neighbour's hull
/// are still emitted; the pieces are meant to be filled as a union.
pub fn pen_pieces(stroke: &Stroke) -> Vec<PenPiece> {
    let pts = &stroke.points;
    let mut out = Vec::with_capacity(pts.len() * 2);
    for i in 0..pts.len() {
        let r = point_radius(stroke, i);
        out.push(PenPiece::Disc { c: pts[i].pos(), r });
        if i + 1 < pts.len() {
            let r1 = point_radius(stroke, i + 1);
            if let Some(q) = tangent_hull(pts[i].pos(), r, pts[i + 1].pos(), r1) {
                out.push(PenPiece::Hull(q));
            }
        }
    }
    out
}

/// A triangle mesh in page units.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// Triangulate a pen stroke. `px_per_unit` picks how many segments each disc gets so
/// curves stay round at the zoom level the mesh will be drawn at.
pub fn pen_mesh(stroke: &Stroke, px_per_unit: f32) -> Mesh {
    let mut m = Mesh::default();
    for piece in pen_pieces(stroke) {
        match piece {
            PenPiece::Disc { c, r } => {
                let segs = ((r * px_per_unit).sqrt() * 4.0).clamp(8.0, 64.0) as u32;
                let base = m.vertices.len() as u32;
                m.vertices.push(c);
                for k in 0..segs {
                    let a = k as f32 / segs as f32 * std::f32::consts::TAU;
                    m.vertices.push([c[0] + r * a.cos(), c[1] + r * a.sin()]);
                }
                for k in 0..segs {
                    m.indices.extend([base, base + 1 + k, base + 1 + (k + 1) % segs]);
                }
            }
            PenPiece::Hull(q) => {
                let base = m.vertices.len() as u32;
                m.vertices.extend(q);
                m.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
    }
    m
}

/// Triangulate a highlighter stroke as one non-overlapping band so its translucency
/// stays even where the line crosses its own joints.
pub fn highlighter_mesh(stroke: &Stroke, px_per_unit: f32) -> Mesh {
    use lyon_tessellation::math::point;
    use lyon_tessellation::path::Path;
    use lyon_tessellation::{
        BuffersBuilder, LineCap, LineJoin, StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers,
    };

    let mut m = Mesh::default();
    let Some(first) = stroke.points.first() else { return m };
    let mut b = Path::builder();
    b.begin(point(first.x, first.y));
    for p in &stroke.points[1..] {
        b.line_to(point(p.x, p.y));
    }
    b.end(false);
    let path = b.build();
    let mut geom: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let opts = StrokeOptions::tolerance((0.25 / px_per_unit).max(0.01))
        .with_line_width(stroke.width)
        .with_line_cap(LineCap::Round)
        .with_line_join(LineJoin::Round);
    let built = StrokeTessellator::new().tessellate_path(
        &path,
        &opts,
        &mut BuffersBuilder::new(&mut geom, |v: StrokeVertex| v.position().to_array()),
    );
    if built.is_ok() {
        m.vertices = geom.vertices;
        m.indices = geom.indices;
    }
    m
}

pub fn stroke_mesh(stroke: &Stroke, px_per_unit: f32) -> Mesh {
    match stroke.kind {
        InkKind::Pen => pen_mesh(stroke, px_per_unit),
        InkKind::Highlighter => highlighter_mesh(stroke, px_per_unit),
    }
}

/// Does a circle of `radius` at `c` touch the stroke?
pub fn stroke_hit(stroke: &Stroke, c: [f32; 2], radius: f32) -> bool {
    let pts = &stroke.points;
    match pts.len() {
        0 => false,
        1 => dist(pts[0].pos(), c) <= radius + point_radius(stroke, 0),
        _ => (0..pts.len() - 1).any(|i| {
            let r = point_radius(stroke, i).max(point_radius(stroke, i + 1));
            dist_to_segment(c, pts[i].pos(), pts[i + 1].pos()) <= radius + r
        }),
    }
}

/// Insert samples so no two neighbours are more than `step` apart.
pub fn densify(pts: &[InkPoint], step: f32) -> Vec<InkPoint> {
    let mut out = Vec::with_capacity(pts.len());
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            let q = pts[i - 1];
            let n = (dist(q.pos(), p.pos()) / step).floor() as usize;
            for k in 1..=n {
                let t = k as f32 / (n + 1) as f32;
                out.push(InkPoint::new(q.x + (p.x - q.x) * t, q.y + (p.y - q.y) * t, q.p + (p.p - q.p) * t));
            }
        }
        out.push(*p);
    }
    out
}

/// Erase the part of `stroke` under a circle. Returns `None` if nothing was touched,
/// otherwise the remaining pieces (possibly none).
pub fn erase_partial(stroke: &Stroke, c: [f32; 2], radius: f32) -> Option<Vec<Stroke>> {
    if !stroke_hit(stroke, c, radius) {
        return None;
    }
    let pts = densify(&stroke.points, (radius * 0.5).max(1.0));
    let mut pieces = Vec::new();
    let mut run: Vec<InkPoint> = Vec::new();
    let flush = |run: &mut Vec<InkPoint>, pieces: &mut Vec<Stroke>| {
        if run.len() >= 2 {
            pieces.push(Stroke {
                id: crate::model::new_id(),
                kind: stroke.kind,
                color: stroke.color,
                width: stroke.width,
                points: simplify(run, 0.1),
                group: stroke.group,
                locked: false,
            });
        }
        run.clear();
    };
    for p in pts {
        if dist(p.pos(), c) <= radius {
            flush(&mut run, &mut pieces);
        } else {
            run.push(p);
        }
    }
    flush(&mut run, &mut pieces);
    Some(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Color;

    fn line(n: usize) -> Stroke {
        Stroke {
            id: 1,
            kind: InkKind::Pen,
            color: Color::BLACK,
            width: 4.0,
            points: (0..n).map(|i| InkPoint::new(i as f32 * 10.0, 0.0, 0.5)).collect(),
            group: 0,
            locked: false,
        }
    }

    #[test]
    fn half_pressure_gives_nominal_width() {
        assert!((radius(4.0, 0.5) - 2.0).abs() < 1e-6);
        assert!(radius(4.0, 1.0) > radius(4.0, 0.0));
    }

    #[test]
    fn hull_is_clockwise_and_tangent() {
        let q = tangent_hull([0.0, 0.0], 2.0, [10.0, 0.0], 2.0).unwrap();
        assert!(shoelace(&q) > 0.0);
        for v in q {
            assert!((v[1].abs() - 2.0).abs() < 1e-4);
        }
        assert!(tangent_hull([0.0, 0.0], 5.0, [1.0, 0.0], 1.0).is_none());
    }

    #[test]
    fn unfiltered_input_passes_through() {
        let mut f = OneEuro::new(0.0);
        f.filter(0.0, [0.0, 0.0]);
        assert_eq!(f.filter(0.01, [5.0, 5.0]), [5.0, 5.0]);
    }

    #[test]
    fn filter_reduces_jitter() {
        let mut f = OneEuro::new(0.8);
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for i in 0..400 {
            let jitter = if i % 2 == 0 { 1.0 } else { -1.0 };
            let out = f.filter(i as f64 / 200.0, [0.0, jitter]);
            if i > 200 {
                lo = lo.min(out[1]);
                hi = hi.max(out[1]);
            }
        }
        // Input swings by 2; the output should barely move.
        assert!(hi - lo < 0.3, "{lo}..{hi}");
    }

    #[test]
    fn simplify_drops_collinear_points() {
        let s = simplify(&line(20).points, 0.1);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn erasing_the_middle_splits_the_stroke() {
        let s = line(11); // 0..100 along x
        let pieces = erase_partial(&s, [50.0, 0.0], 8.0).unwrap();
        assert_eq!(pieces.len(), 2);
        assert!(erase_partial(&s, [50.0, 50.0], 8.0).is_none());
    }

    #[test]
    fn meshes_are_nonempty() {
        let s = line(3);
        assert!(!pen_mesh(&s, 1.0).indices.is_empty());
        let mut h = line(3);
        h.kind = InkKind::Highlighter;
        assert!(!highlighter_mesh(&h, 1.0).indices.is_empty());
    }
}
