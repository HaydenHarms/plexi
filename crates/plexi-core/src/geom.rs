//! Small 2D helpers shared by the model, the hit testers and the exporters.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Rect {
    pub const NOTHING: Rect = Rect { min: [f32::INFINITY; 2], max: [f32::NEG_INFINITY; 2] };

    pub fn from_min_size(min: [f32; 2], size: [f32; 2]) -> Self {
        Self { min, max: [min[0] + size[0], min[1] + size[1]] }
    }

    pub fn from_points(pts: impl IntoIterator<Item = [f32; 2]>) -> Self {
        let mut r = Rect::NOTHING;
        for p in pts {
            r.min[0] = r.min[0].min(p[0]);
            r.min[1] = r.min[1].min(p[1]);
            r.max[0] = r.max[0].max(p[0]);
            r.max[1] = r.max[1].max(p[1]);
        }
        r
    }

    pub fn is_empty(&self) -> bool {
        !(self.min[0] <= self.max[0] && self.min[1] <= self.max[1])
    }

    pub fn width(&self) -> f32 {
        (self.max[0] - self.min[0]).max(0.0)
    }

    pub fn height(&self) -> f32 {
        (self.max[1] - self.min[1]).max(0.0)
    }

    pub fn center(&self) -> [f32; 2] {
        [(self.min[0] + self.max[0]) * 0.5, (self.min[1] + self.max[1]) * 0.5]
    }

    pub fn expand(self, by: f32) -> Self {
        if self.is_empty() {
            return self;
        }
        Self { min: [self.min[0] - by, self.min[1] - by], max: [self.max[0] + by, self.max[1] + by] }
    }

    pub fn union(self, o: Rect) -> Self {
        Self {
            min: [self.min[0].min(o.min[0]), self.min[1].min(o.min[1])],
            max: [self.max[0].max(o.max[0]), self.max[1].max(o.max[1])],
        }
    }

    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.min[0] <= o.max[0] && o.min[0] <= self.max[0] && self.min[1] <= o.max[1] && o.min[1] <= self.max[1]
    }
}

pub fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Distance from `p` to the segment `a`-`b`.
pub fn dist_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return dist(p, a);
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0);
    dist(p, [a[0] + t * dx, a[1] + t * dy])
}

/// Even-odd point-in-polygon test.
pub fn point_in_polygon(p: [f32; 2], poly: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_distance() {
        assert_eq!(dist_to_segment([0.0, 5.0], [-10.0, 0.0], [10.0, 0.0]), 5.0);
        assert_eq!(dist_to_segment([13.0, 4.0], [-10.0, 0.0], [10.0, 0.0]), 5.0);
    }

    #[test]
    fn polygon_contains() {
        let sq = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert!(point_in_polygon([5.0, 5.0], &sq));
        assert!(!point_in_polygon([15.0, 5.0], &sq));
    }

    #[test]
    fn rect_union_and_expand() {
        let r = Rect::from_points([[0.0, 0.0], [2.0, 3.0]]).union(Rect::from_min_size([5.0, 5.0], [1.0, 1.0]));
        assert_eq!(r.max, [6.0, 6.0]);
        assert!(Rect::NOTHING.is_empty());
        assert!(Rect::NOTHING.expand(3.0).is_empty());
    }
}
