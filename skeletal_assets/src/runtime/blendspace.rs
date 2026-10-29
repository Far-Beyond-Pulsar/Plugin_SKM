//! Blend space solving: which samples contribute to an input, with what
//! normalised weights.
//!
//! 1D spaces interpolate between the two neighbouring samples. 2D spaces use a
//! Delaunay triangulation built once at compile time from the samples sorted by
//! `(x, y, index)` in axis-normalised coordinates, so the same asset always
//! produces the same triangles. An input outside the hull is projected onto the
//! nearest hull edge. Weights are non-negative and sum to 1.

use std::sync::Arc;

use crate::blend_space::{BlendAxis, BlendSpaceAsset, EdgePolicy, SampleLoop, SyncMode};

use super::clip::CompiledClip;

#[derive(Clone, Debug)]
pub struct CompiledSample {
    pub id: String,
    pub clip: Arc<CompiledClip>,
    pub x: f32,
    pub y: f32,
    pub rate: f32,
    pub looping: SampleLoop,
    pub phase_offset: f32,
    pub sync_group: Option<String>,
}

/// One contributing sample and its normalised weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleWeight {
    pub sample: usize,
    pub weight: f32,
}

#[derive(Clone, Debug)]
pub struct CompiledBlendSpace {
    pub id: String,
    pub x_axis: BlendAxis,
    pub y_axis: Option<BlendAxis>,
    pub samples: Vec<CompiledSample>,
    pub sync_mode: SyncMode,
    pub leader: Option<String>,
    triangles: Vec<[usize; 3]>,
    /// Sample indices ordered along the line for 1D spaces and for 2D spaces
    /// whose samples are collinear (no triangle exists).
    line: Vec<usize>,
}

impl CompiledBlendSpace {
    /// `clips` resolves each sample's sequence in sample order; a sample whose
    /// clip is missing must be dropped by the caller (and reported) before this.
    pub fn compile(asset: &BlendSpaceAsset, clips: Vec<Arc<CompiledClip>>) -> Result<Self, String> {
        if clips.len() != asset.samples.len() {
            return Err("each blend space sample needs a compiled clip".into());
        }
        let samples: Vec<CompiledSample> = asset
            .samples
            .iter()
            .zip(clips)
            .map(|(s, clip)| CompiledSample {
                id: s.id.clone(),
                clip,
                x: s.x,
                y: s.y.unwrap_or(0.0),
                rate: s.play_rate,
                looping: s.looping,
                phase_offset: s.phase_offset,
                sync_group: s.sync_group.clone(),
            })
            .collect();
        if samples.is_empty() {
            return Err("a blend space needs at least one sample".into());
        }
        let mut space = Self {
            id: asset.id.as_str().to_string(),
            x_axis: asset.x_axis.clone(),
            y_axis: asset.y_axis.clone(),
            sync_mode: asset.sync.mode,
            leader: asset.sync.leader.clone(),
            samples,
            triangles: Vec::new(),
            line: Vec::new(),
        };
        space.build();
        Ok(space)
    }

    fn normalised(&self, s: &CompiledSample) -> [f32; 2] {
        let nx = norm(s.x, &self.x_axis);
        let ny = self.y_axis.as_ref().map_or(0.0, |a| norm(s.y, a));
        [nx, ny]
    }

    fn build(&mut self) {
        let mut order: Vec<usize> = (0..self.samples.len()).collect();
        let pts: Vec<[f32; 2]> = self.samples.iter().map(|s| self.normalised(s)).collect();
        order.sort_by(|&a, &b| pts[a][0].total_cmp(&pts[b][0]).then(pts[a][1].total_cmp(&pts[b][1])).then(a.cmp(&b)));
        if self.y_axis.is_none() {
            self.line = order;
            return;
        }
        // Drop exact duplicates from the triangulation (the first index wins).
        let unique: Vec<usize> = order
            .iter()
            .copied()
            .enumerate()
            .filter(|&(n, i)| n == 0 || pts[order[n - 1]] != pts[i])
            .map(|(_, i)| i)
            .collect();
        self.triangles = delaunay(&unique, &pts);
        if self.triangles.is_empty() {
            // collinear or fewer than three points: interpolate along the line
            self.line = unique;
        }
    }

    /// Apply the axis edge policy to a raw input.
    pub fn clamp_input(&self, x: f32, y: f32) -> (f32, f32) {
        (apply_edge(x, &self.x_axis), self.y_axis.as_ref().map_or(0.0, |a| apply_edge(y, a)))
    }

