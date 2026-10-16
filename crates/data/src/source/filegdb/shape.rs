//! FileGDB geometry blobs: Esri shapes with their coordinates stored as integers (varints) at the
//! precision the geometry field gives, each point after the first a delta from the one before.
//!
//! Z and M values are skipped (RustGIS keeps two dimensions). True curves (circular arcs and
//! Bézier curves) are turned into short straight segments; elliptic arcs are drawn straight.

use super::table::{Bytes, GeomField};
use crate::Geometry;
use crate::io::group_rings;
use rustgis_geom::Coord;

/// What a blob holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    Point,
    MultiPoint,
    Line,
    Polygon,
}

/// The shape type's base, and whether its points carry Z values, M values and curves.
fn kind(code: u64) -> Option<(Base, bool, bool, bool)> {
    let flags_z = code & 0x8000_0000 != 0;
    let flags_m = code & 0x4000_0000 != 0;
    let curves = code & 0x2000_0000 != 0;
    let (base, z, m) = match code & 0xff {
        1 => (Base::Point, false, false),
        9 => (Base::Point, true, false),
        11 => (Base::Point, true, true),
        21 => (Base::Point, false, true),
        52 => (Base::Point, flags_z, flags_m),
        8 => (Base::MultiPoint, false, false),
        20 => (Base::MultiPoint, true, false),
        18 => (Base::MultiPoint, true, true),
        28 => (Base::MultiPoint, false, true),
        53 => (Base::MultiPoint, flags_z, flags_m),
        3 => (Base::Line, false, false),
        10 => (Base::Line, true, false),
        13 => (Base::Line, true, true),
        23 => (Base::Line, false, true),
        50 => (Base::Line, flags_z, flags_m),
        5 => (Base::Polygon, false, false),
        19 => (Base::Polygon, true, false),
        15 => (Base::Polygon, true, true),
        25 => (Base::Polygon, false, true),
        51 => (Base::Polygon, flags_z, flags_m),
        _ => return None,
    };
    Some((base, z, m, curves))
}

/// A blob's bounding box (min x, min y, max x, max y) in the table's coordinates, from its
/// header alone; `None` for an empty or unreadable shape.
pub(super) fn bbox(blob: &[u8], g: &GeomField) -> Option<[f64; 4]> {
    let mut r = Bytes::new(blob);
    let (base, ..) = kind(r.varuint()?)?;
    let s = g.xy_scale;
    match base {
        Base::Point => {
            let (x, y) = (r.varuint()?, r.varuint()?);
            if x == 0 {
                return None;
            }
            let x = (x - 1) as f64 / s + g.x_origin;
            let y = y.checked_sub(1)? as f64 / s + g.y_origin;
            Some([x, y, x, y])
        }
        Base::MultiPoint | Base::Line | Base::Polygon => {
            let n = r.varuint()?;
            if n == 0 {
                return None;
            }
            if base != Base::MultiPoint {
                let _parts = r.varuint()?;
                if has_curves(blob) {
                    let _curves = r.varuint()?;
                }
            }
            let x0 = r.varuint()? as f64 / s + g.x_origin;
            let y0 = r.varuint()? as f64 / s + g.y_origin;
            let x1 = r.varuint()? as f64 / s + x0;
            let y1 = r.varuint()? as f64 / s + y0;
            Some([x0, y0, x1, y1])
        }
    }
}

/// Whether a blob's shape has true curves.
pub(super) fn has_curves(blob: &[u8]) -> bool {
    Bytes::new(blob).varuint().is_some_and(|c| c & 0x2000_0000 != 0)
}

