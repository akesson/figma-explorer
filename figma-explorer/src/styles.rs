//! Design-token extraction.
//!
//! Walks a node subtree to collect colors, fonts, font sizes, spacing,
//! corner radii, shadows/effects, and layout grids. Optionally merges in
//! "published" styles from the file response's top-level `styles` map.
//!
//! Three emit formats:
//!   - `tokens` (raw JSON, nested by category)
//!   - `css`    (CSS variables under `:root`)
//!   - `tailwind` (a `theme.extend` shape for tailwind.config.js)

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{json, Value};

use crate::node::{
    children, effects, fills, is_visible, name as node_name, primary_solid_hex, rgba_to_hex,
    type_style,
};

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum Format {
    #[default]
    Tokens,
    Css,
    Tailwind,
}

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum Scope {
    /// Only inspect the resolved node subtree.
    Target,
    /// Only use published styles from the file response.
    File,
    /// Combine both (default).
    #[default]
    Both,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum, PartialEq, Eq)]
pub enum Category {
    Colors,
    Fonts,
    FontSizes,
    Spacing,
    Radii,
    Shadows,
    Grids,
}

#[derive(Default, Serialize)]
pub struct Tokens {
    pub colors: BTreeMap<String, String>,
    pub fonts: BTreeSet<String>,
    pub font_sizes: BTreeMap<String, f64>,
    pub spacing: BTreeMap<String, f64>,
    pub radii: BTreeMap<String, f64>,
    pub shadows: BTreeMap<String, String>,
    pub grids: Vec<Value>,
}

/// Walk `target` and collect tokens from every visible descendant.
pub fn collect_from_target(target: &Value, tokens: &mut Tokens) {
    walk(target, tokens);
}