    /// Contributing samples and normalised weights for an input, ordered by
    /// sample index. The result always has at least one entry.
    pub fn solve(&self, x: f32, y: f32) -> Vec<SampleWeight> {
        let (x, y) = self.clamp_input(x, y);
        let mut out = if !self.triangles.is_empty() {
            self.solve_2d(x, y)
        } else {
            self.solve_line(x, y)
        };
        out.retain(|w| w.weight > 1e-6);
        let total: f32 = out.iter().map(|w| w.weight).sum();
        if out.is_empty() || total <= 0.0 {
            return vec![SampleWeight { sample: self.line.first().copied().unwrap_or(0), weight: 1.0 }];
        }
        for w in &mut out {
            w.weight /= total;
        }
        out.sort_by_key(|w| w.sample);
        out
    }

    fn solve_line(&self, x: f32, y: f32) -> Vec<SampleWeight> {
        let one = |i: usize| vec![SampleWeight { sample: i, weight: 1.0 }];
        let line = &self.line;
        if line.len() == 1 {
            return one(line[0]);
        }
        // Parameter along the line: x for 1D; the projection onto the first-to-last
        // segment for collinear 2D samples.
        let (t_of, t_in): (Vec<f32>, f32) = if self.y_axis.is_none() {
            (line.iter().map(|&i| self.samples[i].x).collect(), x)
        } else {
            let a = self.normalised(&self.samples[line[0]]);
            let b = self.normalised(&self.samples[*line.last().unwrap()]);
            let d = [b[0] - a[0], b[1] - a[1]];
            let len2 = (d[0] * d[0] + d[1] * d[1]).max(1e-12);
            let proj = |p: [f32; 2]| ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2;
            let p = [
                norm(x, &self.x_axis),
                self.y_axis.as_ref().map_or(0.0, |ya| norm(y, ya)),
            ];
            (line.iter().map(|&i| proj(self.normalised(&self.samples[i]))).collect(), proj(p))
        };
        let wrap = self.y_axis.is_none() && self.x_axis.edge == EdgePolicy::Wrap;
        let n = line.len();
        if t_in <= t_of[0] && !wrap {
            return one(line[0]);
        }
        if t_in >= t_of[n - 1] && !wrap {
            return one(line[n - 1]);
        }
        let pair = |lo: usize, hi: usize, lo_t: f32, hi_t: f32| {
            let span = hi_t - lo_t;
            let w = if span.abs() < 1e-9 { 0.0 } else { ((t_in - lo_t) / span).clamp(0.0, 1.0) };
            vec![SampleWeight { sample: line[lo], weight: 1.0 - w }, SampleWeight { sample: line[hi], weight: w }]
        };
        if wrap && (t_in > t_of[n - 1] || t_in < t_of[0]) {
            let period = self.x_axis.span().max(1e-9);
            // between the last sample and the first one a period later
            let (lo_t, hi_t) = (t_of[n - 1], t_of[0] + period);
            let t = if t_in < t_of[0] { t_in + period } else { t_in };
            let span = hi_t - lo_t;
            let w = if span.abs() < 1e-9 { 0.0 } else { ((t - lo_t) / span).clamp(0.0, 1.0) };
            return vec![SampleWeight { sample: line[n - 1], weight: 1.0 - w }, SampleWeight { sample: line[0], weight: w }];
        }
        let hi = t_of.partition_point(|&t| t <= t_in).min(n - 1).max(1);
        pair(hi - 1, hi, t_of[hi - 1], t_of[hi])
    }

    fn solve_2d(&self, x: f32, y: f32) -> Vec<SampleWeight> {
        let ya = self.y_axis.as_ref().expect("2D space");
        let p = [norm(x, &self.x_axis), norm(y, ya)];
        let pts: Vec<[f32; 2]> = self.samples.iter().map(|s| self.normalised(s)).collect();
        let mut best: Option<(f32, Vec<SampleWeight>)> = None;
        for tri in &self.triangles {
            let [a, b, c] = *tri;
            let bary = barycentric(p, pts[a], pts[b], pts[c]);
            let inside = bary.iter().all(|&w| w >= -1e-5);
            if inside {
                let w: Vec<_> = tri.iter().zip(bary).map(|(&s, w)| SampleWeight { sample: s, weight: w.max(0.0) }).collect();
                return w;
            }
            // outside: remember the closest point on this triangle's boundary
            for (i, j) in [(0usize, 1usize), (1, 2), (2, 0)] {
                let (u, v) = (pts[tri[i]], pts[tri[j]]);
                let t = segment_param(p, u, v);
                let q = [u[0] + (v[0] - u[0]) * t, u[1] + (v[1] - u[1]) * t];
                let d = (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2);
                let better = best.as_ref().is_none_or(|(bd, _)| d < *bd - 1e-9);
                if better {
                    best = Some((
                        d,
                        vec![SampleWeight { sample: tri[i], weight: 1.0 - t }, SampleWeight { sample: tri[j], weight: t }],
                    ));
                }
            }
        }
        best.map(|(_, w)| w).unwrap_or_default()
    }

