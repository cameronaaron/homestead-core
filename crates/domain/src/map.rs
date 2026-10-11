//! Where places are: a shape for each on the farm's plan.
//!
//! Coordinates are feet east (x) and north (y) of a corner the farm picks
//! (a fence post, the shed's corner), not latitude and longitude. A phone's
//! GPS is good to a few metres, which can't tell one 4 ft bed from the
//! next, and there's no aerial photo to trace over; a tape measure from a
//! fixed corner can.

use serde::{Deserialize, Serialize};

use crate::ValidationError;

string_enum!(ShapeKind, "kind" { Point => "point", Line => "line", Polygon => "polygon" });

/// A spot on the plan, in feet: `[x, y]` (east, north).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pt(pub f64, pub f64);

/// Past this many feet from the corner it isn't the homestead any more.
pub const MAX_FEET: f64 = 20_000.0;
pub const MAX_POINTS: usize = 200;

/// A point (a hydrant, a hive), a line (a fence, a drip line) or an
/// outline (a bed, the coop yard).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub kind: ShapeKind,
    pub points: Vec<Pt>,
}

/// Reads what a person typed: numbers in `x,y` pairs, separated by spaces,
/// commas, semicolons or new lines. `how` is `rectangle` (two opposite
/// corners), `outline`, `line` or `point`.
pub fn parse_shape(how: &str, text: &str) -> Result<Shape, ValidationError> {
    let numbers = text
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<f64>().map_err(|_| ValidationError::Malformed("points (expected numbers in x,y pairs)")))
        .collect::<Result<Vec<_>, _>>()?;
    if numbers.len() % 2 != 0 {
        return Err(ValidationError::Malformed("points (a number is missing its pair)"));
    }
    let pts: Vec<Pt> = numbers.chunks(2).map(|p| Pt(p[0], p[1])).collect();
    let shape = match how {
        "rectangle" => match pts[..] {
            [Pt(x1, y1), Pt(x2, y2)] => {
                Shape { kind: ShapeKind::Polygon, points: vec![Pt(x1, y1), Pt(x2, y1), Pt(x2, y2), Pt(x1, y2)] }
            }
            _ => return Err(ValidationError::Malformed("rectangle (give two opposite corners)")),
        },
        "outline" => Shape { kind: ShapeKind::Polygon, points: pts },
        "line" => Shape { kind: ShapeKind::Line, points: pts },
        "point" => Shape { kind: ShapeKind::Point, points: pts },
        other => return Err(ValidationError::UnknownValue { field: "shape", value: other.to_owned() }),
    };
    shape.validated()
}

impl Shape {
    /// Rounds to a hundredth of a foot, drops repeated points (and an
    /// outline's closing copy of its first), and refuses shapes that can't
    /// be drawn or measured: an outline must enclose an area without
    /// crossing itself, or its area would be wrong.
    pub fn validated(self) -> Result<Self, ValidationError> {
        let mut points: Vec<Pt> = Vec::with_capacity(self.points.len());
        for Pt(x, y) in self.points {
            if !x.is_finite() || !y.is_finite() || x.abs() > MAX_FEET || y.abs() > MAX_FEET {
                return Err(ValidationError::Malformed("point (more than 20,000 ft from the corner)"));
            }
            let p = Pt((x * 100.0).round() / 100.0, (y * 100.0).round() / 100.0);
            if points.last() != Some(&p) {
                points.push(p);
            }
        }
        if self.kind == ShapeKind::Polygon && points.len() > 1 && points.first() == points.last() {
            points.pop();
        }
        if points.len() > MAX_POINTS {
            return Err(ValidationError::Malformed("shape (at most 200 points)"));
        }
        let shape = Shape { kind: self.kind, points };
        match shape.kind {
            ShapeKind::Point if shape.points.len() != 1 => Err(ValidationError::Malformed("point (give one x,y)")),
            ShapeKind::Line if shape.points.len() < 2 => {
                Err(ValidationError::Malformed("line (give at least two different points)"))
            }
            // Crossing first: a bow tie's shoelace area is zero, but "encloses no area" would be the wrong reason.
            ShapeKind::Polygon if shape.points.len() >= 3 && crosses_itself(&shape.points) => {
                Err(ValidationError::Malformed("outline (its edges cross)"))
            }
            ShapeKind::Polygon if shape.points.len() < 3 || shape.area_sqft().is_none_or(|a| a < 0.01) => {
                Err(ValidationError::Malformed("outline (it must enclose some area)"))
            }
            _ => Ok(shape),
        }
    }

