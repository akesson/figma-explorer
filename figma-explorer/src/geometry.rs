//! Bounding-box math used to associate Figma comments anchored to absolute
//! canvas coordinates (`Vector` / `Region`) with the nodes whose bounds they
//! fall inside or near.
//!
//! All inputs are in canvas units (the same coordinate system Figma uses for
//! `absoluteBoundingBox`). Width/height are assumed non-negative; nothing here
//! sanitizes pathological zero-area rects.
//!
//! Kept free of allocation and side effects so it can be unit-tested in
//! isolation and reused from `comment_assoc` without `tokio`/cache concerns.

use crate::node::Bounds;

/// True iff `(x, y)` lies inside the closed rectangle defined by `b`.
pub fn contains_point(b: &Bounds, x: f64, y: f64) -> bool {
    x >= b.x && x <= b.x + b.width && y >= b.y && y <= b.y + b.height
}

/// Euclidean distance from `(x, y)` to the rectangle. Zero when the point is
/// inside or on the boundary. Computed by clamping the point to the rect and
/// measuring the offset — works for all eight outside regions in one shot.
pub fn dist_to_rect(b: &Bounds, x: f64, y: f64) -> f64 {
    let cx = x.clamp(b.x, b.x + b.width);
    let cy = y.clamp(b.y, b.y + b.height);
    let dx = x - cx;
    let dy = y - cy;
    (dx * dx + dy * dy).sqrt()
}

pub fn area(b: &Bounds) -> f64 {
    b.width.max(0.0) * b.height.max(0.0)
}

/// Area of the intersection of two rectangles, or 0 if they're disjoint.
pub fn intersection_area(a: &Bounds, b: &Bounds) -> f64 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.width).min(b.x + b.width);
    let y2 = (a.y + a.height).min(b.y + b.height);
    let w = x2 - x1;
    let h = y2 - y1;
    if w <= 0.0 || h <= 0.0 {
        0.0
    } else {
        w * h
    }
}

/// Intersection-over-union. Returns 0.0 if both rects are zero-area.
pub fn iou(a: &Bounds, b: &Bounds) -> f64 {
    let inter = intersection_area(a, b);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: f64, y: f64, w: f64, h: f64) -> Bounds {
        Bounds {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn contains_point_inside_outside_and_corners() {
        let r = b(10.0, 20.0, 100.0, 50.0);
        assert!(contains_point(&r, 60.0, 45.0));
        assert!(
            contains_point(&r, 10.0, 20.0),
            "top-left corner is inclusive"
        );
        assert!(
            contains_point(&r, 110.0, 70.0),
            "bottom-right corner is inclusive"
        );
        assert!(!contains_point(&r, 9.9, 45.0));
        assert!(!contains_point(&r, 60.0, 70.1));
    }

    #[test]
    fn dist_to_rect_zero_when_inside() {
        let r = b(0.0, 0.0, 100.0, 100.0);
        assert_eq!(dist_to_rect(&r, 50.0, 50.0), 0.0);
        assert_eq!(dist_to_rect(&r, 0.0, 0.0), 0.0);
    }

    #[test]
    fn dist_to_rect_orthogonal_outside() {
        let r = b(0.0, 0.0, 100.0, 100.0);
        // 30px to the right of the right edge.
        assert!((dist_to_rect(&r, 130.0, 50.0) - 30.0).abs() < 1e-9);
        // 20px below the bottom edge.
        assert!((dist_to_rect(&r, 50.0, 120.0) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn dist_to_rect_diagonal_outside() {
        let r = b(0.0, 0.0, 100.0, 100.0);
        // Corner (3,4) away from top-left → distance 5.
        let d = dist_to_rect(&r, -3.0, -4.0);
        assert!((d - 5.0).abs() < 1e-9, "got {d}");
    }

    #[test]
    fn iou_identical_is_one() {
        let r = b(10.0, 10.0, 20.0, 20.0);
        assert!((iou(&r, &r) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn iou_disjoint_is_zero() {
        let a = b(0.0, 0.0, 10.0, 10.0);
        let c = b(100.0, 100.0, 10.0, 10.0);
        assert_eq!(iou(&a, &c), 0.0);
    }

    #[test]
    fn iou_symmetric_and_partial() {
        let a = b(0.0, 0.0, 10.0, 10.0);
        let c = b(5.0, 5.0, 10.0, 10.0);
        let ac = iou(&a, &c);
        let ca = iou(&c, &a);
        assert!((ac - ca).abs() < 1e-12);
        // intersection = 5x5 = 25, union = 100 + 100 - 25 = 175 → 1/7.
        assert!((ac - (25.0 / 175.0)).abs() < 1e-9, "got {ac}");
    }

    #[test]
    fn intersection_area_disjoint_is_zero() {
        let a = b(0.0, 0.0, 10.0, 10.0);
        let c = b(11.0, 0.0, 10.0, 10.0);
        assert_eq!(intersection_area(&a, &c), 0.0);
    }
}