    pub fn triangles(&self) -> &[[usize; 3]] {
        &self.triangles
    }
}

fn norm(v: f32, axis: &BlendAxis) -> f32 {
    let span = axis.span();
    if span.abs() < 1e-12 { 0.0 } else { (v - axis.min) / span }
}

fn apply_edge(v: f32, axis: &BlendAxis) -> f32 {
    if !v.is_finite() {
        return axis.min;
    }
    match axis.edge {
        EdgePolicy::Clamp => v.clamp(axis.min, axis.max),
        EdgePolicy::Wrap => {
            let span = axis.span();
            if span <= 0.0 { axis.min } else { axis.min + (v - axis.min).rem_euclid(span) }
        }
    }
}

fn barycentric(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> [f32; 3] {
    let v0 = [b[0] - a[0], b[1] - a[1]];
    let v1 = [c[0] - a[0], c[1] - a[1]];
    let v2 = [p[0] - a[0], p[1] - a[1]];
    let den = v0[0] * v1[1] - v1[0] * v0[1];
    if den.abs() < 1e-12 {
        return [1.0, 0.0, 0.0];
    }
    let v = (v2[0] * v1[1] - v1[0] * v2[1]) / den;
    let w = (v0[0] * v2[1] - v2[0] * v0[1]) / den;
    [1.0 - v - w, v, w]
}

fn segment_param(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let len2 = d[0] * d[0] + d[1] * d[1];
    if len2 < 1e-12 {
        return 0.0;
    }
    (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0)
}

/// Bowyer-Watson triangulation of `indices` (already sorted), deterministic
/// because points are inserted in the given order and ties break by index.
fn delaunay(indices: &[usize], pts: &[[f32; 2]]) -> Vec<[usize; 3]> {
    if indices.len() < 3 {
        return Vec::new();
    }
    // points as f64 for the in-circle predicate
    let n = indices.len();
    let mut p: Vec<[f64; 2]> = indices.iter().map(|&i| [pts[i][0] as f64, pts[i][1] as f64]).collect();
    // collinear check
    let collinear = {
        let (a, b) = (p[0], p[n - 1]);
        p.iter().all(|c| ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() < 1e-12)
    };
    if collinear {
        return Vec::new();
    }
    let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
    for q in &p {
        for k in 0..2 {
            min[k] = min[k].min(q[k]);
            max[k] = max[k].max(q[k]);
        }
    }
    let d = ((max[0] - min[0]).max(max[1] - min[1])).max(1e-6) * 20.0;
    let mid = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
    p.push([mid[0] - d, mid[1] - d]);
    p.push([mid[0], mid[1] + d]);
    p.push([mid[0] + d, mid[1] - d]);
    let mut tris: Vec<[usize; 3]> = vec![[n, n + 1, n + 2]];
    for i in 0..n {
        let mut bad = Vec::new();
        for (t, tri) in tris.iter().enumerate() {
            if in_circumcircle(p[i], p[tri[0]], p[tri[1]], p[tri[2]]) {
                bad.push(t);
            }
        }
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for &t in &bad {
            let tri = tris[t];
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                let shared = bad.iter().any(|&o| {
                    o != t && {
                        let other = tris[o];
                        [(other[0], other[1]), (other[1], other[2]), (other[2], other[0])]
                            .iter()
                            .any(|&(c, e)| (c == a && e == b) || (c == b && e == a))
                    }
                });
                if !shared {
                    edges.push((a, b));
                }
            }
        }
        for &t in bad.iter().rev() {
            tris.remove(t);
        }
        for (a, b) in edges {
            tris.push([a, b, i]);
        }
    }
    tris.retain(|t| t.iter().all(|&v| v < n));
    // map back to sample indices and give each triangle a canonical order
    let mut out: Vec<[usize; 3]> = tris
        .into_iter()
        .map(|t| {
            let mut s = [indices[t[0]], indices[t[1]], indices[t[2]]];
            s.sort_unstable();
            s
        })
        .collect();
    out.sort_unstable();
    out
}