    /// An outline's area (shoelace formula); lines and points have none.
    pub fn area_sqft(&self) -> Option<f64> {
        if self.kind != ShapeKind::Polygon || self.points.len() < 3 {
            return None;
        }
        Some(twice_signed_area(&self.points).abs() / 2.0)
    }

    /// A line's length in feet.
    pub fn length_ft(&self) -> Option<f64> {
        (self.kind == ShapeKind::Line)
            .then(|| self.points.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum())
    }

    /// Where to put its name: an outline's centroid, a line's middle
    /// vertex (or its midpoint when it's one segment), or the point.
    pub fn label_at(&self) -> Pt {
        let p = &self.points;
        match self.kind {
            ShapeKind::Polygon => {
                let a2 = twice_signed_area(p);
                let (mut cx, mut cy) = (0.0, 0.0);
                for (i, a) in p.iter().enumerate() {
                    let b = p[(i + 1) % p.len()];
                    let cross = a.0 * b.1 - b.0 * a.1;
                    cx += (a.0 + b.0) * cross;
                    cy += (a.1 + b.1) * cross;
                }
                Pt(cx / (3.0 * a2), cy / (3.0 * a2))
            }
            ShapeKind::Line if p.len() == 2 => Pt((p[0].0 + p[1].0) / 2.0, (p[0].1 + p[1].1) / 2.0),
            ShapeKind::Line => p[p.len() / 2],
            ShapeKind::Point => p[0],
        }
    }

    /// `(min x, min y, max x, max y)`.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        self.points.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |(a, b, c, d), p| {
            (a.min(p.0), b.min(p.1), c.max(p.0), d.max(p.1))
        })
    }

    /// The points as a person would type them back in.
    pub fn points_text(&self) -> String {
        self.points.iter().map(|Pt(x, y)| format!("{x},{y}")).collect::<Vec<_>>().join(" ")
    }
}

fn twice_signed_area(p: &[Pt]) -> f64 {
    (0..p.len()).map(|i| p[i].0 * p[(i + 1) % p.len()].1 - p[(i + 1) % p.len()].0 * p[i].1).sum()
}

/// Whether any two edges that don't share a corner touch or cross. At 200
/// points this is under 20,000 segment tests.
fn crosses_itself(p: &[Pt]) -> bool {
    let n = p.len();
    let edge = |i: usize| (p[i], p[(i + 1) % n]);
    (0..n).any(|i| {
        (i + 1..n).any(|j| {
            let adjacent = j == i + 1 || (i == 0 && j == n - 1);
            if adjacent {
                // Neighbours share a corner; they only go wrong by folding back along each other.
                let (a, b) = edge(i);
                let (c, d) = edge(j);
                let (shared, u, v) = if b == c { (b, a, d) } else { (a, b, c) };
                orient(shared, u, v) == 0.0 && dot(shared, u, v) > 0.0
            } else {
                let (a, b) = edge(i);
                let (c, d) = edge(j);
                segments_touch(a, b, c, d)
            }
        })
    })
}

fn orient(a: Pt, b: Pt, c: Pt) -> f64 {
    let v = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    if v.abs() < 1e-9 { 0.0 } else { v }
}

fn dot(o: Pt, a: Pt, b: Pt) -> f64 {
    (a.0 - o.0) * (b.0 - o.0) + (a.1 - o.1) * (b.1 - o.1)
}

fn on_segment(a: Pt, b: Pt, p: Pt) -> bool {
    p.0 >= a.0.min(b.0) && p.0 <= a.0.max(b.0) && p.1 >= a.1.min(b.1) && p.1 <= a.1.max(b.1)
}

