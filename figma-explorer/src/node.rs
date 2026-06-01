//! Generic JSON-view accessors over Figma node objects.
//!
//! The figma-api crate models every node type as its own struct inside a
//! 26-variant `SubcanvasNode` enum. Pattern-matching all of those for every
//! field we touch would explode the code size, and tree walking is exactly
//! the use case where serde_json::Value is more ergonomic than the typed
//! tree. So at the seam between API call and analysis we drop into JSON
//! and stay there.
//!
//! All accessors take `&serde_json::Value` (a node object) and return `Option<_>`
//! when the field may be missing. None of them allocate.
//!
//! Visibility: Figma omits `visible` when the node is visible. We treat a
//! missing field as visible — only `visible: false` hides a node.

use serde_json::Value;

pub fn id(node: &Value) -> Option<&str> {
    node.get("id")?.as_str()
}

pub fn name(node: &Value) -> Option<&str> {
    node.get("name")?.as_str()
}

pub fn type_str(node: &Value) -> Option<&str> {
    node.get("type")?.as_str()
}

pub fn is_visible(node: &Value) -> bool {
    !matches!(node.get("visible"), Some(Value::Bool(false)))
}

pub fn children(node: &Value) -> &[Value] {
    node.get("children")
        .and_then(|c| c.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// `absoluteBoundingBox` if present; coordinates and size in canvas units.
pub fn bounds(node: &Value) -> Option<Bounds> {
    let bb = node.get("absoluteBoundingBox")?.as_object()?;
    Some(Bounds {
        x: bb.get("x")?.as_f64()?,
        y: bb.get("y")?.as_f64()?,
        width: bb.get("width")?.as_f64()?,
        height: bb.get("height")?.as_f64()?,
    })
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl std::fmt::Display for Bounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}×{} @{},{}",
            self.width.round() as i64,
            self.height.round() as i64,
            self.x.round() as i64,
            self.y.round() as i64,
        )
    }
}

impl Bounds {
    /// Whitespace-free rendering: `WxH@X,Y`. Used by the flat `ls` output
    /// where the bounds occupy a fixed column and inner spaces would create
    /// phantom awk fields. Distinct from `Display` (which keeps the inner
    /// space for human-friendly tree rendering).
    pub fn compact(&self) -> String {
        format!(
            "{}x{}@{},{}",
            self.width.round() as i64,
            self.height.round() as i64,
            self.x.round() as i64,
            self.y.round() as i64,
        )
    }
}

pub fn fills(node: &Value) -> &[Value] {
    node.get("fills")
        .and_then(|f| f.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn strokes(node: &Value) -> &[Value] {
    node.get("strokes")
        .and_then(|f| f.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn effects(node: &Value) -> &[Value] {
    node.get("effects")
        .and_then(|e| e.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn type_style(node: &Value) -> Option<&Value> {
    node.get("style")
}

/// First visible solid fill, as a 6- or 8-digit hex string (`#rrggbb` or `#rrggbbaa`).
pub fn primary_solid_hex(node: &Value) -> Option<String> {
    for paint in fills(node) {
        if !is_paint_visible(paint) {
            continue;
        }
        if paint.get("type").and_then(|v| v.as_str()) == Some("SOLID") {
            if let Some(color) = paint.get("color") {
                return Some(rgba_to_hex(color));
            }
        }
    }
    None
}

pub fn is_paint_visible(paint: &Value) -> bool {
    !matches!(paint.get("visible"), Some(Value::Bool(false)))
}

pub fn rgba_to_hex(color: &Value) -> String {
    let r = color.get("r").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let g = color.get("g").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let b = color.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let a = color.get("a").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let r = (r.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (b.clamp(0.0, 1.0) * 255.0).round() as u8;
    if (a - 1.0).abs() < f64::EPSILON {
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    } else {
        let a = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}{:02x}", r, g, b, a)
    }
}

pub fn has_image_fill(node: &Value) -> bool {
    fills(node)
        .iter()
        .filter(|p| is_paint_visible(p))
        .any(|p| p.get("type").and_then(|v| v.as_str()) == Some("IMAGE"))
}

/// Walk every descendant (in DFS pre-order), skipping invisible nodes.
pub fn walk_visible<'a, F>(node: &'a Value, mut f: F)
where
    F: FnMut(&'a Value),
{
    fn rec<'a, F: FnMut(&'a Value)>(n: &'a Value, f: &mut F, depth: usize) {
        if !is_visible(n) {
            return;
        }
        f(n);
        if depth >= crate::MAX_NODE_DEPTH {
            eprintln!(
                "node: node tree exceeded max depth {}; truncating walk",
                crate::MAX_NODE_DEPTH
            );
            return;
        }
        for c in children(n) {
            rec(c, f, depth + 1);
        }
    }
    rec(node, &mut f, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn visibility_default_is_visible() {
        let n = json!({ "id": "1", "name": "x", "type": "FRAME" });
        assert!(is_visible(&n));
    }

    #[test]
    fn visibility_explicit_false_hides() {
        let n = json!({ "visible": false });
        assert!(!is_visible(&n));
    }

    #[test]
    fn rgba_to_hex_round_trips_pure_red() {
        let red = json!({ "r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0 });
        assert_eq!(rgba_to_hex(&red), "#ff0000");
    }

    #[test]
    fn rgba_to_hex_includes_alpha_when_non_opaque() {
        let semi = json!({ "r": 0.0, "g": 0.0, "b": 0.0, "a": 0.5 });
        assert_eq!(rgba_to_hex(&semi), "#00000080");
    }

    #[test]
    fn walk_visible_skips_hidden_subtrees() {
        let n = json!({
            "id": "a", "type": "FRAME", "children": [
                { "id": "b", "type": "FRAME", "visible": false, "children": [
                    { "id": "c", "type": "RECTANGLE" }
                ]},
                { "id": "d", "type": "RECTANGLE" }
            ]
        });
        let mut seen = vec![];
        walk_visible(&n, |v| seen.push(id(v).unwrap_or("").to_owned()));
        assert_eq!(seen, vec!["a", "d"]);
    }
}