fn in_circumcircle(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let (ax, ay) = (a[0] - p[0], a[1] - p[1]);
    let (bx, by) = (b[0] - p[0], b[1] - p[1]);
    let (cx, cy) = (c[0] - p[0], c[1] - p[1]);
    let det = (ax * ax + ay * ay) * (bx * cy - cx * by) - (bx * bx + by * by) * (ax * cy - cx * ay)
        + (cx * cx + cy * cy) * (ax * by - bx * ay);
    let orient = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    if orient > 0.0 { det > 1e-12 } else { det < -1e-12 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend_space::{BlendAxis, BlendSample, BlendSpaceAsset};
    use crate::runtime::clip::tests::clip_asset;
    use crate::runtime::pose::tests::chain_skeleton;
    use crate::runtime::pose::RuntimeSkeleton;

    fn space(points: &[(f32, Option<f32>)], two_d: bool, edge: EdgePolicy) -> CompiledBlendSpace {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(2)).unwrap();
        let clip = Arc::new(CompiledClip::compile(&clip_asset("c", 1.0, vec![]), &skel));
        let mut asset = BlendSpaceAsset::default();
        asset.x_axis = BlendAxis { name: "X".into(), min: 0.0, max: 10.0, edge, ..BlendAxis::default() };
        if two_d {
            asset.y_axis = Some(BlendAxis { name: "Y".into(), min: 0.0, max: 10.0, ..BlendAxis::default() });
        }
        asset.samples = points.iter().map(|&(x, y)| BlendSample::new(None, x, y)).collect();
        CompiledBlendSpace::compile(&asset, points.iter().map(|_| clip.clone()).collect()).unwrap()
    }

    fn weights(w: &[SampleWeight]) -> Vec<(usize, f32)> {
        w.iter().map(|w| (w.sample, (w.weight * 1000.0).round() / 1000.0)).collect()
    }

    #[test]
    fn one_d_interpolates_clamps_and_wraps() {
        let s = space(&[(0.0, None), (5.0, None), (10.0, None)], false, EdgePolicy::Clamp);
        assert_eq!(weights(&s.solve(2.5, 0.0)), [(0, 0.5), (1, 0.5)]);
        assert_eq!(weights(&s.solve(5.0, 0.0)), [(1, 1.0)]);
        assert_eq!(weights(&s.solve(-3.0, 0.0)), [(0, 1.0)]);
        assert_eq!(weights(&s.solve(99.0, 0.0)), [(2, 1.0)]);
        // unsorted samples are ordered by position, not by index
        let u = space(&[(10.0, None), (0.0, None)], false, EdgePolicy::Clamp);
        assert_eq!(weights(&u.solve(2.5, 0.0)), [(0, 0.25), (1, 0.75)]);
        // a wrapping axis blends the last sample into the first across the seam
        let w = space(&[(2.0, None), (8.0, None)], false, EdgePolicy::Wrap);
        assert_eq!(weights(&w.solve(9.0, 0.0)), [(0, 0.25), (1, 0.75)]);
        assert_eq!(weights(&w.solve(11.0, 0.0)), [(0, 0.75), (1, 0.25)]);
    }

    #[test]
    fn two_d_weights_are_normalised_and_deterministic() {
        let pts = [(0.0, Some(0.0)), (10.0, Some(0.0)), (0.0, Some(10.0)), (10.0, Some(10.0)), (5.0, Some(5.0))];
        let s = space(&pts, true, EdgePolicy::Clamp);
        assert_eq!(s.triangles().len(), 4, "{:?}", s.triangles());
        let w = s.solve(5.0, 5.0);
        assert_eq!(weights(&w), [(4, 1.0)]);
        for (x, y) in [(1.0, 1.0), (7.0, 3.0), (9.9, 9.9), (0.0, 5.0), (3.0, 8.0)] {
            let w = s.solve(x, y);
            let total: f32 = w.iter().map(|w| w.weight).sum();
            assert!((total - 1.0).abs() < 1e-5, "({x},{y}) {w:?}");
            assert!(w.iter().all(|w| w.weight >= 0.0 && w.weight <= 1.0));
            // reconstructing the input from the weights recovers it
            let rx: f32 = w.iter().map(|w| w.weight * pts[w.sample].0).sum();
            let ry: f32 = w.iter().map(|w| w.weight * pts[w.sample].1.unwrap()).sum();
            assert!((rx - x).abs() < 1e-3 && (ry - y).abs() < 1e-3, "({x},{y}) -> ({rx},{ry})");
        }
        let again = space(&pts, true, EdgePolicy::Clamp);
        assert_eq!(s.triangles(), again.triangles());
        assert_eq!(s.solve(3.3, 4.4), again.solve(3.3, 4.4));
    }

    #[test]
    fn two_d_outside_the_hull_projects_to_the_boundary() {
        let s = space(&[(2.0, Some(2.0)), (8.0, Some(2.0)), (5.0, Some(8.0))], true, EdgePolicy::Clamp);
        let w = s.solve(5.0, 0.0);
        let total: f32 = w.iter().map(|w| w.weight).sum();
        assert!((total - 1.0).abs() < 1e-5);
        assert_eq!(weights(&w), [(0, 0.5), (1, 0.5)]);
    }

    #[test]
    fn collinear_two_d_samples_fall_back_to_the_line() {
        let s = space(&[(0.0, Some(0.0)), (5.0, Some(5.0)), (10.0, Some(10.0))], true, EdgePolicy::Clamp);
        assert!(s.triangles().is_empty());
        let w = s.solve(2.5, 2.5);
        assert_eq!(weights(&w), [(0, 0.5), (1, 0.5)]);
    }
}
