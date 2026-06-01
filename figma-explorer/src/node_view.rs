//! Curated node-info output builder.
//!
//! Walks a raw `serde_json::Value` node (as returned by `/v1/files/{key}`)
//! and produces a normalized, LLM-friendly view: layout, fills, strokes,
//! effects, text style, component metadata, prototype, style + variable
//! references. Top-level `variables` and `styles_index` blocks are populated
//! by the same walk via a [`Collector`] so they only include entries actually
//! referenced by the emitted nodes.
//!
//! Conventions:
//! - All output keys are `snake_case`.
//! - Empty arrays and default values are omitted (e.g. `fills: []` doesn't appear).
//! - Bound variables and named styles are emitted as ids only; the lookup is
//!   hoisted to the top-level `variables` / `styles_index` blocks to avoid
//!   per-node duplication of token data.
//! - Children at depth ≥ 1 drop `effects`, `prototype`, `export_settings`,
//!   `dev_status`, `annotations` to keep subtree output compact.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::node::rgba_to_hex;

/// Output-shaping knobs for the curated node view.
#[derive(Clone, Debug)]
pub struct ViewOptions {
    /// Subtree depth limit. `None` = unlimited (default). `Some(0)` = target
    /// only, no children. `Some(n)` = n levels of descendants.
    pub depth: Option<usize>,
    /// Hard cap on emitted descendant count. When hit, the rest are omitted
    /// and a `truncated` marker is added to the top-level output by the
    /// `node-info` command. Defaults to 500 in `node-info`'s Args.
    pub max_nodes: usize,
    /// Include `prototype` block (interactions, transitions, overlay).
    pub prototype: bool,
    /// Include `dev_status`, `annotations`, `export_settings`.
    pub meta: bool,
    /// Include `text.overrides` (per-range style overrides) on TEXT nodes.
    /// `node-info` auto-enables this when overrides differ on non-`fills`
    /// fields; otherwise the user must opt in via `--rich-text`.
    pub rich_text: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            depth: None,
            max_nodes: 500,
            prototype: false,
            meta: false,
            rich_text: false,
        }
    }
}

/// Accumulates which variable / style ids the emitted output referenced.
/// The `node-info` command consults this after the walk to populate the
/// top-level `variables` and `styles_index` blocks with just the referenced
/// entries, avoiding 40× repetition of the same token data in a card grid.
///
/// Also tracks the descendant emission count + the truncation tail so the
/// caller can attach a `truncated` block when `max_nodes` was hit.
#[derive(Debug, Default)]
pub struct Collector {
    /// Set of `VariableID:...` strings encountered via `boundVariables`
    /// (on the node itself, on paints, on effects, etc.).
    pub variables: BTreeSet<String>,
    /// Set of `S:...` style ids encountered via the node's `styles` map.
    pub styles: BTreeSet<String>,
    /// How many descendants have been emitted (excluding the target itself).
    /// Compared against `ViewOptions::max_nodes` by the recursive walker.
    pub emitted_descendants: usize,
    /// Ids of descendants that were dropped because `max_nodes` was hit.
    /// Output by `node-info` under the top-level `truncated` block.
    pub omitted_ids: Vec<String>,
    /// Shapes encountered during the walk that the view builder didn't know
    /// how to render. Fires when Figma adds a paint type, effect/layout
    /// field, bound-variables shape, or variable `resolvedType` we haven't
    /// taught the builder about — so silent data loss becomes a surfaced
    /// `_warnings` block in node-info output instead.
    pub warnings: Vec<UnknownShape>,
}

/// One unrecognized shape encountered during view building. `subject` is the
/// node id, variable id, or whatever the warning is attributed to; empty when
/// no obvious attribution applies.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UnknownShape {
    pub location: String,
    pub subject: String,
    pub detail: String,
}

impl Collector {
    pub fn truncated(&self) -> bool {
        !self.omitted_ids.is_empty()
    }

    /// Record an unknown-shape warning. De-duplicated on the (location,
    /// detail) pair so a single Figma schema gap doesn't fire once per node.
    pub fn record_unknown(&mut self, location: &str, subject: &str, detail: impl Into<String>) {
        let detail = detail.into();
        let dedupe_key = (location, detail.as_str());
        if self
            .warnings
            .iter()
            .any(|w| (w.location.as_str(), w.detail.as_str()) == dedupe_key)
        {
            return;
        }
        self.warnings.push(UnknownShape {
            location: location.to_owned(),
            subject: subject.to_owned(),
            detail,
        });
    }
}

/// Paint `type` strings the view builder knows how to render. Anything not in
/// this list lands in `_warnings` so an added Figma paint kind doesn't
/// silently emit a stub object.
const KNOWN_PAINT_TYPES: &[&str] = &[
    "SOLID",
    "GRADIENT_LINEAR",
    "GRADIENT_RADIAL",
    "GRADIENT_ANGULAR",
    "GRADIENT_DIAMOND",
    "IMAGE",
    "PATTERN",
];

/// Effect-object keys the view builder consumes; any other key on an effect
/// object that isn't on this allow-list raises a warning.
const KNOWN_EFFECT_KEYS: &[&str] = &[
    "type",
    "visible",
    "color",
    "offset",
    "radius",
    "spread",
    "blendMode",
    "showShadowBehindNode",
    "boundVariables",
];

/// Node-level keys consumed by the layout builder. Membership is checked
/// against keys whose prefix matches `LAYOUT_KEY_PREFIXES`, so unrelated
/// node keys (id, name, fills, …) aren't flagged.
const KNOWN_LAYOUT_KEYS: &[&str] = &[
    "layoutMode",
    "layoutWrap",
    "layoutGrids",
    "layoutAlign",
    "layoutGrow",
    "layoutPositioning",
    "layoutSizingHorizontal",
    "layoutSizingVertical",
    "primaryAxisSizingMode",
    "primaryAxisAlignItems",
    "primaryAxisMinSize",
    "primaryAxisMaxSize",
    "counterAxisSizingMode",
    "counterAxisAlignItems",
    "counterAxisAlignContent",
    "counterAxisSpacing",
    "paddingTop",
    "paddingRight",
    "paddingBottom",
    "paddingLeft",
    "itemSpacing",
];

const LAYOUT_KEY_PREFIXES: &[&str] = &[
    "layout",
    "primaryAxis",
    "counterAxis",
    "padding",
    "itemSpacing",
];

/// Variable `resolvedType` values the view builder renders without warning.
/// Other values pass the raw payload through unchanged but surface a warning
/// so the LLM consumer knows the value wasn't normalized.
const KNOWN_RESOLVED_TYPES: &[&str] = &["COLOR", "FLOAT", "STRING", "BOOLEAN"];

// ───────────────────────────────────────────────────────────────────────────
// Top-level entry point
// ───────────────────────────────────────────────────────────────────────────

/// Build the curated view for `node`. Recurses into children up to
/// `opts.depth` and `opts.max_nodes`. Mutates `collector` to record which
/// variable / style ids were referenced and which children (if any) were
/// dropped by the node cap.
pub fn build_node_view(node: &Value, opts: &ViewOptions, collector: &mut Collector) -> Value {
    build_view_recursive(node, opts, collector, /* depth_from_target */ 0)
}