/// Decodes a blob: `Ok(None)` for an empty shape, `Err` for one that doesn't parse or isn't
/// supported (a multipatch).
pub(super) fn decode(blob: &[u8], g: &GeomField) -> Result<Option<Geometry>, ()> {
    let mut r = Bytes::new(blob);
    let (base, has_z, has_m, has_curves) = kind(r.varuint().ok_or(())?).ok_or(())?;
    let s = g.xy_scale;
    let at = |x: i64, y: i64| Coord::new(x as f64 / s + g.x_origin, y as f64 / s + g.y_origin);
    match base {
        Base::Point => {
            let x = r.varuint().ok_or(())?;
            let y = r.varuint().ok_or(())?;
            if x == 0 {
                return Ok(None);
            }
            let c = Coord::new((x - 1) as f64 / s + g.x_origin, y.checked_sub(1).ok_or(())? as f64 / s + g.y_origin);
            Ok(Some(Geometry::Point(c)))
        }
        Base::MultiPoint => {
            let n = count(&mut r, 2)?;
            if n == 0 {
                return Ok(None);
            }
            for _ in 0..4 {
                r.varuint().ok_or(())?;
            }
            Ok(Some(Geometry::MultiPoint(points(&mut r, n, at)?)))
        }
        Base::Line | Base::Polygon => {
            let n = count(&mut r, 2)?;
            let parts = count(&mut r, 1)?;
            let curves = if has_curves { count(&mut r, 1)? } else { 0 };
            if n == 0 {
                return Ok(None);
            }
            if parts == 0 || parts > n {
                return Err(());
            }
            for _ in 0..4 {
                r.varuint().ok_or(())?;
            }
            // Each part's number of points but the last's (the rest).
            let mut sizes = Vec::with_capacity(parts);
            for _ in 1..parts {
                sizes.push(usize::try_from(r.varuint().ok_or(())?).map_err(|_| ())?);
            }
            let used = sizes.iter().try_fold(0usize, |a, b| a.checked_add(*b)).ok_or(())?;
            sizes.push(n.checked_sub(used).ok_or(())?);
            let pts = points(&mut r, n, at)?;
            let mut lines = Vec::with_capacity(parts);
            let mut start = 0usize;
            for size in &sizes {
                lines.push(pts.get(start..start + size).ok_or(())?.to_vec());
                start += size;
            }
            if curves > 0 {
                // The Z and M values come before the curves.
                for _ in 0..(usize::from(has_z) + usize::from(has_m)) * n {
                    r.varint().ok_or(())?;
                }
                // A curve not understood leaves the shape drawn through its points.
                if let Some(segments) = read_curves(&mut r, curves, &pts) {
                    lines = densify(&sizes, &pts, &segments);
                }
            }
            lines.retain(|l| !l.is_empty());
            if base == Base::Line {
                return Ok(Some(Geometry::Line(lines)));
            }
            // A ring wound clockwise is an outer one, as in a Shapefile.
            let outer = |r: &[Coord]| r.windows(2).map(|w| (w[1].x - w[0].x) * (w[1].y + w[0].y)).sum::<f64>() >= 0.0;
            Ok(Some(group_rings(
                lines
                    .into_iter()
                    .map(|r| {
                        let o = outer(&r);
                        (r, o)
                    })
                    .collect(),
            )))
        }
    }
}

/// A count of items each taking at least `min_bytes` of what's left: refused when they couldn't
/// fit, so a damaged count never allocates more than the blob could hold.
fn count(r: &mut Bytes<'_>, min_bytes: usize) -> Result<usize, ()> {
    let n = usize::try_from(r.varuint().ok_or(())?).map_err(|_| ())?;
    if n.saturating_mul(min_bytes) > r.remaining() {
        return Err(());
    }
    Ok(n)
}

/// `n` points stored as deltas from the one before (the first from 0).
fn points(r: &mut Bytes<'_>, n: usize, at: impl Fn(i64, i64) -> Coord) -> Result<Vec<Coord>, ()> {
    let mut out = Vec::with_capacity(n);
    let (mut x, mut y) = (0i64, 0i64);
    for _ in 0..n {
        x = x.checked_add(r.varint().ok_or(())?).ok_or(())?;
        y = y.checked_add(r.varint().ok_or(())?).ok_or(())?;
        out.push(at(x, y));
    }
    Ok(out)
}

/// A curved segment, from the point it starts at to the next.
#[derive(Clone, Copy, Debug)]
enum Segment {
    /// A circular arc through `p` (`interior`) or about centre `p`, counter-clockwise or not.
    Arc {
        p: Coord,
        interior: bool,
        ccw: bool,
    },
    Bezier(Coord, Coord),
    /// An elliptic arc, drawn straight.
    Straight,
}