fn walk(node: &Value, tokens: &mut Tokens) {
    if !is_visible(node) {
        return;
    }

    // Colors: solid fills.
    for paint in fills(node) {
        if paint.get("visible") == Some(&Value::Bool(false)) {
            continue;
        }
        if paint.get("type").and_then(|v| v.as_str()) == Some("SOLID") {
            if let Some(color) = paint.get("color") {
                let hex = rgba_to_hex(color);
                let key = node_name(node).unwrap_or("color").to_owned();
                tokens.colors.entry(slugify(&key)).or_insert(hex);
            }
        }
    }

    // Typography from inline `style` block (present on TEXT nodes).
    if let Some(style) = type_style(node) {
        if let Some(family) = style.get("fontFamily").and_then(|v| v.as_str()) {
            tokens.fonts.insert(family.to_owned());
        }
        if let Some(size) = style.get("fontSize").and_then(|v| v.as_f64()) {
            let label = if let Some(nm) = node_name(node) {
                slugify(nm)
            } else {
                format!("{}", size.round() as i64)
            };
            tokens.font_sizes.entry(label).or_insert(size);
        }
    }

    // Spacing & padding from auto-layout frames.
    let padding_fields = [
        ("paddingLeft", "padding-left"),
        ("paddingRight", "padding-right"),
        ("paddingTop", "padding-top"),
        ("paddingBottom", "padding-bottom"),
        ("itemSpacing", "gap"),
    ];
    for (field, label) in padding_fields {
        if let Some(v) = node.get(field).and_then(|v| v.as_f64()) {
            if v > 0.0 {
                let key = match node_name(node) {
                    Some(n) if !n.is_empty() => format!("{}-{}", slugify(n), label),
                    _ => label.to_owned(),
                };
                tokens.spacing.entry(key).or_insert(v);
            }
        }
    }

    // Corner radii.
    if let Some(r) = node.get("cornerRadius").and_then(|v| v.as_f64()) {
        let key = node_name(node)
            .map(slugify)
            .unwrap_or_else(|| "rounded".into());
        tokens.radii.entry(key).or_insert(r);
    }

    // Effects (shadows).
    for fx in effects(node) {
        if fx.get("visible") == Some(&Value::Bool(false)) {
            continue;
        }
        let ty = fx.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "DROP_SHADOW" || ty == "INNER_SHADOW" {
            let offset = fx.get("offset");
            let x = offset
                .and_then(|o| o.get("x"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let y = offset
                .and_then(|o| o.get("y"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let blur = fx.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let spread = fx.get("spread").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let color = fx.get("color").map(rgba_to_hex).unwrap_or_default();
            let inset = if ty == "INNER_SHADOW" { "inset " } else { "" };
            let value = format!("{}{}px {}px {}px {}px {}", inset, x, y, blur, spread, color);
            let key = node_name(node)
                .map(slugify)
                .unwrap_or_else(|| "shadow".into());
            tokens.shadows.entry(key).or_insert(value);
        }
    }

    // Layout grids.
    if let Some(grids) = node.get("layoutGrids").and_then(|v| v.as_array()) {
        for g in grids {
            tokens.grids.push(g.clone());
        }
    }

    for c in children(node) {
        walk(c, tokens);
    }
}

/// Merge published styles from the file response (top-level `styles` map).
///
/// The published-styles map is keyed by style id; values include `key`, `name`,
/// `description`, and `styleType` (FILL / TEXT / EFFECT / GRID). Names often
/// use slash-separated paths (e.g. `color/primary/500`) which we preserve as
/// dotted token paths.
pub fn merge_published(file_resp: &Value, tokens: &mut Tokens) {
    let Some(styles) = file_resp.get("styles").and_then(|v| v.as_object()) else {
        return;
    };
    // Build a map from style-id → category. We may need it later to attach
    // resolved values from nodes.
    let mut categorized: Vec<(&str, &str, &str)> = Vec::new();
    for (sid, meta) in styles {
        let ty = meta.get("styleType").and_then(|v| v.as_str()).unwrap_or("");
        let nm = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
        categorized.push((sid, ty, nm));
    }
    // Walk the document, looking for nodes whose `styles` map references our
    // published style ids; pull the corresponding value out.
    fn walk(node: &Value, categorized: &[(&str, &str, &str)], tokens: &mut Tokens) {
        if !is_visible(node) {
            return;
        }
        if let Some(map) = node.get("styles").and_then(|v| v.as_object()) {
            for (slot, sid) in map {
                let Some(sid) = sid.as_str() else { continue };
                let Some((_, _ty, name)) = categorized.iter().find(|(id, _, _)| *id == sid) else {
                    continue;
                };
                let key = name.replace('/', ".");
                match slot.as_str() {
                    "fill" | "fills" => {
                        if let Some(hex) = primary_solid_hex(node) {
                            tokens.colors.entry(key).or_insert(hex);
                        }
                    }
                    "text" => {
                        if let Some(style) = type_style(node) {
                            if let Some(family) = style.get("fontFamily").and_then(|v| v.as_str()) {
                                tokens.fonts.insert(family.to_owned());
                            }
                            if let Some(size) = style.get("fontSize").and_then(|v| v.as_f64()) {
                                tokens.font_sizes.entry(key.clone()).or_insert(size);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for c in children(node) {
            walk(c, categorized, tokens);
        }
    }
    if let Some(doc) = file_resp.get("document") {
        walk(doc, &categorized, tokens);
    }
}

pub fn filter(tokens: &mut Tokens, only: &[Category]) {
    if only.is_empty() {
        return;
    }
    let want = |c: Category| only.contains(&c);
    if !want(Category::Colors) {
        tokens.colors.clear();
    }
    if !want(Category::Fonts) {
        tokens.fonts.clear();
    }
    if !want(Category::FontSizes) {
        tokens.font_sizes.clear();
    }
    if !want(Category::Spacing) {
        tokens.spacing.clear();
    }
    if !want(Category::Radii) {
        tokens.radii.clear();
    }
    if !want(Category::Shadows) {
        tokens.shadows.clear();
    }
    if !want(Category::Grids) {
        tokens.grids.clear();
    }
}

pub fn render(tokens: &Tokens, format: Format) -> Value {
    match format {
        Format::Tokens => json!({
            "colors": tokens.colors,
            "fonts": tokens.fonts.iter().collect::<Vec<_>>(),
            "fontSizes": tokens.font_sizes,
            "spacing": tokens.spacing,
            "radii": tokens.radii,
            "shadows": tokens.shadows,
            "grids": tokens.grids,
        }),
        Format::Css => Value::String(render_css(tokens)),
        Format::Tailwind => render_tailwind(tokens),
    }
}

fn render_css(tokens: &Tokens) -> String {
    let mut s = String::from(":root {\n");
    for (k, v) in &tokens.colors {
        s.push_str(&format!("  --color-{}: {};\n", slugify(k), v));
    }
    for (i, family) in tokens.fonts.iter().enumerate() {
        s.push_str(&format!("  --font-{}: {:?};\n", i, family));
    }
    for (k, v) in &tokens.font_sizes {
        s.push_str(&format!("  --font-size-{}: {}px;\n", slugify(k), v));
    }
    for (k, v) in &tokens.spacing {
        s.push_str(&format!("  --space-{}: {}px;\n", slugify(k), v));
    }
    for (k, v) in &tokens.radii {
        s.push_str(&format!("  --radius-{}: {}px;\n", slugify(k), v));
    }
    for (k, v) in &tokens.shadows {
        s.push_str(&format!("  --shadow-{}: {};\n", slugify(k), v));
    }
    s.push_str("}\n");
    s
}

fn render_tailwind(tokens: &Tokens) -> Value {
    let mut font_size = serde_json::Map::new();
    for (k, v) in &tokens.font_sizes {
        font_size.insert(k.clone(), json!(format!("{}px", v)));
    }
    let mut spacing = serde_json::Map::new();
    for (k, v) in &tokens.spacing {
        spacing.insert(k.clone(), json!(format!("{}px", v)));
    }
    let mut radii = serde_json::Map::new();
    for (k, v) in &tokens.radii {
        radii.insert(k.clone(), json!(format!("{}px", v)));
    }
    let mut shadows = serde_json::Map::new();
    for (k, v) in &tokens.shadows {
        shadows.insert(k.clone(), json!(v));
    }
    let mut font_family = serde_json::Map::new();
    for (i, f) in tokens.fonts.iter().enumerate() {
        font_family.insert(format!("custom-{}", i), json!([f]));
    }
    json!({
        "theme": {
            "extend": {
                "colors": tokens.colors,
                "fontFamily": font_family,
                "fontSize": font_size,
                "spacing": spacing,
                "borderRadius": radii,
                "boxShadow": shadows,
            }
        }
    })
}

/// Token names fall back to `x` when a name slugifies to empty.
fn slugify(s: &str) -> String {
    crate::util::slugify(s, "x")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_solid_fill_color() {
        let n = json!({
            "type": "RECTANGLE", "name": "bg",
            "fills": [{ "type": "SOLID", "color": { "r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0 } }]
        });
        let mut t = Tokens::default();
        collect_from_target(&n, &mut t);
        assert_eq!(t.colors.get("bg"), Some(&"#ff0000".to_string()));
    }

    #[test]
    fn css_format_emits_root_block() {
        let mut t = Tokens::default();
        t.colors.insert("primary".into(), "#ff0000".into());
        let v = render(&t, Format::Css);
        assert!(v.as_str().unwrap().contains("--color-primary: #ff0000"));
    }

    #[test]
    fn slugify_handles_special_chars() {
        assert_eq!(slugify("Primary Button"), "primary-button");
        assert_eq!(slugify("Color/Brand/Red"), "color-brand-red");
    }
}