fn build_view_recursive(
    node: &Value,
    opts: &ViewOptions,
    collector: &mut Collector,
    depth: usize,
) -> Value {
    let mut out = Map::new();
    // Attribution for any unknown-shape warnings raised below. Empty when
    // the node lacks an `id` (rare; defensive against bad input).
    let node_id = node.get("id").and_then(Value::as_str).unwrap_or("");

    // ── Identity ───────────────────────────────────────────────────────────
    if let Some(v) = node.get("id") {
        out.insert("id".into(), v.clone());
    }
    if let Some(v) = node.get("type") {
        out.insert("type".into(), v.clone());
    }
    if let Some(v) = node.get("name") {
        out.insert("name".into(), v.clone());
    }

    // ── Common modifiers (omit defaults) ───────────────────────────────────
    if let Some(Value::Bool(false)) = node.get("visible") {
        out.insert("visible".into(), json!(false));
    }
    if let Some(Value::Bool(true)) = node.get("locked") {
        out.insert("locked".into(), json!(true));
    }
    if let Some(r) = node.get("rotation").and_then(Value::as_f64) {
        if r.abs() > 1e-6 {
            out.insert("rotation".into(), json!(r));
        }
    }
    if let Some(o) = node.get("opacity").and_then(Value::as_f64) {
        if (o - 1.0).abs() > 1e-6 {
            out.insert("opacity".into(), json!(o));
        }
    }
    if let Some(bm) = node.get("blendMode").and_then(Value::as_str) {
        if bm != "PASS_THROUGH" && bm != "NORMAL" {
            out.insert("blend_mode".into(), json!(bm));
        }
    }
    if let Some(Value::Bool(true)) = node.get("preserveRatio") {
        out.insert("preserve_ratio".into(), json!(true));
    }
    if let Some(Value::Bool(true)) = node.get("isMask") {
        out.insert("is_mask".into(), json!(true));
        if let Some(mt) = node.get("maskType") {
            out.insert("mask_type".into(), mt.clone());
        }
    }

    // ── Geometry ───────────────────────────────────────────────────────────
    if let Some(b) = node.get("absoluteBoundingBox") {
        out.insert("bounds".into(), b.clone());
    }
    if let Some(s) = node.get("size") {
        out.insert("size".into(), s.clone());
    }
    if let Some(t) = node.get("relativeTransform") {
        // Only emit when non-identity (any non-zero off-diagonal or non-1/0).
        if !is_identity_transform(t) {
            out.insert("relative_transform".into(), t.clone());
        }
    }
    if let Some(c) = node.get("constraints") {
        out.insert("constraints".into(), c.clone());
    }
    // Size constraints (used in responsive auto-layout).
    if let Some(min_w) = node.get("minWidth") {
        merge_size_constraint(&mut out, "min_width", min_w);
    }
    if let Some(max_w) = node.get("maxWidth") {
        merge_size_constraint(&mut out, "max_width", max_w);
    }
    if let Some(min_h) = node.get("minHeight") {
        merge_size_constraint(&mut out, "min_height", min_h);
    }
    if let Some(max_h) = node.get("maxHeight") {
        merge_size_constraint(&mut out, "max_height", max_h);
    }

    // ── Corner radius ──────────────────────────────────────────────────────
    if let Some(arr) = node.get("rectangleCornerRadii").and_then(Value::as_array) {
        out.insert(
            "corner".into(),
            json!({ "rectangle_corner_radii": arr.clone() }),
        );
    } else if let Some(r) = node.get("cornerRadius").and_then(Value::as_f64) {
        if r > 0.0 {
            out.insert("corner".into(), json!({ "radius": r }));
        }
    }
    if let Some(Value::Bool(true)) = node.get("clipsContent") {
        out.insert("clips_content".into(), json!(true));
    }

    // ── Fills / strokes / effects ──────────────────────────────────────────
    if let Some(arr) = node.get("fills").and_then(Value::as_array) {
        let paints = build_paints(arr, collector, node_id);
        if !paints.is_empty() {
            out.insert("fills".into(), Value::Array(paints));
        }
    }
    if let Some(arr) = node.get("strokes").and_then(Value::as_array) {
        let paints = build_paints(arr, collector, node_id);
        if !paints.is_empty() {
            out.insert("strokes".into(), Value::Array(paints));
            // Companion `stroke` block (weight/align/join/cap/dashes).
            let stroke = build_stroke(node);
            if !stroke.is_empty() {
                out.insert("stroke".into(), Value::Object(stroke));
            }
        }
    }

    // Effects are noise for non-target depths. Keep on target, drop deeper.
    if depth == 0 {
        if let Some(arr) = node.get("effects").and_then(Value::as_array) {
            let effects = build_effects(arr, collector, node_id);
            if !effects.is_empty() {
                out.insert("effects".into(), Value::Array(effects));
            }
        }
    }

    // ── Layout (auto-layout) ───────────────────────────────────────────────
    if let Some(mode) = node.get("layoutMode").and_then(Value::as_str) {
        if mode != "NONE" {
            out.insert("layout".into(), build_layout(node, collector, node_id));
        }
    }
    let layout_child = build_layout_child(node);
    if !layout_child.is_empty() {
        out.insert("layout_child".into(), Value::Object(layout_child));
    }
    if let Some(grids) = node.get("layoutGrids").and_then(Value::as_array) {
        if !grids.is_empty() && depth == 0 {
            out.insert("layout_grids".into(), Value::Array(grids.clone()));
        }
    }

    // ── Text ────────────────────────────────────────────────────────────────
    if node.get("type").and_then(Value::as_str) == Some("TEXT") {
        if let Some(text) = build_text(node, opts, collector) {
            out.insert("text".into(), text);
        }
    }

    // ── Component / instance metadata ──────────────────────────────────────
    let component = build_component(node);
    if !component.is_empty() {
        out.insert("component".into(), Value::Object(component));
    }
    if let Some(refs) = node.get("componentPropertyReferences") {
        if !refs.is_null() && refs.as_object().is_some_and(|m| !m.is_empty()) {
            out.insert("property_refs".into(), refs.clone());
        }
    }

    // ── Prototype (opt-in, but always emit when this node starts a flow) ───
    if opts.prototype || node.get("prototypeStartNodeID").is_some() {
        if let Some(proto) = build_prototype(node, opts.prototype) {
            if !proto.as_object().is_none_or(Map::is_empty) {
                out.insert("prototype".into(), proto);
            }
        }
    }

    // ── Meta (dev status, annotations, export settings) — opt-in ───────────
    if opts.meta {
        if let Some(ds) = node.get("devStatus") {
            if !ds.is_null() {
                out.insert("dev_status".into(), ds.clone());
            }
        }
        if let Some(arr) = node.get("annotations").and_then(Value::as_array) {
            if !arr.is_empty() {
                out.insert("annotations".into(), Value::Array(arr.clone()));
            }
        }
        if let Some(arr) = node.get("exportSettings").and_then(Value::as_array) {
            if !arr.is_empty() {
                out.insert("export_settings".into(), Value::Array(arr.clone()));
            }
        }
    }

    // ── Style + variable references ────────────────────────────────────────
    if let Some(styles_map) = node.get("styles").and_then(Value::as_object) {
        // Pass the styles map through as-is (small) and collect ids so the
        // top-level `styles_index` block resolves them.
        let mut out_styles = Map::with_capacity(styles_map.len());
        for (k, v) in styles_map {
            if let Some(s) = v.as_str() {
                collector.styles.insert(s.to_owned());
            }
            out_styles.insert(k.clone(), v.clone());
        }
        if !out_styles.is_empty() {
            out.insert("styles".into(), Value::Object(out_styles));
        }
    }
    if let Some(bv) = node.get("boundVariables") {
        let flat = flatten_bound_variables(bv, collector, node_id);
        if let Some(map) = flat.as_object() {
            for v in map.values() {
                if let Some(s) = v.as_str() {
                    collector.variables.insert(s.to_owned());
                }
            }
            if !map.is_empty() {
                out.insert("bound_variables".into(), flat);
            }
        }
    }
    if let Some(emodes) = node.get("explicitVariableModes") {
        if !emodes.is_null() && emodes.as_object().is_some_and(|m| !m.is_empty()) {
            out.insert("explicit_variable_modes".into(), emodes.clone());
        }
    }

    // ── Children ───────────────────────────────────────────────────────────
    let allow_deeper = opts.depth.is_none_or(|d| depth < d);
    if allow_deeper {
        if let Some(arr) = node.get("children").and_then(Value::as_array) {
            let mut out_children: Vec<Value> = Vec::new();
            for child in arr {
                if collector.emitted_descendants >= opts.max_nodes {
                    if let Some(id) = child.get("id").and_then(Value::as_str) {
                        collector.omitted_ids.push(id.to_owned());
                    }
                    continue;
                }
                collector.emitted_descendants += 1;
                let v = build_view_recursive(child, opts, collector, depth + 1);
                out_children.push(v);
            }
            if !out_children.is_empty() {
                out.insert("children".into(), Value::Array(out_children));
            }
        }
    }

    Value::Object(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Paints
// ───────────────────────────────────────────────────────────────────────────

fn build_paints(arr: &[Value], collector: &mut Collector, node_id: &str) -> Vec<Value> {
    arr.iter()
        .map(|p| build_paint(p, collector, node_id))
        .collect()
}

fn build_paint(paint: &Value, collector: &mut Collector, node_id: &str) -> Value {
    let mut out = Map::new();
    if let Some(t) = paint.get("type") {
        out.insert("type".into(), t.clone());
    }
    if let Some(Value::Bool(false)) = paint.get("visible") {
        out.insert("visible".into(), json!(false));
    }
    if let Some(o) = paint.get("opacity").and_then(Value::as_f64) {
        if (o - 1.0).abs() > 1e-6 {
            out.insert("opacity".into(), json!(o));
        }
    }
    if let Some(bm) = paint.get("blendMode").and_then(Value::as_str) {
        if bm != "NORMAL" {
            out.insert("blend_mode".into(), json!(bm));
        }
    }

    let ty = paint.get("type").and_then(Value::as_str).unwrap_or("");
    if !ty.is_empty() && !KNOWN_PAINT_TYPES.contains(&ty) {
        collector.record_unknown("paint.type", node_id, ty);
    }
    match ty {
        "SOLID" => {
            if let Some(c) = paint.get("color") {
                out.insert("color".into(), c.clone());
                out.insert("hex".into(), json!(rgba_to_hex(c)));
            }
            // Inline `boundVariables.color` if present.
            if let Some(bv) = paint.get("boundVariables").and_then(Value::as_object) {
                if let Some(color_alias) = bv
                    .get("color")
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                {
                    collector.variables.insert(color_alias.to_owned());
                    out.insert("bound_variable".into(), json!(color_alias));
                }
            }
        }
        t if t.starts_with("GRADIENT_") => {
            if let Some(h) = paint.get("gradientHandlePositions") {
                out.insert("gradient_handles".into(), h.clone());
            }
            if let Some(stops) = paint.get("gradientStops").and_then(Value::as_array) {
                let out_stops: Vec<Value> = stops
                    .iter()
                    .map(|s| build_gradient_stop(s, collector))
                    .collect();
                out.insert("stops".into(), Value::Array(out_stops));
            }
        }
        "IMAGE" => {
            for (key, out_key) in [
                ("imageRef", "image_ref"),
                ("scaleMode", "scale_mode"),
                ("imageTransform", "image_transform"),
                ("scalingFactor", "scaling_factor"),
                ("filters", "filters"),
                ("gifRef", "gif_ref"),
                ("rotation", "rotation"),
            ] {
                if let Some(v) = paint.get(key) {
                    if !v.is_null() {
                        out.insert(out_key.into(), v.clone());
                    }
                }
            }
        }
        "PATTERN" => {
            for (key, out_key) in [
                ("sourceNodeId", "source_node_id"),
                ("tileType", "tile_type"),
                ("horizontalAlignment", "horizontal_alignment"),
                ("verticalAlignment", "vertical_alignment"),
                ("spacing", "spacing"),
                ("scalingFactor", "scaling_factor"),
            ] {
                if let Some(v) = paint.get(key) {
                    if !v.is_null() {
                        out.insert(out_key.into(), v.clone());
                    }
                }
            }
        }
        _ => {}
    }
    Value::Object(out)
}

fn build_gradient_stop(stop: &Value, collector: &mut Collector) -> Value {
    let mut out = Map::new();
    if let Some(p) = stop.get("position") {
        out.insert("position".into(), p.clone());
    }
    if let Some(c) = stop.get("color") {
        out.insert("color".into(), c.clone());
        out.insert("hex".into(), json!(rgba_to_hex(c)));
    }
    if let Some(bv) = stop.get("boundVariables").and_then(Value::as_object) {
        if let Some(alias) = bv
            .get("color")
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
        {
            collector.variables.insert(alias.to_owned());
            out.insert("bound_variable".into(), json!(alias));
        }
    }
    Value::Object(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Stroke (the companion block to `strokes`)
// ───────────────────────────────────────────────────────────────────────────

fn build_stroke(node: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(w) = node.get("strokeWeight") {
        out.insert("weight".into(), w.clone());
    }
    if let Some(weights) = node
        .get("individualStrokeWeights")
        .and_then(Value::as_object)
    {
        let mut sides = Map::new();
        for k in ["top", "right", "bottom", "left"] {
            if let Some(v) = weights.get(k) {
                sides.insert(k.into(), v.clone());
            }
        }
        if !sides.is_empty() {
            out.insert("weights".into(), Value::Object(sides));
        }
    }
    if let Some(a) = node.get("strokeAlign") {
        out.insert("align".into(), a.clone());
    }
    if let Some(j) = node.get("strokeJoin") {
        out.insert("join".into(), j.clone());
    }
    if let Some(c) = node.get("strokeCap") {
        out.insert("cap".into(), c.clone());
    }
    if let Some(d) = node.get("strokeDashes").and_then(Value::as_array) {
        if !d.is_empty() {
            out.insert("dashes".into(), Value::Array(d.clone()));
        }
    }
    if let Some(m) = node.get("strokeMiterAngle") {
        out.insert("miter_angle".into(), m.clone());
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// Effects
// ───────────────────────────────────────────────────────────────────────────

fn build_effects(arr: &[Value], collector: &mut Collector, node_id: &str) -> Vec<Value> {
    arr.iter()
        .map(|e| build_effect(e, collector, node_id))
        .collect()
}

fn build_effect(effect: &Value, collector: &mut Collector, node_id: &str) -> Value {
    let mut out = Map::new();
    if let Some(t) = effect.get("type") {
        out.insert("type".into(), t.clone());
    }
    if let Some(Value::Bool(false)) = effect.get("visible") {
        out.insert("visible".into(), json!(false));
    }
    for (key, out_key) in [
        ("color", "color"),
        ("offset", "offset"),
        ("radius", "radius"),
        ("spread", "spread"),
        ("blendMode", "blend_mode"),
        ("showShadowBehindNode", "show_shadow_behind_node"),
    ] {
        if let Some(v) = effect.get(key) {
            if !v.is_null() {
                out.insert(out_key.into(), v.clone());
            }
        }
    }
    if let Some(obj) = effect.as_object() {
        for k in obj.keys() {
            if !KNOWN_EFFECT_KEYS.contains(&k.as_str()) {
                collector.record_unknown("effect.field", node_id, k.as_str());
            }
        }
    }
    if let Some(c) = effect.get("color") {
        out.insert("hex".into(), json!(rgba_to_hex(c)));
    }
    if let Some(bv) = effect.get("boundVariables").and_then(Value::as_object) {
        // Shadows can bind color/offsetX/Y/radius/spread.
        let mut bound = Map::new();
        for (k, v) in bv {
            if let Some(id) = v.get("id").and_then(Value::as_str) {
                collector.variables.insert(id.to_owned());
                bound.insert(k.clone(), json!(id));
            }
        }
        if !bound.is_empty() {
            out.insert("bound_variables".into(), Value::Object(bound));
        }
    }
    Value::Object(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Layout (auto-layout block)
// ───────────────────────────────────────────────────────────────────────────

fn build_layout(node: &Value, collector: &mut Collector, node_id: &str) -> Value {
    let mut out = Map::new();
    if let Some(obj) = node.as_object() {
        for k in obj.keys() {
            let prefix_match = LAYOUT_KEY_PREFIXES.iter().any(|p| k.starts_with(p));
            if prefix_match && !KNOWN_LAYOUT_KEYS.contains(&k.as_str()) {
                collector.record_unknown("layout.field", node_id, k.as_str());
            }
        }
    }
    if let Some(m) = node.get("layoutMode") {
        out.insert("mode".into(), m.clone());
    }

    let mut primary = Map::new();
    if let Some(v) = node.get("primaryAxisSizingMode") {
        primary.insert("sizing".into(), v.clone());
    }
    if let Some(v) = node.get("primaryAxisAlignItems") {
        primary.insert("align".into(), v.clone());
    }
    if let Some(v) = node.get("primaryAxisMinSize") {
        primary.insert("min".into(), v.clone());
    }
    if let Some(v) = node.get("primaryAxisMaxSize") {
        primary.insert("max".into(), v.clone());
    }
    if !primary.is_empty() {
        out.insert("primary_axis".into(), Value::Object(primary));
    }

    let mut counter = Map::new();
    if let Some(v) = node.get("counterAxisSizingMode") {
        counter.insert("sizing".into(), v.clone());
    }
    if let Some(v) = node.get("counterAxisAlignItems") {
        counter.insert("align".into(), v.clone());
    }
    if let Some(v) = node.get("counterAxisAlignContent") {
        counter.insert("align_content".into(), v.clone());
    }
    if let Some(v) = node.get("counterAxisSpacing") {
        counter.insert("spacing".into(), v.clone());
    }
    if !counter.is_empty() {
        out.insert("counter_axis".into(), Value::Object(counter));
    }

    if let Some(w) = node.get("layoutWrap") {
        out.insert("wrap".into(), w.clone());
    }
    if let Some(s) = node.get("itemSpacing") {
        out.insert("item_spacing".into(), s.clone());
    }

    let mut padding = Map::new();
    for (key, out_key) in [
        ("paddingTop", "top"),
        ("paddingRight", "right"),
        ("paddingBottom", "bottom"),
        ("paddingLeft", "left"),
    ] {
        if let Some(v) = node.get(key) {
            padding.insert(out_key.into(), v.clone());
        }
    }
    if !padding.is_empty() {
        out.insert("padding".into(), Value::Object(padding));
    }

    Value::Object(out)
}

fn build_layout_child(node: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(v) = node.get("layoutAlign") {
        let s = v.as_str().unwrap_or("");
        if s != "INHERIT" {
            out.insert("align".into(), v.clone());
        }
    }
    if let Some(v) = node.get("layoutGrow").and_then(Value::as_f64) {
        if v != 0.0 {
            out.insert("grow".into(), json!(v));
        }
    }
    if let Some(v) = node.get("layoutPositioning") {
        let s = v.as_str().unwrap_or("");
        if s != "AUTO" {
            out.insert("positioning".into(), v.clone());
        }
    }
    if let Some(v) = node.get("layoutSizingHorizontal") {
        out.insert("sizing_horizontal".into(), v.clone());
    }
    if let Some(v) = node.get("layoutSizingVertical") {
        out.insert("sizing_vertical".into(), v.clone());
    }
    // GRID parent — child grid placement.
    let mut grid = Map::new();
    for (key, out_key) in [
        ("gridRowSpan", "row_span"),
        ("gridColumnSpan", "column_span"),
        ("gridRowAnchorIndex", "row_anchor"),
        ("gridColumnAnchorIndex", "column_anchor"),
    ] {
        if let Some(v) = node.get(key) {
            grid.insert(out_key.into(), v.clone());
        }
    }
    if !grid.is_empty() {
        out.insert("grid".into(), Value::Object(grid));
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// Text
// ───────────────────────────────────────────────────────────────────────────

fn build_text(node: &Value, opts: &ViewOptions, collector: &mut Collector) -> Option<Value> {
    let mut out = Map::new();
    let node_id = node.get("id").and_then(Value::as_str).unwrap_or("");
    if let Some(c) = node.get("characters") {
        out.insert("characters".into(), c.clone());
    }
    if let Some(style) = node.get("style") {
        out.insert("style".into(), build_type_style(style, collector, node_id));
    }
    if opts.rich_text {
        let has_overrides = node
            .get("characterStyleOverrides")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty());
        if has_overrides {
            let ranges = derive_text_override_ranges(node);
            if !ranges.is_empty() {
                out.insert(
                    "overrides".into(),
                    Value::Array(
                        ranges
                            .into_iter()
                            .map(|(start, end, style_idx)| {
                                let style = node
                                    .get("styleOverrideTable")
                                    .and_then(Value::as_object)
                                    .and_then(|t| t.get(&style_idx.to_string()))
                                    .cloned()
                                    .unwrap_or(Value::Null);
                                json!({
                                    "range": [start, end],
                                    "style_index": style_idx,
                                    "style": if style.is_null() {
                                        Value::Null
                                    } else {
                                        build_type_style(&style, collector, node_id)
                                    },
                                })
                            })
                            .collect(),
                    ),
                );
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

fn build_type_style(style: &Value, collector: &mut Collector, node_id: &str) -> Value {
    let mut out = Map::new();
    for (key, out_key) in [
        ("fontFamily", "font_family"),
        ("fontPostScriptName", "font_post_script_name"),
        ("fontStyle", "font_style"),
        ("italic", "italic"),
        ("fontWeight", "font_weight"),
        ("fontSize", "font_size"),
        ("textCase", "text_case"),
        ("textAlignHorizontal", "text_align_horizontal"),
        ("textAlignVertical", "text_align_vertical"),
        ("letterSpacing", "letter_spacing"),
        ("lineHeightPx", "line_height_px"),
        ("lineHeightPercent", "line_height_percent"),
        ("lineHeightPercentFontSize", "line_height_percent_font_size"),
        ("lineHeightUnit", "line_height_unit"),
        ("paragraphSpacing", "paragraph_spacing"),
        ("paragraphIndent", "paragraph_indent"),
        ("listSpacing", "list_spacing"),
        ("textDecoration", "text_decoration"),
        ("textAutoResize", "text_auto_resize"),
        ("textTruncation", "text_truncation"),
        ("maxLines", "max_lines"),
        ("hyperlink", "hyperlink"),
    ] {
        if let Some(v) = style.get(key) {
            if !v.is_null() {
                out.insert(out_key.into(), v.clone());
            }
        }
    }
    if let Some(flags) = style.get("opentypeFlags").and_then(Value::as_object) {
        if !flags.is_empty() {
            out.insert("opentype_flags".into(), Value::Object(flags.clone()));
        }
    }
    if let Some(arr) = style.get("fills").and_then(Value::as_array) {
        let paints = build_paints(arr, collector, node_id);
        if !paints.is_empty() {
            out.insert("fills".into(), Value::Array(paints));
        }
    }
    // Per-style boundVariables (e.g. text color bound to a token).
    if let Some(bv) = style.get("boundVariables") {
        let flat = flatten_bound_variables(bv, collector, node_id);
        if let Some(map) = flat.as_object() {
            for v in map.values() {
                if let Some(s) = v.as_str() {
                    collector.variables.insert(s.to_owned());
                }
            }
            if !map.is_empty() {
                out.insert("bound_variables".into(), flat);
            }
        }
    }
    Value::Object(out)
}

/// Derive contiguous (start, end_exclusive, style_index) ranges from a TEXT
/// node's `characterStyleOverrides`. `0` is the base style and is skipped.
fn derive_text_override_ranges(node: &Value) -> Vec<(usize, usize, u64)> {
    let arr = match node
        .get("characterStyleOverrides")
        .and_then(Value::as_array)
    {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut ranges: Vec<(usize, usize, u64)> = Vec::new();
    let mut i = 0;
    while i < arr.len() {
        let style_idx = arr[i].as_u64().unwrap_or(0);
        if style_idx == 0 {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < arr.len() && arr[j].as_u64().unwrap_or(0) == style_idx {
            j += 1;
        }
        ranges.push((i, j, style_idx));
        i = j;
    }
    ranges
}

// ───────────────────────────────────────────────────────────────────────────
// Component / instance metadata
// ───────────────────────────────────────────────────────────────────────────

fn build_component(node: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let ty = node.get("type").and_then(Value::as_str).unwrap_or("");
    let is_relevant = matches!(ty, "INSTANCE" | "COMPONENT" | "COMPONENT_SET");
    if !is_relevant {
        return out;
    }
    for (key, out_key) in [
        ("componentId", "component_id"),
        ("componentKey", "component_key"),
        ("componentSetId", "component_set_id"),
        ("isExposedInstance", "is_exposed_instance"),
        ("mainComponent", "main_component"),
    ] {
        if let Some(v) = node.get(key) {
            if !v.is_null() {
                out.insert(out_key.into(), v.clone());
            }
        }
    }
    // Figma puts the variant property assignment on `variantProperties`
    // (older shape) or inside `componentProperties` with `type: VARIANT`.
    if let Some(vp) = node.get("variantProperties") {
        if !vp.is_null() {
            out.insert("variant_properties".into(), vp.clone());
        }
    }
    if let Some(cp) = node.get("componentProperties") {
        if !cp.is_null() {
            out.insert("component_properties".into(), cp.clone());
        }
    }
    if let Some(pd) = node.get("componentPropertyDefinitions") {
        if !pd.is_null() {
            out.insert("property_definitions".into(), pd.clone());
        }
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// Prototype
// ───────────────────────────────────────────────────────────────────────────

fn build_prototype(node: &Value, include_interactions: bool) -> Option<Value> {
    let mut out = Map::new();
    if node.get("prototypeStartNodeID").is_some() {
        out.insert("is_flow_start".into(), json!(true));
    }
    if include_interactions {
        if let Some(arr) = node.get("interactions").and_then(Value::as_array) {
            if !arr.is_empty() {
                out.insert("interactions".into(), Value::Array(arr.clone()));
            }
        }
        if let Some(v) = node.get("transitionNodeID") {
            if !v.is_null() {
                out.insert("transition_node_id".into(), v.clone());
            }
        }
        for (key, out_key) in [
            (
                "overlayBackgroundInteraction",
                "overlay_background_interaction",
            ),
            (
                "overlayBackgroundAppearance",
                "overlay_background_appearance",
            ),
            ("overlayPositionType", "overlay_position_type"),
        ] {
            if let Some(v) = node.get(key) {
                if !v.is_null() {
                    out.insert(out_key.into(), v.clone());
                }
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Object(out))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Bound variables flattening
// ───────────────────────────────────────────────────────────────────────────

/// Flatten Figma's `boundVariables` shape into a simple `{ property_path: id }`
/// map. Handles three layouts:
/// - `{ cornerRadius: { id, type: VARIABLE_ALIAS } }` → `cornerRadius -> id`
/// - `{ fills: [{ id, type }, { id, type }] }` → `fills[0] -> id`, `fills[1] -> id`
/// - `{ characters: { id } }` → string-property bindings on TEXT nodes.
fn flatten_bound_variables(bv: &Value, collector: &mut Collector, subject: &str) -> Value {
    let mut out = Map::new();
    let obj = match bv.as_object() {
        Some(o) => o,
        None => {
            collector.record_unknown(
                "bound_variables.root",
                subject,
                format!("expected object, got {}", value_kind(bv)),
            );
            return Value::Object(out);
        }
    };
    for (key, value) in obj {
        if let Some(arr) = value.as_array() {
            for (i, entry) in arr.iter().enumerate() {
                if let Some(id) = entry.get("id").and_then(Value::as_str) {
                    out.insert(format!("{key}[{i}]"), json!(id));
                } else {
                    collector.record_unknown(
                        "bound_variables.entry",
                        subject,
                        format!("{key}[{i}] has no .id"),
                    );
                }
            }
        } else if let Some(id) = value.get("id").and_then(Value::as_str) {
            out.insert(key.clone(), json!(id));
        } else {
            collector.record_unknown(
                "bound_variables.entry",
                subject,
                format!("{key} has no .id and is not an array"),
            );
        }
    }
    Value::Object(out)
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Top-level blocks (variables index, styles index)
// ───────────────────────────────────────────────────────────────────────────

/// Build the top-level `variables` block from the variables sidecar, but
/// only for ids that the emitted nodes referenced. `vars_root` is the raw
/// `/v1/files/{key}/variables/local` response or `None` if no sidecar.
pub fn build_variables_block(
    vars_root: Option<&Value>,
    refs: &BTreeSet<String>,
    collector: &mut Collector,
) -> Value {
    if refs.is_empty() {
        return Value::Object(Map::new());
    }
    let Some(root) = vars_root else {
        return Value::Object(Map::new());
    };
    let meta = root.get("meta").unwrap_or(root);
    let variables = meta.get("variables").and_then(Value::as_object);
    let collections = meta.get("variableCollections").and_then(Value::as_object);

    // Worklist over the closure of `refs` under alias edges: building a
    // variable entry can record alias-target ids into `collector.variables`
    // (see `resolve_variable_value`), and those targets need their own entries
    // too. We can't just iterate `refs` because it's an immutable snapshot
    // taken before any entry was built. Accumulate into a BTreeMap so the
    // output stays sorted regardless of discovery order.
    let mut entries: BTreeMap<String, Value> = BTreeMap::new();
    let mut seen: BTreeSet<String> = refs.iter().cloned().collect();
    let mut queue: Vec<String> = refs.iter().cloned().collect();
    while let Some(id) = queue.pop() {
        let Some(v) = variables.and_then(|m| m.get(&id)) else {
            continue;
        };
        let entry = build_variable_entry(v, collections, collector, &id);
        entries.insert(id, entry);
        let new_ids: Vec<String> = collector
            .variables
            .iter()
            .filter(|v| !seen.contains(*v))
            .cloned()
            .collect();
        for nid in new_ids {
            seen.insert(nid.clone());
            queue.push(nid);
        }
    }
    let mut out = Map::new();
    for (id, entry) in entries {
        out.insert(id, entry);
    }
    Value::Object(out)
}

fn build_variable_entry(
    v: &Value,
    collections: Option<&Map<String, Value>>,
    collector: &mut Collector,
    subject: &str,
) -> Value {
    let mut out = Map::new();
    if let Some(name) = v.get("name") {
        out.insert("name".into(), name.clone());
    }
    if let Some(rt) = v.get("resolvedType") {
        out.insert("resolved_type".into(), rt.clone());
    }
    if let Some(scopes) = v.get("scopes") {
        out.insert("scopes".into(), scopes.clone());
    }
    if let Some(cs) = v.get("codeSyntax") {
        if !cs.is_null() && cs.as_object().is_some_and(|m| !m.is_empty()) {
            out.insert("code_syntax".into(), cs.clone());
        }
    }
    let collection_id = v.get("variableCollectionId").and_then(Value::as_str);
    let collection = collection_id.and_then(|cid| collections.and_then(|m| m.get(cid)));
    let mode_names: BTreeMap<String, String> = collection
        .and_then(|c| c.get("modes").and_then(Value::as_array))
        .map(|modes| {
            modes
                .iter()
                .filter_map(|m| {
                    let id = m.get("modeId").and_then(Value::as_str)?;
                    let name = m.get("name").and_then(Value::as_str)?;
                    Some((id.to_owned(), name.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(c) = collection {
        let mut cmeta = Map::new();
        if let Some(id) = collection_id {
            cmeta.insert("id".into(), json!(id));
        }
        if let Some(n) = c.get("name") {
            cmeta.insert("name".into(), n.clone());
        }
        if let Some(default_mode_id) = c.get("defaultModeId").and_then(Value::as_str) {
            cmeta.insert(
                "default_mode".into(),
                json!(mode_names
                    .get(default_mode_id)
                    .cloned()
                    .unwrap_or_else(|| default_mode_id.to_owned())),
            );
        }
        if let Some(modes) = c.get("modes").and_then(Value::as_array) {
            let modes_out: Vec<Value> = modes
                .iter()
                .map(|m| {
                    json!({
                        "id": m.get("modeId").cloned().unwrap_or(Value::Null),
                        "name": m.get("name").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            cmeta.insert("modes".into(), Value::Array(modes_out));
        }
        out.insert("collection".into(), Value::Object(cmeta));
    }
    let resolved_type = v.get("resolvedType").and_then(Value::as_str).unwrap_or("");
    if let Some(values_by_mode) = v.get("valuesByMode").and_then(Value::as_object) {
        let mut vbm = Map::new();
        for (mode_id, raw_val) in values_by_mode {
            let key = mode_names
                .get(mode_id)
                .cloned()
                .unwrap_or_else(|| mode_id.clone());
            vbm.insert(
                key,
                resolve_variable_value(raw_val, resolved_type, collector, subject),
            );
        }
        if !vbm.is_empty() {
            out.insert("values_by_mode".into(), Value::Object(vbm));
        }
    }
    Value::Object(out)
}

/// Convert a raw variable value to its display form. COLOR values become
/// hex strings; FLOAT/BOOLEAN/STRING pass through; aliases surface as
/// `{ alias: <id> }`. Unknown `resolved_type` strings raise a warning so the
/// caller can tell the value wasn't normalized.
fn resolve_variable_value(
    raw: &Value,
    resolved_type: &str,
    collector: &mut Collector,
    subject: &str,
) -> Value {
    // Alias indirection: {"type": "VARIABLE_ALIAS", "id": "..."}. Record the
    // aliased variable id so `build_variables_block` pulls its definition into
    // the output too — otherwise the value references an id with no entry.
    if let Some(obj) = raw.as_object() {
        if obj.get("type").and_then(Value::as_str) == Some("VARIABLE_ALIAS") {
            if let Some(id) = obj.get("id") {
                if let Some(id_str) = id.as_str() {
                    collector.variables.insert(id_str.to_owned());
                }
                return json!({ "alias": id.clone() });
            }
        }
    }
    if !resolved_type.is_empty() && !KNOWN_RESOLVED_TYPES.contains(&resolved_type) {
        collector.record_unknown("variable.resolved_type", subject, resolved_type);
    }
    match resolved_type {
        "COLOR" => json!(rgba_to_hex(raw)),
        _ => raw.clone(),
    }
}

/// Build the top-level `styles_index` block from the file's `styles` map,
/// keyed by style id, but only for the ids that emitted nodes referenced.
pub fn build_styles_index_block(file_root: &Value, refs: &BTreeSet<String>) -> Value {
    if refs.is_empty() {
        return Value::Object(Map::new());
    }
    let Some(styles) = file_root.get("styles").and_then(Value::as_object) else {
        return Value::Object(Map::new());
    };
    let mut out = Map::new();
    for id in refs {
        if let Some(entry) = styles.get(id) {
            // Normalize the style record to snake_case while keeping all
            // useful fields. Figma's shape: { key, name, description, styleType }.
            let mut s = Map::new();
            for (key, out_key) in [
                ("key", "key"),
                ("name", "name"),
                ("description", "description"),
                ("remote", "remote"),
                ("styleType", "type"),
            ] {
                if let Some(v) = entry.get(key) {
                    if !v.is_null() {
                        s.insert(out_key.into(), v.clone());
                    }
                }
            }
            out.insert(id.clone(), Value::Object(s));
        }
    }
    Value::Object(out)
}

// ───────────────────────────────────────────────────────────────────────────
// File-level summaries (used by node-info on file:N targets)
// ───────────────────────────────────────────────────────────────────────────

/// Build the `file_summary` block for a file target: counts, page list,
/// components, component sets, styles, variable collections, recent
/// comments.
///
/// `pages_cap` / `components_cap` / `styles_cap` cap each list to a
/// reasonable size; the JSON contains the totals in `counts`.
pub fn build_file_summary(
    file_root: &Value,
    vars_root: Option<&Value>,
    comments_count: usize,
    recent_comments: Option<Value>,
) -> Value {
    let pages: Vec<Value> = file_root
        .get("document")
        .and_then(|d| d.get("children"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|p| {
                    json!({
                        "id": p.get("id").cloned().unwrap_or(Value::Null),
                        "name": p.get("name").cloned().unwrap_or(Value::Null),
                        "child_count": p.get("children").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let components = file_root
        .get("components")
        .and_then(Value::as_object)
        .map(|m| {
            let entries: Vec<Value> = m
                .iter()
                .take(50)
                .map(|(id, v)| {
                    json!({
                        "id": id,
                        "key": v.get("key").cloned().unwrap_or(Value::Null),
                        "name": v.get("name").cloned().unwrap_or(Value::Null),
                        "description": v.get("description").cloned().unwrap_or(Value::Null),
                        "component_set_id": v.get("componentSetId").cloned().unwrap_or(Value::Null),
                        "remote": v.get("remote").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            (m.len(), entries)
        })
        .unwrap_or((0, Vec::new()));

    let component_sets = file_root
        .get("componentSets")
        .and_then(Value::as_object)
        .map(|m| {
            let entries: Vec<Value> = m
                .iter()
                .take(50)
                .map(|(id, v)| {
                    json!({
                        "id": id,
                        "key": v.get("key").cloned().unwrap_or(Value::Null),
                        "name": v.get("name").cloned().unwrap_or(Value::Null),
                        "description": v.get("description").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            (m.len(), entries)
        })
        .unwrap_or((0, Vec::new()));

    let styles = file_root
        .get("styles")
        .and_then(Value::as_object)
        .map(|m| {
            let entries: Vec<Value> = m
                .iter()
                .take(100)
                .map(|(id, v)| {
                    json!({
                        "id": id,
                        "key": v.get("key").cloned().unwrap_or(Value::Null),
                        "name": v.get("name").cloned().unwrap_or(Value::Null),
                        "type": v.get("styleType").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            (m.len(), entries)
        })
        .unwrap_or((0, Vec::new()));

    let (variable_collections, variable_count) = vars_root
        .and_then(|v| v.get("meta"))
        .and_then(|m| m.as_object())
        .map(|meta| {
            let collections = meta
                .get("variableCollections")
                .and_then(Value::as_object)
                .map(|cs| {
                    cs.iter()
                        .map(|(id, c)| {
                            let modes = c.get("modes").and_then(Value::as_array);
                            let default = c.get("defaultModeId").and_then(Value::as_str);
                            let default_name = modes
                                .and_then(|ms| {
                                    ms.iter().find(|m| {
                                        m.get("modeId").and_then(Value::as_str) == default
                                    })
                                })
                                .and_then(|m| m.get("name").cloned());
                            json!({
                                "id": id,
                                "name": c.get("name").cloned().unwrap_or(Value::Null),
                                "modes": modes.cloned().map(Value::Array).unwrap_or(Value::Null),
                                "default_mode": default_name.unwrap_or(Value::Null),
                                "variable_count": c
                                    .get("variableIds")
                                    .and_then(Value::as_array)
                                    .map(Vec::len)
                                    .unwrap_or(0),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let var_count = meta
                .get("variables")
                .and_then(Value::as_object)
                .map(|m| m.len())
                .unwrap_or(0);
            (collections, var_count)
        })
        .unwrap_or_default();

    let total_pages = pages.len();
    // Node count is easier to take from the rkyv cache than the raw JSON;
    // the caller passes the file-summary builder the meta, which already has
    // node_count. But to keep this function self-contained, count from
    // document for safety.
    let total_nodes = count_nodes_value(file_root.get("document"));

    json!({
        "counts": {
            "nodes": total_nodes,
            "pages": total_pages,
            "components": components.0,
            "component_sets": component_sets.0,
            "styles": styles.0,
            "variables": variable_count,
            "comments": comments_count,
        },
        "pages": pages,
        "components": components.1,
        "component_sets": component_sets.1,
        "styles": styles.1,
        "variable_collections": variable_collections,
        "recent_comments": recent_comments.unwrap_or(Value::Array(Vec::new())),
    })
}

fn count_nodes_value(node: Option<&Value>) -> usize {
    let Some(n) = node else { return 0 };
    let mut count = 1;
    if let Some(arr) = n.get("children").and_then(Value::as_array) {
        for c in arr {
            count += count_nodes_value(Some(c));
        }
    }
    count
}

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn merge_size_constraint(out: &mut Map<String, Value>, key: &str, v: &Value) {
    if v.is_null() {
        return;
    }
    let sc = out
        .entry("size_constraints".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(map) = sc.as_object_mut() {
        map.insert(key.into(), v.clone());
    }
}

/// Identity check for a 2x3 affine `relativeTransform` (Figma's shape).
/// Identity is `[[1,0,0],[0,1,0]]`. Anything else is non-identity.
fn is_identity_transform(t: &Value) -> bool {
    let rows = match t.as_array() {
        Some(a) if a.len() == 2 => a,
        _ => return false,
    };
    let expected: [[f64; 3]; 2] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    for (i, row) in rows.iter().enumerate() {
        let cols = match row.as_array() {
            Some(c) if c.len() == 3 => c,
            _ => return false,
        };
        for (j, v) in cols.iter().enumerate() {
            let f = v.as_f64().unwrap_or(f64::NAN);
            if (f - expected[i][j]).abs() > 1e-9 {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_paint_solid_includes_hex_and_collects_bound_variable() {
        let mut c = Collector::default();
        let paint = json!({
            "type": "SOLID",
            "color": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0},
            "boundVariables": {"color": {"type": "VARIABLE_ALIAS", "id": "VariableID:42:1"}},
        });
        let v = build_paint(&paint, &mut c, "1:2");
        assert_eq!(v["type"], "SOLID");
        assert_eq!(v["hex"], "#ff0000");
        assert_eq!(v["bound_variable"], "VariableID:42:1");
        assert!(c.variables.contains("VariableID:42:1"));
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn unknown_paint_type_surfaces_warning() {
        let mut c = Collector::default();
        let paint = json!({"type": "GLITCH"});
        let _ = build_paint(&paint, &mut c, "1:42");
        assert!(
            c.warnings
                .iter()
                .any(|w| w.location == "paint.type" && w.detail == "GLITCH"),
            "expected paint.type warning, got {:?}",
            c.warnings
        );
    }

    #[test]
    fn build_layout_emits_axes_and_padding() {
        let node = json!({
            "layoutMode": "VERTICAL",
            "primaryAxisSizingMode": "AUTO",
            "primaryAxisAlignItems": "MIN",
            "counterAxisSizingMode": "FIXED",
            "counterAxisAlignItems": "CENTER",
            "itemSpacing": 12,
            "paddingTop": 20,
            "paddingBottom": 20,
            "paddingLeft": 16,
            "paddingRight": 16,
        });
        let mut c = Collector::default();
        let v = build_layout(&node, &mut c, "1:2");
        assert_eq!(v["mode"], "VERTICAL");
        assert_eq!(v["primary_axis"]["sizing"], "AUTO");
        assert_eq!(v["primary_axis"]["align"], "MIN");
        assert_eq!(v["counter_axis"]["align"], "CENTER");
        assert_eq!(v["item_spacing"], 12);
        assert_eq!(v["padding"]["top"], 20);
        assert_eq!(v["padding"]["left"], 16);
    }

    #[test]
    fn flatten_bound_variables_handles_arrays_and_objects() {
        let bv = json!({
            "cornerRadius": {"type": "VARIABLE_ALIAS", "id": "v1"},
            "fills": [
                {"type": "VARIABLE_ALIAS", "id": "v2"},
                {"type": "VARIABLE_ALIAS", "id": "v3"}
            ]
        });
        let mut c = Collector::default();
        let v = flatten_bound_variables(&bv, &mut c, "1:2");
        let m = v.as_object().unwrap();
        assert_eq!(m["cornerRadius"], "v1");
        assert_eq!(m["fills[0]"], "v2");
        assert_eq!(m["fills[1]"], "v3");
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn flatten_bound_variables_warns_on_non_object_root() {
        let mut c = Collector::default();
        let bv = json!("oops");
        let _ = flatten_bound_variables(&bv, &mut c, "1:99");
        assert!(c
            .warnings
            .iter()
            .any(|w| w.location == "bound_variables.root"));
    }

    #[test]
    fn flatten_bound_variables_warns_on_missing_id_in_entry() {
        let mut c = Collector::default();
        let bv = json!({ "fills": [ {"type": "VARIABLE_ALIAS"} ] });
        let _ = flatten_bound_variables(&bv, &mut c, "1:99");
        assert!(c
            .warnings
            .iter()
            .any(|w| w.location == "bound_variables.entry"));
    }

    #[test]
    fn build_node_view_drops_visible_default_and_zero_rotation() {
        let mut c = Collector::default();
        let node = json!({
            "id": "1:2",
            "type": "FRAME",
            "name": "X",
            "visible": true,
            "rotation": 0.0,
            "absoluteBoundingBox": {"x": 0, "y": 0, "width": 100, "height": 50}
        });
        let v = build_node_view(&node, &ViewOptions::default(), &mut c);
        let m = v.as_object().unwrap();
        assert!(!m.contains_key("visible"));
        assert!(!m.contains_key("rotation"));
        assert!(m.contains_key("bounds"));
    }

    #[test]
    fn build_node_view_collects_style_refs() {
        let mut c = Collector::default();
        let node = json!({
            "id": "1:2",
            "type": "FRAME",
            "name": "X",
            "styles": { "fill": "S:abc", "text": "S:xyz" },
        });
        let _ = build_node_view(&node, &ViewOptions::default(), &mut c);
        assert!(c.styles.contains("S:abc"));
        assert!(c.styles.contains("S:xyz"));
    }

    #[test]
    fn build_node_view_truncates_at_max_nodes() {
        let mut c = Collector::default();
        // 6 children, max_nodes = 3
        let node = json!({
            "id": "0:0",
            "type": "FRAME",
            "name": "P",
            "children": [
                {"id": "c1", "type": "FRAME", "name": "1"},
                {"id": "c2", "type": "FRAME", "name": "2"},
                {"id": "c3", "type": "FRAME", "name": "3"},
                {"id": "c4", "type": "FRAME", "name": "4"},
                {"id": "c5", "type": "FRAME", "name": "5"},
                {"id": "c6", "type": "FRAME", "name": "6"},
            ]
        });
        let opts = ViewOptions {
            max_nodes: 3,
            ..ViewOptions::default()
        };
        let v = build_node_view(&node, &opts, &mut c);
        let kids = v["children"].as_array().unwrap();
        assert_eq!(kids.len(), 3);
        assert_eq!(c.emitted_descendants, 3);
        assert_eq!(c.omitted_ids, vec!["c4", "c5", "c6"]);
        assert!(c.truncated());
    }

    #[test]
    fn variables_block_renders_color_as_hex() {
        let mut refs = BTreeSet::new();
        refs.insert("VariableID:42:1".to_owned());
        let vars_root = json!({
            "meta": {
                "variables": {
                    "VariableID:42:1": {
                        "name": "color/brand/primary",
                        "variableCollectionId": "VariableCollectionId:42:0",
                        "resolvedType": "COLOR",
                        "valuesByMode": {
                            "42:0": {"r": 0.039, "g": 0.522, "b": 1.0, "a": 1.0},
                        },
                        "scopes": ["FILL"]
                    }
                },
                "variableCollections": {
                    "VariableCollectionId:42:0": {
                        "name": "Semantic",
                        "defaultModeId": "42:0",
                        "modes": [{"modeId": "42:0", "name": "Light"}]
                    }
                }
            }
        });
        let mut c = Collector::default();
        let block = build_variables_block(Some(&vars_root), &refs, &mut c);
        let entry = &block["VariableID:42:1"];
        assert_eq!(entry["name"], "color/brand/primary");
        assert_eq!(entry["resolved_type"], "COLOR");
        assert_eq!(entry["values_by_mode"]["Light"], "#0a85ff");
        assert_eq!(entry["collection"]["default_mode"], "Light");
    }

    #[test]
    fn variables_block_pulls_in_aliased_variable() {
        // A semantic COLOR variable whose value aliases a primitive variable.
        // `refs` references only the semantic id; the block must still emit the
        // aliased primitive so the `{alias: id}` value resolves to a definition.
        let mut refs = BTreeSet::new();
        refs.insert("VariableID:sem:1".to_owned());
        let vars_root = json!({
            "meta": {
                "variables": {
                    "VariableID:sem:1": {
                        "name": "color/semantic/accent",
                        "variableCollectionId": "VariableCollectionId:1:0",
                        "resolvedType": "COLOR",
                        "valuesByMode": {
                            "1:0": {"type": "VARIABLE_ALIAS", "id": "VariableID:prim:1"},
                        },
                    },
                    "VariableID:prim:1": {
                        "name": "color/primitive/blue-500",
                        "variableCollectionId": "VariableCollectionId:1:0",
                        "resolvedType": "COLOR",
                        "valuesByMode": {
                            "1:0": {"r": 0.039, "g": 0.522, "b": 1.0, "a": 1.0},
                        },
                    }
                },
                "variableCollections": {
                    "VariableCollectionId:1:0": {
                        "name": "Tokens",
                        "defaultModeId": "1:0",
                        "modes": [{"modeId": "1:0", "name": "Light"}]
                    }
                }
            }
        });
        let mut c = Collector::default();
        let block = build_variables_block(Some(&vars_root), &refs, &mut c);
        // The alias value surfaces the target id…
        assert_eq!(
            block["VariableID:sem:1"]["values_by_mode"]["Light"]["alias"],
            "VariableID:prim:1"
        );
        // …and the aliased primitive is pulled into the block with its own def.
        assert_eq!(
            block["VariableID:prim:1"]["values_by_mode"]["Light"], "#0a85ff",
            "aliased primitive must be pulled into the block"
        );
    }

    #[test]
    fn styles_index_block_only_includes_referenced_entries() {
        let mut refs = BTreeSet::new();
        refs.insert("S:abc".to_owned());
        let file_root = json!({
            "styles": {
                "S:abc": {
                    "key": "abc-key",
                    "name": "color/brand",
                    "description": "Brand primary",
                    "styleType": "FILL"
                },
                "S:zzz": {
                    "key": "zzz-key",
                    "name": "unused",
                    "styleType": "FILL"
                }
            }
        });
        let block = build_styles_index_block(&file_root, &refs);
        let map = block.as_object().unwrap();
        assert!(map.contains_key("S:abc"));
        assert!(!map.contains_key("S:zzz"));
        assert_eq!(map["S:abc"]["type"], "FILL");
    }
}