/// The curves after the points: each the index of the point it starts at, its type and its
/// parameters. `None` when one isn't understood (the shape is then drawn through its points).
fn read_curves(r: &mut Bytes<'_>, n: usize, pts: &[Coord]) -> Option<Vec<(usize, Segment)>> {
    let mut out = Vec::with_capacity(n.min(pts.len()));
    for _ in 0..n {
        let start = usize::try_from(r.varuint()?).ok()?;
        if start + 1 >= pts.len() {
            return None;
        }
        let seg = match r.varuint()? {
            1 => {
                let p = Coord::new(r.f64()?, r.f64()?);
                let bits = r.u32()?;
                // Empty, a line or a point: straight.
                if bits & (0x01 | 0x20 | 0x40) != 0 {
                    Segment::Straight
                } else {
                    Segment::Arc { p, interior: bits & 0x80 != 0, ccw: bits & 0x08 != 0 }
                }
            }
            4 => Segment::Bezier(Coord::new(r.f64()?, r.f64()?), Coord::new(r.f64()?, r.f64()?)),
            5 => {
                // Centre (or a vertex), rotation, semi-major axis, ratio, and flags.
                r.take(5 * 8 + 4)?;
                Segment::Straight
            }
            _ => return None,
        };
        out.push((start, seg));
    }
    Some(out)
}

/// The parts with their curved segments replaced by short straight ones.
fn densify(sizes: &[usize], pts: &[Coord], segments: &[(usize, Segment)]) -> Vec<Vec<Coord>> {
    let mut by_start: Vec<Option<Segment>> = vec![None; pts.len()];
    for (i, s) in segments {
        if let Some(slot) = by_start.get_mut(*i) {
            *slot = Some(*s);
        }
    }
    let mut out = Vec::with_capacity(sizes.len());
    let mut start = 0usize;
    for size in sizes {
        let part = pts.get(start..start + size).unwrap_or(&[]);
        let mut line = Vec::with_capacity(part.len());
        for (k, c) in part.iter().enumerate() {
            line.push(*c);
            let Some(next) = part.get(k + 1) else { break };
            match by_start.get(start + k).copied().flatten() {
                Some(Segment::Arc { p, interior, ccw }) => arc(*c, *next, p, interior, ccw, &mut line),
                Some(Segment::Bezier(a, b)) => {
                    for i in 1..BEZIER_STEPS {
                        let t = i as f64 / BEZIER_STEPS as f64;
                        let u = 1.0 - t;
                        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
                        line.push(Coord::new(
                            w[0] * c.x + w[1] * a.x + w[2] * b.x + w[3] * next.x,
                            w[0] * c.y + w[1] * a.y + w[2] * b.y + w[3] * next.y,
                        ));
                    }
                }
                Some(Segment::Straight) | None => {}
            }
        }
        out.push(line);
        start += size;
    }
    out
}

const BEZIER_STEPS: usize = 32;