fn segments_touch(a: Pt, b: Pt, c: Pt, d: Pt) -> bool {
    let (o1, o2, o3, o4) = (orient(a, b, c), orient(a, b, d), orient(c, d, a), orient(c, d, b));
    if o1 * o2 < 0.0 && o3 * o4 < 0.0 {
        return true;
    }
    (o1 == 0.0 && on_segment(a, b, c))
        || (o2 == 0.0 && on_segment(a, b, d))
        || (o3 == 0.0 && on_segment(c, d, a))
        || (o4 == 0.0 && on_segment(c, d, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn check_list_matches_migration() {
        let sql = include_str!("../../../migrations/0042_farm_map.sql");
        crate::tests::assert_check_list(sql, "kind", ShapeKind::ALL, ShapeKind::as_str);
    }

    #[test]
    fn a_rectangle_is_two_corners_and_measures_its_area() {
        let bed = parse_shape("rectangle", "10,20 14,32").expect("bed");
        assert_eq!(bed.kind, ShapeKind::Polygon);
        assert_eq!(bed.points, [Pt(10.0, 20.0), Pt(14.0, 20.0), Pt(14.0, 32.0), Pt(10.0, 32.0)]);
        assert!(close(bed.area_sqft().expect("area"), 48.0), "4 ft × 12 ft");
        assert_eq!(bed.label_at(), Pt(12.0, 26.0));
        assert_eq!(bed.bounds(), (10.0, 20.0, 14.0, 32.0));
        // Corners given the other way round are the same bed.
        assert!(close(parse_shape("rectangle", "14,32 10,20").expect("r").area_sqft().expect("a"), 48.0));
        assert!(parse_shape("rectangle", "0,0 4,4 8,8").is_err(), "three corners isn't a rectangle");
        assert!(parse_shape("rectangle", "0,0 0,9").is_err(), "no width, no area");
    }

    #[test]
    fn typing_is_forgiving_about_separators_but_not_numbers() {
        let a = parse_shape("outline", "0,0 30,0 30,20 0,20").expect("a");
        let b = parse_shape("outline", "0, 0; 30, 0\n30,20\n0 20\n0,0").expect("b");
        assert_eq!(a, b, "spaces, semicolons, new lines and a closing point read the same");
        assert!(parse_shape("outline", "0,0 30,0 30").is_err(), "an unpaired number");
        assert!(parse_shape("outline", "0,0 3O,0 30,20").is_err(), "a letter O is not a zero");
        assert!(parse_shape("hexagon", "0,0").is_err());
    }

    #[test]
    fn an_outline_that_crosses_itself_or_folds_back_is_refused() {
        // A bow tie: its shoelace "area" is zero, which is not its real area, nor the reason it's wrong.
        let edges_cross = Err(ValidationError::Malformed("outline (its edges cross)"));
        assert_eq!(parse_shape("outline", "0,0 10,10 10,0 0,10"), edges_cross);
        // Crossing edges that still leave shoelace area.
        assert!(parse_shape("outline", "0,0 10,0 10,10 5,-5 0,10").is_err());
        // An edge doubling back along its neighbour.
        assert!(parse_shape("outline", "0,0 10,0 5,0 5,5").is_err());
        // A point touching a non-adjacent edge.
        assert!(parse_shape("outline", "0,0 10,0 10,10 5,0 0,10").is_err());
        // An L-shaped (concave) bed is fine.
        let l = parse_shape("outline", "0,0 20,0 20,4 4,4 4,12 0,12").expect("L");
        assert!(close(l.area_sqft().expect("a"), 20.0 * 4.0 + 4.0 * 8.0));
    }

    #[test]
    fn lines_and_points_measure_what_they_can() {
        let fence = parse_shape("line", "0,0 30,0 30,40").expect("fence");
        assert!(close(fence.length_ft().expect("len"), 70.0));
        assert_eq!(fence.area_sqft(), None);
        assert_eq!(fence.label_at(), Pt(30.0, 0.0));
        assert_eq!(parse_shape("line", "0,0 0,10").expect("l").label_at(), Pt(0.0, 5.0));
        assert!(parse_shape("line", "3,3 3,3").is_err(), "a line needs two different points");
        let hive = parse_shape("point", "12.5,-4").expect("hive");
        assert_eq!((hive.label_at(), hive.length_ft()), (Pt(12.5, -4.0), None));
        assert!(parse_shape("point", "1,1 2,2").is_err());
    }

    #[test]
    fn coordinates_are_bounded_and_rounded() {
        assert!(parse_shape("point", "20001,0").is_err());
        assert!(parse_shape("point", "NaN,0").is_err());
        assert!(parse_shape("point", "inf,0").is_err());
        assert_eq!(parse_shape("point", "1.23456,2.006").expect("p").points, [Pt(1.23, 2.01)]);
        let many = (0..201).map(|i| format!("{i},0")).collect::<Vec<_>>().join(" ");
        assert!(parse_shape("line", &many).is_err(), "more than 200 points");
    }

    #[test]
    fn typed_points_read_back_as_the_same_shape() {
        let s = parse_shape("outline", "0,0 12.5,0 12.5,8 0,8").expect("s");
        assert_eq!(parse_shape("outline", &s.points_text()).expect("again"), s);
        let json = serde_json::to_value(&s.points).expect("json");
        assert_eq!(json, serde_json::json!([[0.0, 0.0], [12.5, 0.0], [12.5, 8.0], [0.0, 8.0]]));
    }
}