/// Pushes the points strictly between `a` and `b` along a circular arc (`b` is pushed by the
/// caller): through `p` when `interior`, else about centre `p`.
fn arc(a: Coord, b: Coord, p: Coord, interior: bool, ccw: bool, out: &mut Vec<Coord>) {
    use std::f64::consts::TAU;
    let closed = (a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12;
    let (centre, ccw) = if interior {
        if closed {
            // A full circle: the interior point is opposite the start.
            (Coord::new((a.x + p.x) / 2.0, (a.y + p.y) / 2.0), ccw)
        } else {
            // The circle through all three, turning the way they do.
            let cross = (p.x - a.x) * (b.y - a.y) - (p.y - a.y) * (b.x - a.x);
            match circumcentre(a, p, b) {
                Some(c) => (c, cross > 0.0),
                None => return,
            }
        }
    } else {
        (p, ccw)
    };
    let radius = ((a.x - centre.x).powi(2) + (a.y - centre.y).powi(2)).sqrt();
    if !radius.is_finite() || radius == 0.0 {
        return;
    }
    let a0 = (a.y - centre.y).atan2(a.x - centre.x);
    let a1 = (b.y - centre.y).atan2(b.x - centre.x);
    let mut sweep = a1 - a0;
    if closed {
        sweep = if ccw { TAU } else { -TAU };
    } else if ccw {
        while sweep <= 0.0 {
            sweep += TAU;
        }
    } else {
        while sweep >= 0.0 {
            sweep -= TAU;
        }
    }
    // A point every 2 degrees, at least a few for a short arc.
    let steps = ((sweep.abs() / TAU * 180.0).ceil() as usize).clamp(4, 1024);
    for i in 1..steps {
        let t = a0 + sweep * i as f64 / steps as f64;
        out.push(Coord::new(centre.x + radius * t.cos(), centre.y + radius * t.sin()));
    }
}

/// The centre of the circle through three points; `None` when they're in a line.
fn circumcentre(a: Coord, b: Coord, c: Coord) -> Option<Coord> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if d.abs() < 1e-300 {
        return None;
    }
    let (a2, b2, c2) = (a.x * a.x + a.y * a.y, b.x * b.x + b.y * b.y, c.x * c.x + c.y * c.y);
    let x = (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d;
    let y = (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d;
    (x.is_finite() && y.is_finite()).then_some(Coord::new(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field() -> GeomField {
        GeomField { wkt: String::new(), x_origin: -400.0, y_origin: -400.0, xy_scale: 1e9, extent: [f64::NAN; 4] }
    }

    #[test]
    fn arcs_through_an_interior_point() {
        let mut out = Vec::new();
        // From (0,0) through (5,5) to (10,0): clockwise over the top, centre (5,0), radius 5.
        arc(Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(5.0, 5.0), true, false, &mut out);
        assert!(out.len() >= 4);
        for c in &out {
            assert!(((c.x - 5.0).powi(2) + c.y.powi(2)).sqrt() - 5.0 < 1e-9);
            assert!(c.y > 0.0, "{c:?}");
        }
        // A full circle: start and end alike, the interior point opposite.
        let mut out = Vec::new();
        arc(Coord::new(0.0, 0.0), Coord::new(0.0, 0.0), Coord::new(0.0, 10.0), true, false, &mut out);
        assert!(out.len() > 100);
        assert!(out.iter().any(|c| c.x > 4.9) && out.iter().any(|c| c.x < -4.9));
    }

    #[test]
    fn arcs_about_a_centre() {
        let mut out = Vec::new();
        arc(Coord::new(1.0, 0.0), Coord::new(0.0, 1.0), Coord::new(0.0, 0.0), false, true, &mut out);
        // A quarter turn counter-clockwise: every point in the first quadrant.
        assert!(out.iter().all(|c| c.x > 0.0 && c.y > 0.0));
        let mut out = Vec::new();
        arc(Coord::new(1.0, 0.0), Coord::new(0.0, 1.0), Coord::new(0.0, 0.0), false, false, &mut out);
        // Clockwise the long way round: through the other three quadrants.
        assert!(out.iter().any(|c| c.x < 0.0 && c.y < 0.0));
    }

    #[test]
    fn damaged_blobs_are_refused() {
        let g = field();
        // A polyline claiming far more points than its bytes hold.
        let blob = [3u8, 0xff, 0xff, 0x7f, 1, 0, 0, 0, 0];
        assert!(decode(&blob, &g).is_err());
        // More parts than points.
        let blob = [3u8, 1, 5, 0, 0, 0, 0, 0, 0];
        assert!(decode(&blob, &g).is_err());
        assert!(decode(&[], &g).is_err());
        assert!(decode(&[77], &g).is_err());
        // An empty point.
        assert_eq!(decode(&[1, 0, 0], &g), Ok(None));
        for n in 0..blob.len() {
            let _ = decode(blob.get(..n).unwrap_or(&[]), &g);
            let _ = bbox(blob.get(..n).unwrap_or(&[]), &g);
        }
    }
}
