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
//! - Empty arrays and default values are omitted (e.g. `fills: []` doesn't
//!   appear, nor does `constraints: {LEFT, TOP}` — Figma's default).
//! - Hidden children (`visible: false`) are pruned by default and listed on
//!   the parent as `hidden_children: [{id, name, type}]`; a hidden *target* is
//!   still rendered, and so is a hidden layer inside a component definition
//!   whose visibility is driven by a BOOLEAN property (the property wiring
//!   and the layer's styling are what an implementer needs).
//!   `ViewOptions::include_hidden` restores every subtree.
//! - `bounds` is absolute on the target and parent-relative on descendants;
//!   flow children of an auto-layout parent carry only `width`/`height`
//!   because their position is derived by the layout. A CANVAS has no box,
//!   so a page's children are relative to the canvas origin — which is
//!   exactly Figma's absolute coordinates.
//! - Bound variables are emitted as short handles (`v1`, `v2`, …) assigned in
//!   encounter order; the handle → id/name/value lookup is hoisted to the
//!   top-level `variables` block, named styles to `styles_index`, so token
//!   data isn't duplicated per node.
//! - Colors are emitted as `hex` only (`#rrggbb` / `#rrggbbaa`); the float
//!   channels are available via `node-info --raw`.
//! - Children at depth ≥ 1 drop `effects`, `prototype`, `export_settings`,
//!   `dev_status`, `annotations` to keep subtree output compact; geometry
//!   leaves (VECTOR & co.) also drop the layout block that cannot apply to
//!   them: `constraints` when they are flow children of an auto-layout
//!   parent, `layout_child` when the parent has no auto-layout.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::node::rgba_to_hex;

/// One selectable output section for `node-info --only`. The section → code
/// mapping (not 1:1 with the `add_*` helpers):
///
/// - `Geometry` → bounds, size, relative_transform, constraints, size_constraints
/// - `Corner` → corner + clips_content
/// - `Fills` / `Strokes` / `Effects` → the three paint branches (`Strokes`
///   includes the companion `stroke` weight/align block)
/// - `Layout` → layout, layout_child, layout_grids
/// - `Text` / `Component` / `Prototype` / `Meta` → those blocks (`component`
///   includes property_refs)
/// - `Styles` → the per-node `styles` map AND the top-level `styles_index`
/// - `Variables` → per-node `bound_variables`/`explicit_variable_modes` AND
///   the top-level `variables` block (variables referenced by kept paint
///   sections are still hoisted regardless — see `Collector`)
/// - `Comments` → the anchored-comments block (`node-info`-level)
/// - `Pages` → the page list; file targets only (rejected on node targets)
///
/// Identity (id/type/name + modifier flags) is always emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Section {
    Fills,
    Strokes,
    Effects,
    Geometry,
    Corner,
    Layout,
    Text,
    Component,
    Prototype,
    Meta,
    Styles,
    Variables,
    Comments,
    Pages,
}

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
    /// `--only` section selection. `None` = every section (default). The
    /// filter applies per node at every depth; identity and the `children`
    /// recursion are unaffected.
    pub only: Option<BTreeSet<Section>>,
    /// Render `visible: false` children instead of pruning them into the
    /// parent's `hidden_children` list. Off by default — hidden subtrees are
    /// component-slot alternates that don't render, and in a real screen
    /// they were 65% of the output.
    pub include_hidden: bool,
}

impl ViewOptions {
    /// Is `s` selected? Everything is selected when no `--only` was given.
    pub fn wants(&self, s: Section) -> bool {
        self.only.as_ref().is_none_or(|set| set.contains(&s))
    }
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            depth: None,
            max_nodes: 500,
            prototype: false,
            meta: false,
            rich_text: false,
            only: None,
            include_hidden: false,
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
    /// `VariableID:...` strings encountered via `boundVariables` (on the
    /// node itself, on paints, on effects, etc.) in encounter order; index
    /// `i` is handle `v{i+1}`. Nodes reference variables by handle (see
    /// [`Collector::var_ref`]) and the top-level `variables` block maps
    /// handle → id (+ resolved data).
    pub var_handles: Vec<String>,
    /// Reverse index of `var_handles` so interning stays O(1).
    var_index: HashMap<String, usize>,
    /// Ids of every node the view rendered (target + descendants). Lets the
    /// caller tell whether something that points at a node — an anchored
    /// comment, say — points at a node the reader can actually see.
    pub emitted_ids: BTreeSet<String>,
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

    /// Intern a `VariableID:...` string and return its short handle (`v1`,
    /// `v2`, … in first-seen order). Every place the view emits a variable
    /// reference goes through here so the same id always renders as the
    /// same handle, and the `variables` block can list each id once.
    pub fn var_ref(&mut self, id: &str) -> String {
        let idx = match self.var_index.get(id) {
            Some(&i) => i,
            None => {
                self.var_handles.push(id.to_owned());
                let i = self.var_handles.len() - 1;
                self.var_index.insert(id.to_owned(), i);
                i
            }
        };
        var_handle(idx)
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

/// Node types whose subtree is pure geometry (icon path internals). At
/// depth ≥ 1 these drop `constraints` and `layout_child`: they describe how
/// a path scales inside its icon frame, which no implementation reproduces —
/// the icon ships as an SVG. Fills/strokes stay (they carry the color token).
const GEOMETRY_LEAF_TYPES: &[&str] = &[
    "VECTOR",
    "BOOLEAN_OPERATION",
    "STAR",
    "LINE",
    "REGULAR_POLYGON",
];

/// Handle string for the `i`-th interned variable (0-based).
fn var_handle(i: usize) -> String {
    format!("v{}", i + 1)
}

/// Is this node a geometry leaf below the target (see `GEOMETRY_LEAF_TYPES`)?
fn is_geometry_leaf(node: &Value, depth: usize) -> bool {
    depth > 0
        && node
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| GEOMETRY_LEAF_TYPES.contains(&t))
}

/// Does `parent` lay its children out (any `layoutMode` but NONE)?
fn parent_has_auto_layout(parent: Option<&Value>) -> bool {
    parent
        .and_then(|p| p.get("layoutMode"))
        .and_then(Value::as_str)
        .is_some_and(|m| m != "NONE")
}

/// Is `node` positioned by its parent's auto-layout (as opposed to being
/// absolutely positioned inside it, or living in a plain frame)?
fn is_flow_child(node: &Value, parent: Option<&Value>) -> bool {
    parent_has_auto_layout(parent)
        && node.get("layoutPositioning").and_then(Value::as_str) != Some("ABSOLUTE")
}

/// Is the child's `visible` flag wired to a component property? Such a
/// layer is hidden in the definition only until the property is set, so it
/// is rendered even though `visible: false` (inside a component definition).
fn has_visible_property_ref(node: &Value) -> bool {
    node.get("componentPropertyReferences")
        .and_then(|r| r.get("visible"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
}

// ───────────────────────────────────────────────────────────────────────────
// Top-level entry point
// ───────────────────────────────────────────────────────────────────────────

/// Build the curated view for `node`. Recurses into children up to
/// `opts.depth` and `opts.max_nodes`. Mutates `collector` to record which
/// variable / style ids were referenced and which children (if any) were
/// dropped by the node cap.
pub fn build_node_view(node: &Value, opts: &ViewOptions, collector: &mut Collector) -> Value {
    // `componentPropertyReferences` (which prop drives which node) is only
    // meaningful when the target itself is a component definition — inside
    // an instance it's noise on every descendant.
    let root_is_component = node
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| matches!(t, "COMPONENT" | "COMPONENT_SET"));
    let ctx = WalkCtx { root_is_component };
    let mut view = build_view_recursive(
        node, None, opts, collector, /* depth_from_target */ 0, &ctx,
    );
    round_floats(&mut view);
    view
}

/// Figma stores geometry as f32 and the REST API prints it as f64, so a
/// 14.4-wide icon arrives as `14.40007495880127`. Three decimals keep
/// sub-pixel precision (and every colour channel exact) while dropping the
/// noise. Applied once over the finished view so every section gets it.
pub fn round_floats(v: &mut Value) {
    match v {
        Value::Number(n) => {
            if n.is_f64() {
                if let Some(f) = n.as_f64() {
                    let r = (f * 1000.0).round() / 1000.0;
                    if r != f {
                        if let Some(num) = serde_json::Number::from_f64(r) {
                            *v = Value::Number(num);
                        }
                    }
                }
            }
        }
        Value::Object(m) => m.values_mut().for_each(round_floats),
        Value::Array(a) => a.iter_mut().for_each(round_floats),
        _ => {}
    }
}

/// Facts about the target that every level of the walk needs.
struct WalkCtx {
    root_is_component: bool,
}

fn build_view_recursive(
    node: &Value,
    parent: Option<&Value>,
    opts: &ViewOptions,
    collector: &mut Collector,
    depth: usize,
    ctx: &WalkCtx,
) -> Value {
    let mut out = Map::new();
    // Attribution for any unknown-shape warnings raised below. Empty when
    // the node lacks an `id` (rare; defensive against bad input).
    let node_id = node.get("id").and_then(Value::as_str).unwrap_or("");
    if !node_id.is_empty() {
        collector.emitted_ids.insert(node_id.to_owned());
    }

    // Each `add_*` helper appends its section to `out` in source order; the
    // order of these calls IS the output key order (serde_json Map is
    // insertion-ordered) — stable regardless of the `--only` selection.
    // `depth` is threaded where the `depth == 0` gates live (effects, layout
    // grids). Identity stays unconditional: a fill is useless without
    // knowing which node it belongs to.
    add_identity_and_modifiers(&mut out, node);
    if opts.wants(Section::Geometry) {
        add_geometry(&mut out, node, parent, depth);
    }
    if opts.wants(Section::Corner) {
        add_corner(&mut out, node);
    }
    add_paint_layers(&mut out, node, opts, collector, node_id, depth);
    if opts.wants(Section::Layout) {
        add_layout(&mut out, node, parent, collector, node_id, depth);
    }
    if opts.wants(Section::Text) {
        add_text(&mut out, node, opts, collector);
    }
    if opts.wants(Section::Component) {
        add_component(&mut out, node, collector, node_id, depth, ctx);
    }
    if opts.wants(Section::Prototype) {
        add_prototype(&mut out, node, opts);
    }
    if opts.wants(Section::Meta) {
        add_meta(&mut out, node, opts);
    }
    add_styles_and_variables(&mut out, node, opts, collector, node_id);

    // ── Children ───────────────────────────────────────────────────────────
    // Stays inline: it recurses into build_view_recursive and its collector
    // bookkeeping (emitted_descendants increment, omitted_ids push, the cap
    // check) is order-critical against the recursion.
    let allow_deeper = opts.depth.is_none_or(|d| depth < d);
    if allow_deeper {
        if let Some(arr) = node.get("children").and_then(Value::as_array) {
            let mut out_children: Vec<Value> = Vec::new();
            let mut hidden: Vec<Value> = Vec::new();
            for child in arr {
                // Hidden subtrees are pruned before the node cap so they
                // neither consume the budget nor masquerade as visible. The
                // exception is a property-driven layer in a component
                // definition (`visible` bound to a BOOLEAN prop): it renders
                // whenever the prop is on, so its wiring and styling stay.
                let property_driven = ctx.root_is_component && has_visible_property_ref(child);
                if !opts.include_hidden && !crate::node::is_visible(child) && !property_driven {
                    hidden.push(json!({
                        "id": child.get("id").cloned().unwrap_or(Value::Null),
                        "name": child.get("name").cloned().unwrap_or(Value::Null),
                        "type": child.get("type").cloned().unwrap_or(Value::Null),
                    }));
                    continue;
                }
                if collector.emitted_descendants >= opts.max_nodes {
                    if let Some(id) = child.get("id").and_then(Value::as_str) {
                        collector.omitted_ids.push(id.to_owned());
                    }
                    continue;
                }
                collector.emitted_descendants += 1;
                let v = build_view_recursive(child, Some(node), opts, collector, depth + 1, ctx);
                out_children.push(v);
            }
            if !out_children.is_empty() {
                out.insert("children".into(), Value::Array(out_children));
            }
            if !hidden.is_empty() {
                out.insert("hidden_children".into(), Value::Array(hidden));
            }
        }
    }

    Value::Object(out)
}

/// Identity (id/type/name) plus the common modifier flags that are emitted
/// only when they differ from their defaults.
fn add_identity_and_modifiers(out: &mut Map<String, Value>, node: &Value) {
    if let Some(v) = node.get("id") {
        out.insert("id".into(), v.clone());
    }
    if let Some(v) = node.get("type") {
        out.insert("type".into(), v.clone());
    }
    if let Some(v) = node.get("name") {
        out.insert("name".into(), v.clone());
    }
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
}

/// Bounds, size, non-identity transform, constraints, and responsive size
/// constraints.
///
/// `bounds` is absolute (canvas coordinates) on the target and
/// parent-relative on descendants. A flow child of an auto-layout parent
/// gets only `width`/`height`: its x/y is an output of the layout, not an
/// input to the implementation.
fn add_geometry(out: &mut Map<String, Value>, node: &Value, parent: Option<&Value>, depth: usize) {
    let abs = node.get("absoluteBoundingBox");
    if let Some(b) = abs {
        let bounds = match parent {
            Some(p) if depth > 0 => relative_bounds(b, node, p),
            _ => b.clone(),
        };
        out.insert("bounds".into(), bounds);
    }
    if let Some(s) = node.get("size") {
        // `size` is the untransformed size; it only adds information when it
        // differs from the bounding box (rotation / skew).
        let same = abs.is_some_and(|b| {
            approx_eq(b.get("width"), s.get("x")) && approx_eq(b.get("height"), s.get("y"))
        });
        if !same {
            out.insert("size".into(), s.clone());
        }
    }
    if let Some(t) = node.get("relativeTransform") {
        // Only emit when non-identity (any non-zero off-diagonal or non-1/0).
        if !is_identity_transform(t) {
            out.insert("relative_transform".into(), t.clone());
        }
    }
    if let Some(c) = node.get("constraints") {
        // A geometry leaf laid out by its parent's auto-layout never uses
        // its constraints; anywhere else they decide how it resizes.
        let inapplicable = is_geometry_leaf(node, depth) && is_flow_child(node, parent);
        if !is_default_constraints(c) && !inapplicable {
            out.insert("constraints".into(), c.clone());
        }
    }
    // Size constraints (used in responsive auto-layout).
    if let Some(min_w) = node.get("minWidth") {
        merge_size_constraint(out, "min_width", min_w);
    }
    if let Some(max_w) = node.get("maxWidth") {
        merge_size_constraint(out, "max_width", max_w);
    }
    if let Some(min_h) = node.get("minHeight") {
        merge_size_constraint(out, "min_height", min_h);
    }
    if let Some(max_h) = node.get("maxHeight") {
        merge_size_constraint(out, "max_height", max_h);
    }
}

/// Corner radii (uniform or per-corner) and the clip flag.
fn add_corner(out: &mut Map<String, Value>, node: &Value) {
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
}

/// Fills, strokes (+ companion stroke block), and effects. Effects are noise
/// for non-target depths, so they're emitted only at `depth == 0`.
fn add_paint_layers(
    out: &mut Map<String, Value>,
    node: &Value,
    opts: &ViewOptions,
    collector: &mut Collector,
    node_id: &str,
    depth: usize,
) {
    // Fills / strokes / effects gate individually — `--only strokes` must
    // not walk fills (the Collector would otherwise hoist fill variables the
    // narrowed output never shows).
    if opts.wants(Section::Fills) {
        if let Some(arr) = node.get("fills").and_then(Value::as_array) {
            let paints = build_paints(arr, collector, node_id);
            if !paints.is_empty() {
                out.insert("fills".into(), Value::Array(paints));
            }
        }
    }
    if opts.wants(Section::Strokes) {
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
    }
    if depth == 0 && opts.wants(Section::Effects) {
        if let Some(arr) = node.get("effects").and_then(Value::as_array) {
            let effects = build_effects(arr, collector, node_id);
            if !effects.is_empty() {
                out.insert("effects".into(), Value::Array(effects));
            }
        }
    }
}

/// Auto-layout container settings, this node's own layout-child settings, and
/// layout grids (grids only at `depth == 0`).
fn add_layout(
    out: &mut Map<String, Value>,
    node: &Value,
    parent: Option<&Value>,
    collector: &mut Collector,
    node_id: &str,
    depth: usize,
) {
    if let Some(mode) = node.get("layoutMode").and_then(Value::as_str) {
        if mode != "NONE" {
            out.insert("layout".into(), build_layout(node, collector, node_id));
        }
    }
    // A geometry leaf's layout-child settings only mean something when a
    // parent auto-layout reads them (a FILL-width divider line, say).
    let inapplicable = is_geometry_leaf(node, depth) && !parent_has_auto_layout(parent);
    if !inapplicable {
        let layout_child = build_layout_child(node);
        if !layout_child.is_empty() {
            out.insert("layout_child".into(), Value::Object(layout_child));
        }
    }
    if let Some(grids) = node.get("layoutGrids").and_then(Value::as_array) {
        if !grids.is_empty() && depth == 0 {
            out.insert("layout_grids".into(), Value::Array(grids.clone()));
        }
    }
}

/// TEXT node content block.
fn add_text(
    out: &mut Map<String, Value>,
    node: &Value,
    opts: &ViewOptions,
    collector: &mut Collector,
) {
    if node.get("type").and_then(Value::as_str) == Some("TEXT") {
        if let Some(text) = build_text(node, opts, collector) {
            out.insert("text".into(), text);
        }
    }
}

/// Component / instance metadata and component-property references.
fn add_component(
    out: &mut Map<String, Value>,
    node: &Value,
    collector: &mut Collector,
    node_id: &str,
    depth: usize,
    ctx: &WalkCtx,
) {
    let component = build_component(node, collector, node_id);
    if !component.is_empty() {
        out.insert("component".into(), Value::Object(component));
    }
    // Property references matter on the target itself and anywhere inside a
    // component definition; inside an instance subtree they're wiring noise.
    if depth == 0 || ctx.root_is_component {
        if let Some(refs) = node.get("componentPropertyReferences") {
            if !refs.is_null() && refs.as_object().is_some_and(|m| !m.is_empty()) {
                out.insert("property_refs".into(), refs.clone());
            }
        }
    }
}

/// Prototype interactions — opt-in, but always emitted when this node starts a
/// flow.
fn add_prototype(out: &mut Map<String, Value>, node: &Value, opts: &ViewOptions) {
    if opts.prototype || node.get("prototypeStartNodeID").is_some() {
        if let Some(proto) = build_prototype(node, opts.prototype) {
            if !proto.as_object().is_none_or(Map::is_empty) {
                out.insert("prototype".into(), proto);
            }
        }
    }
}

/// Dev status, annotations, and export settings — opt-in (`--meta`).
fn add_meta(out: &mut Map<String, Value>, node: &Value, opts: &ViewOptions) {
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
}

/// Named-style references and bound/explicit variable modes. Mutates the
/// collector (style ids, variable ids) as a side channel so the top-level
/// `styles_index` / `variables` blocks can resolve them.
fn add_styles_and_variables(
    out: &mut Map<String, Value>,
    node: &Value,
    opts: &ViewOptions,
    collector: &mut Collector,
    node_id: &str,
) {
    // The two halves gate independently: `Styles` feeds the top-level
    // `styles_index` via the collector, `Variables` the `bound_variables`
    // block (and thereby the hoisted `variables` block).
    if opts.wants(Section::Styles) {
        if let Some(styles_map) = node.get("styles").and_then(Value::as_object) {
            // Pass the styles map through as-is (small) and collect ids so
            // the top-level `styles_index` block resolves them.
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
    }
    if opts.wants(Section::Variables) {
        if let Some(bv) = node.get("boundVariables") {
            let mut flat = flatten_bound_variables(bv, collector, node_id);
            drop_paint_bindings_already_on_paints(&mut flat, out);
            if flat.as_object().is_some_and(|m| !m.is_empty()) {
                out.insert("bound_variables".into(), flat);
            }
        }
        if let Some(emodes) = node.get("explicitVariableModes") {
            if !emodes.is_null() && emodes.as_object().is_some_and(|m| !m.is_empty()) {
                out.insert("explicit_variable_modes".into(), emodes.clone());
            }
        }
    }
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
                out.insert("hex".into(), json!(rgba_to_hex(c)));
            }
            // Inline `boundVariables.color` if present.
            if let Some(bv) = paint.get("boundVariables").and_then(Value::as_object) {
                if let Some(color_alias) = bv
                    .get("color")
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                {
                    out.insert(
                        "bound_variable".into(),
                        json!(collector.var_ref(color_alias)),
                    );
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
        out.insert("hex".into(), json!(rgba_to_hex(c)));
    }
    if let Some(bv) = stop.get("boundVariables").and_then(Value::as_object) {
        if let Some(alias) = bv
            .get("color")
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
        {
            out.insert("bound_variable".into(), json!(collector.var_ref(alias)));
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
                bound.insert(k.clone(), json!(collector.var_ref(id)));
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

    // Axis blocks elide Figma's defaults: `sizing: AUTO` (hug) and
    // `align: MIN` (start).
    let mut primary = Map::new();
    if let Some(v) = node.get("primaryAxisSizingMode").filter(|v| *v != "AUTO") {
        primary.insert("sizing".into(), v.clone());
    }
    if let Some(v) = node.get("primaryAxisAlignItems").filter(|v| *v != "MIN") {
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
    if let Some(v) = node.get("counterAxisSizingMode").filter(|v| *v != "AUTO") {
        counter.insert("sizing".into(), v.clone());
    }
    if let Some(v) = node.get("counterAxisAlignItems").filter(|v| *v != "MIN") {
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

    if let Some(w) = node.get("layoutWrap").filter(|w| *w != "NO_WRAP") {
        out.insert("wrap".into(), w.clone());
    }
    if let Some(s) = node.get("itemSpacing") {
        if s.as_f64().is_none_or(|f| f != 0.0) {
            out.insert("item_spacing".into(), s.clone());
        }
    }

    // Padding: one number when uniform, else `[top, right, bottom, left]`
    // (CSS order). Absent sides are 0. All-zero padding is omitted.
    let sides: Vec<f64> = ["paddingTop", "paddingRight", "paddingBottom", "paddingLeft"]
        .iter()
        .map(|k| node.get(k).and_then(Value::as_f64).unwrap_or(0.0))
        .collect();
    if sides.iter().any(|v| *v != 0.0) {
        if sides.iter().all(|v| (*v - sides[0]).abs() < 1e-9) {
            out.insert("padding".into(), json!(sides[0]));
        } else {
            out.insert("padding".into(), json!(sides));
        }
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
    // `sizing: "H/V"` — horizontal then vertical, each FIXED | HUG | FILL.
    let sh = node.get("layoutSizingHorizontal").and_then(Value::as_str);
    let sv = node.get("layoutSizingVertical").and_then(Value::as_str);
    if sh.is_some() || sv.is_some() {
        out.insert(
            "sizing".into(),
            json!(format!("{}/{}", sh.unwrap_or("-"), sv.unwrap_or("-"))),
        );
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
    // Dropped on purpose: `fontPostScriptName` (family + style say the same),
    // `lineHeightPercent` / `lineHeightPercentFontSize` (restate
    // `lineHeightPx` against the font size). `lineHeightUnit` is kept only
    // when it isn't PIXELS, alignment only when it isn't the LEFT/TOP default.
    for (key, out_key) in [
        ("fontFamily", "font_family"),
        ("fontStyle", "font_style"),
        ("italic", "italic"),
        ("fontWeight", "font_weight"),
        ("fontSize", "font_size"),
        ("textCase", "text_case"),
        ("textAlignHorizontal", "text_align_horizontal"),
        ("textAlignVertical", "text_align_vertical"),
        ("letterSpacing", "letter_spacing"),
        ("lineHeightPx", "line_height_px"),
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
            let is_default = match key {
                "textAlignHorizontal" => v == "LEFT",
                "textAlignVertical" => v == "TOP",
                "lineHeightUnit" => v == "PIXELS",
                _ => false,
            };
            if !v.is_null() && !is_default {
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
        if flat.as_object().is_some_and(|m| !m.is_empty()) {
            out.insert("bound_variables".into(), flat);
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

fn build_component(node: &Value, collector: &mut Collector, node_id: &str) -> Map<String, Value> {
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
    // Variant assignments come as `variantProperties` (older shape) and/or
    // `componentProperties` entries with `type: VARIANT`; both land in
    // `variants`. Everything else (BOOLEAN / TEXT / INSTANCE_SWAP) lands in
    // `properties` as plain scalars — the `{type, value}` wrapper and the
    // `#n:m` disambiguation suffix Figma appends to names are dropped. An
    // INSTANCE_SWAP keeps its `preferredValues` (the allowed swap targets)
    // as `preferred`: nothing else in the view says what the slot accepts.
    let mut variants = Map::new();
    let mut properties = Map::new();
    if let Some(vp) = node.get("variantProperties").and_then(Value::as_object) {
        for (k, v) in vp {
            variants.insert(k.clone(), v.clone());
        }
    }
    if let Some(cp) = node.get("componentProperties").and_then(Value::as_object) {
        let names = display_property_names(cp);
        for (raw_name, prop) in cp {
            let name = names
                .get(raw_name)
                .cloned()
                .unwrap_or_else(|| raw_name.clone());
            let ptype = prop.get("type").and_then(Value::as_str).unwrap_or("");
            let value = prop.get("value").cloned().unwrap_or(Value::Null);
            let bound = prop
                .get("boundVariables")
                .filter(|bv| bv.as_object().is_some_and(|m| !m.is_empty()))
                .map(|bv| flatten_bound_variables(bv, collector, node_id));
            let mut rendered = match ptype {
                "VARIANT" | "BOOLEAN" | "TEXT" => value,
                "INSTANCE_SWAP" => {
                    let mut m = Map::new();
                    m.insert("instance".into(), value);
                    if let Some(pv) = prop.get("preferredValues").and_then(Value::as_array) {
                        if !pv.is_empty() {
                            m.insert("preferred".into(), Value::Array(pv.clone()));
                        }
                    }
                    Value::Object(m)
                }
                other => {
                    collector.record_unknown("component_property.type", node_id, other);
                    json!({ "type": other, "value": value })
                }
            };
            if let Some(bv) = bound {
                let mut wrapped = match rendered {
                    Value::Object(m) => m,
                    scalar => {
                        let mut m = Map::new();
                        m.insert("value".into(), scalar);
                        m
                    }
                };
                wrapped.insert("bound_variables".into(), bv);
                rendered = Value::Object(wrapped);
            }
            if ptype == "VARIANT" {
                variants.insert(name, rendered);
            } else {
                properties.insert(name, rendered);
            }
        }
    }
    if !variants.is_empty() {
        out.insert("variants".into(), Value::Object(variants));
    }
    if !properties.is_empty() {
        out.insert("properties".into(), Value::Object(properties));
    }
    if let Some(pd) = node.get("componentPropertyDefinitions") {
        if !pd.is_null() {
            out.insert("property_definitions".into(), pd.clone());
        }
    }
    out
}

/// Map each raw component-property name to its display name: the `#n:m`
/// suffix is stripped unless two properties would collide, in which case
/// both keep their raw names.
fn display_property_names(props: &Map<String, Value>) -> BTreeMap<String, String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let stripped: Vec<(String, String)> = props
        .keys()
        .map(|k| (k.clone(), strip_property_suffix(k).to_owned()))
        .collect();
    for (_, s) in &stripped {
        *counts.entry(s.clone()).or_default() += 1;
    }
    stripped
        .into_iter()
        .map(|(raw, s)| {
            let display = if counts[&s] > 1 { raw.clone() } else { s };
            (raw, display)
        })
        .collect()
}

/// `Close#1610:0` → `Close`. Leaves names without a `#<digits>:<digits>`
/// tail untouched.
fn strip_property_suffix(name: &str) -> &str {
    let Some(hash) = name.rfind('#') else {
        return name;
    };
    let tail = &name[hash + 1..];
    let is_ref = tail.split_once(':').is_some_and(|(a, b)| {
        !a.is_empty() && !b.is_empty() && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())
    });
    if is_ref && hash > 0 {
        &name[..hash]
    } else {
        name
    }
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

/// Figma reports a colour binding twice: on the paint itself
/// (`fills[i].boundVariables.color`) and in the node's `boundVariables` as
/// `fills[i]`. The view already prints the paint's copy as
/// `fills[i].bound_variable`, so the node-level `fills[i]`/`strokes[i]` entry
/// is removed when the emitted paint at that index carries the same handle.
/// Any entry that isn't mirrored (paint list not emitted under `--only`,
/// gradient paints, a differing handle) is kept.
fn drop_paint_bindings_already_on_paints(flat: &mut Value, out: &Map<String, Value>) {
    let Some(map) = flat.as_object_mut() else {
        return;
    };
    map.retain(|key, handle| {
        let Some((list, idx)) = parse_paint_key(key) else {
            return true;
        };
        let on_paint = out
            .get(list)
            .and_then(Value::as_array)
            .and_then(|paints| paints.get(idx))
            .and_then(|p| p.get("bound_variable"));
        on_paint != Some(handle)
    });
}

/// `fills[3]` → `("fills", 3)`; anything else → `None`.
fn parse_paint_key(key: &str) -> Option<(&str, usize)> {
    let (list, rest) = key.split_once('[')?;
    if !matches!(list, "fills" | "strokes") {
        return None;
    }
    rest.strip_suffix(']')?.parse().ok().map(|i| (list, i))
}

/// Flatten Figma's `boundVariables` shape into a simple
/// `{ property_path: handle }` map, interning each id via
/// [`Collector::var_ref`]. Handles three layouts:
/// - `{ cornerRadius: { id, type: VARIABLE_ALIAS } }` → `cornerRadius -> v1`
/// - `{ fills: [{ id, type }, { id, type }] }` → `fills[0] -> v1`, `fills[1] -> v2`
/// - `{ characters: { id } }` → string-property bindings on TEXT nodes.
fn flatten_bound_variables(bv: &Value, collector: &mut Collector, subject: &str) -> Value {
    let mut out = Map::new();
    let Some(obj) = bv.as_object() else {
        collector.record_unknown(
            "bound_variables.root",
            subject,
            format!("expected object, got {}", value_kind(bv)),
        );
        return Value::Object(out);
    };
    flatten_bound_variables_into(obj, "", &mut out, collector, subject);
    Value::Object(out)
}

/// Recursive worker for [`flatten_bound_variables`]. `prefix` is the dotted
/// property path so far; nested maps such as per-corner radii
/// (`rectangleCornerRadii.RECTANGLE_TOP_LEFT_CORNER_RADIUS`) or `size.x`
/// flatten to dotted keys instead of being dropped with a warning.
fn flatten_bound_variables_into(
    obj: &Map<String, Value>,
    prefix: &str,
    out: &mut Map<String, Value>,
    collector: &mut Collector,
    subject: &str,
) {
    for (key, value) in obj {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if let Some(arr) = value.as_array() {
            for (i, entry) in arr.iter().enumerate() {
                if let Some(id) = entry.get("id").and_then(Value::as_str) {
                    out.insert(format!("{path}[{i}]"), json!(collector.var_ref(id)));
                } else {
                    collector.record_unknown(
                        "bound_variables.entry",
                        subject,
                        format!("{path}[{i}] has no .id"),
                    );
                }
            }
        } else if let Some(id) = value.get("id").and_then(Value::as_str) {
            out.insert(path, json!(collector.var_ref(id)));
        } else if let Some(nested) = value
            .as_object()
            .filter(|m| m.values().any(|v| v.is_object() || v.is_array()))
        {
            if key == "rectangleCornerRadii" {
                flatten_corner_radii(nested, &path, out, collector, subject);
            } else {
                flatten_bound_variables_into(nested, &path, out, collector, subject);
            }
        } else {
            collector.record_unknown(
                "bound_variables.entry",
                subject,
                format!("{path} has no .id and is not an array"),
            );
        }
    }
}

/// Per-corner radius bindings arrive as
/// `{ RECTANGLE_TOP_LEFT_CORNER_RADIUS: {id}, … }`. When all four corners
/// bind the same variable — the common case — emit one `path -> handle`;
/// otherwise `path.top_left` etc. Unknown corner keys fall back to the
/// generic recursion so nothing is dropped.
fn flatten_corner_radii(
    nested: &Map<String, Value>,
    path: &str,
    out: &mut Map<String, Value>,
    collector: &mut Collector,
    subject: &str,
) {
    const CORNERS: [(&str, &str); 4] = [
        ("RECTANGLE_TOP_LEFT_CORNER_RADIUS", "top_left"),
        ("RECTANGLE_TOP_RIGHT_CORNER_RADIUS", "top_right"),
        ("RECTANGLE_BOTTOM_RIGHT_CORNER_RADIUS", "bottom_right"),
        ("RECTANGLE_BOTTOM_LEFT_CORNER_RADIUS", "bottom_left"),
    ];
    let all_known = nested
        .keys()
        .all(|k| CORNERS.iter().any(|(raw, _)| raw == k));
    let ids: Vec<Option<&str>> = CORNERS
        .iter()
        .map(|(raw, _)| {
            nested
                .get(*raw)
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str)
        })
        .collect();
    if !all_known || ids.iter().any(Option::is_none) {
        flatten_bound_variables_into(nested, path, out, collector, subject);
        return;
    }
    if ids.iter().all(|id| *id == ids[0]) {
        let handle = collector.var_ref(ids[0].unwrap_or_default());
        out.insert(path.to_owned(), json!(handle));
        return;
    }
    for ((_, short), id) in CORNERS.iter().zip(ids) {
        let handle = collector.var_ref(id.unwrap_or_default());
        out.insert(format!("{path}.{short}"), json!(handle));
    }
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

/// Build the top-level `variables` block: one entry per interned handle
/// (`v1`, `v2`, … in the order the walk encountered them), each carrying the
/// raw `id` plus — when the variables sidecar has it — name, collection,
/// type and values. `vars_root` is the raw `/v1/files/{key}/variables/local`
/// response or `None` if no sidecar; without it every entry is `{id}` only,
/// which still lets a reader see which properties share a token.
pub fn build_variables_block(vars_root: Option<&Value>, collector: &mut Collector) -> Value {
    let meta = vars_root.map(|root| root.get("meta").unwrap_or(root));
    let variables = meta
        .and_then(|m| m.get("variables"))
        .and_then(Value::as_object);
    let collections = meta
        .and_then(|m| m.get("variableCollections"))
        .and_then(Value::as_object);

    // Index loop rather than iterator: building an entry can intern alias
    // targets (see `resolve_variable_value`), which appends to
    // `var_handles` — those need entries too, so we walk until the list
    // stops growing.
    let mut out = Map::new();
    let mut i = 0;
    while i < collector.var_handles.len() {
        let id = collector.var_handles[i].clone();
        let mut entry = Map::new();
        entry.insert("id".into(), json!(id));
        if let Some(v) = variables.and_then(|m| m.get(&id)) {
            if let Value::Object(resolved) = build_variable_entry(v, collections, collector, &id) {
                entry.extend(resolved);
            }
        }
        out.insert(var_handle(i), Value::Object(entry));
        i += 1;
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
                let shown = match id.as_str() {
                    Some(id_str) => json!(collector.var_ref(id_str)),
                    None => id.clone(),
                };
                return json!({ "alias": shown });
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

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Figma's default constraints; carry no information, so they're elided.
fn is_default_constraints(c: &Value) -> bool {
    c.get("vertical").and_then(Value::as_str) == Some("TOP")
        && c.get("horizontal").and_then(Value::as_str) == Some("LEFT")
}

fn approx_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a.and_then(Value::as_f64), b.and_then(Value::as_f64)) {
        (Some(x), Some(y)) => (x - y).abs() < 0.01,
        _ => false,
    }
}

/// A descendant's `bounds` relative to its parent's absolute box. Flow
/// children (parent has auto-layout, child isn't absolutely positioned) get
/// `width`/`height` only — auto-layout computes their position. A parent
/// without a box (a CANVAS: pages have no `absoluteBoundingBox`) counts as
/// sitting at the origin, so its children come out in canvas coordinates —
/// which *is* their position relative to the page. A child box the
/// arithmetic can't read falls back to the raw value so nothing is lost.
/// Float noise from the subtraction is cleaned by `round_floats`.
fn relative_bounds(abs: &Value, node: &Value, parent: &Value) -> Value {
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
    let (Some(x), Some(y), Some(w), Some(h)) = (
        num(abs, "x"),
        num(abs, "y"),
        num(abs, "width"),
        num(abs, "height"),
    ) else {
        return abs.clone();
    };
    if is_flow_child(node, Some(parent)) {
        return json!({ "width": w, "height": h });
    }
    let pbox = parent.get("absoluteBoundingBox");
    let px = pbox.and_then(|b| num(b, "x")).unwrap_or(0.0);
    let py = pbox.and_then(|b| num(b, "y")).unwrap_or(0.0);
    json!({
        "x": x - px,
        "y": y - py,
        "width": w,
        "height": h,
    })
}

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

    /// A node carrying every major section, with a child that also has fills
    /// and layout — exercises `--only` at both depths.
    fn full_section_node() -> Value {
        json!({
            "id": "1:1", "type": "FRAME", "name": "Card",
            "absoluteBoundingBox": {"x": 0.0, "y": 0.0, "width": 100.0, "height": 50.0},
            "fills": [{"type": "SOLID", "color": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0},
                       "boundVariables": {"color": {"type": "VARIABLE_ALIAS", "id": "VariableID:42:1"}}}],
            "strokes": [{"type": "SOLID", "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}],
            "strokeWeight": 2.0,
            "layoutMode": "VERTICAL",
            "itemSpacing": 8.0,
            "children": [
                {"id": "1:2", "type": "TEXT", "name": "Title", "characters": "Hello",
                 "layoutAlign": "STRETCH",
                 "fills": [{"type": "SOLID", "color": {"r": 0.0, "g": 1.0, "b": 0.0, "a": 1.0}}]}
            ]
        })
    }

    fn only(sections: &[Section]) -> ViewOptions {
        ViewOptions {
            only: Some(sections.iter().copied().collect()),
            ..ViewOptions::default()
        }
    }

    #[test]
    fn only_fills_restricts_sections() {
        let node = full_section_node();
        let mut c = Collector::default();
        let view = build_node_view(&node, &only(&[Section::Fills]), &mut c);
        let obj = view.as_object().unwrap();
        // Identity always present.
        assert_eq!(obj["id"], "1:1");
        assert_eq!(obj["name"], "Card");
        assert!(obj.contains_key("fills"));
        // Everything else pruned; children recursion unaffected.
        for gone in ["strokes", "stroke", "layout", "bounds", "size"] {
            assert!(!obj.contains_key(gone), "unexpected `{gone}` in {obj:?}");
        }
        assert!(obj.contains_key("children"));
    }

    #[test]
    fn only_filter_recurses_into_children() {
        let node = full_section_node();
        let mut c = Collector::default();
        let view = build_node_view(&node, &only(&[Section::Fills]), &mut c);
        let child = &view["children"][0];
        assert!(child.get("fills").is_some());
        assert!(child.get("layout_child").is_none());
        assert!(child.get("text").is_none());
    }

    #[test]
    fn only_fills_still_collects_bound_variables() {
        let node = full_section_node();
        let mut c = Collector::default();
        build_node_view(&node, &only(&[Section::Fills]), &mut c);
        assert!(
            c.var_handles.contains(&"VariableID:42:1".to_owned()),
            "kept sections must keep feeding the hoisted variables block"
        );
    }

    #[test]
    fn only_geometry_does_not_collect_paint_variables() {
        let node = full_section_node();
        let mut c = Collector::default();
        let view = build_node_view(&node, &only(&[Section::Geometry]), &mut c);
        assert!(
            c.var_handles.is_empty(),
            "excluded sections must not pollute the collector"
        );
        assert!(view.get("bounds").is_some());
        assert!(view.get("fills").is_none());
    }

    #[test]
    fn wants_none_includes_everything() {
        let node = full_section_node();
        let mut c_all = Collector::default();
        let all = build_node_view(&node, &ViewOptions::default(), &mut c_all);
        let obj = all.as_object().unwrap();
        for key in ["fills", "strokes", "layout", "bounds", "children"] {
            assert!(obj.contains_key(key), "missing `{key}` in unfiltered view");
        }
        assert!(c_all.var_handles.contains(&"VariableID:42:1".to_owned()));
    }

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
        assert_eq!(v["bound_variable"], "v1");
        assert!(c.var_handles.contains(&"VariableID:42:1".to_owned()));
        assert_eq!(c.var_handles, vec!["VariableID:42:1"]);
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
        // AUTO sizing / MIN alignment are Figma's defaults and are elided.
        assert!(v.get("primary_axis").is_none(), "{v}");
        assert_eq!(
            v["counter_axis"],
            json!({"sizing": "FIXED", "align": "CENTER"})
        );
        assert_eq!(v["item_spacing"], 12);
        // Non-uniform padding is `[top, right, bottom, left]`.
        assert_eq!(v["padding"], json!([20.0, 16.0, 20.0, 16.0]));
        assert!(v.get("wrap").is_none());

        let uniform = json!({"layoutMode": "HORIZONTAL", "paddingTop": 8, "paddingRight": 8,
                             "paddingBottom": 8, "paddingLeft": 8, "layoutWrap": "WRAP",
                             "itemSpacing": 0});
        let v = build_layout(&uniform, &mut c, "1:3");
        assert_eq!(v["padding"], 8.0);
        assert_eq!(v["wrap"], "WRAP");
        assert!(v.get("item_spacing").is_none(), "zero gap is the default");
    }

    #[test]
    fn flatten_bound_variables_recurses_into_nested_maps() {
        // Per-corner radii and `size` bind through one more level of nesting.
        let bv = json!({
            "rectangleCornerRadii": {
                "RECTANGLE_TOP_LEFT_CORNER_RADIUS": {"type": "VARIABLE_ALIAS", "id": "VariableID:r"},
                "RECTANGLE_TOP_RIGHT_CORNER_RADIUS": {"type": "VARIABLE_ALIAS", "id": "VariableID:r"}
            },
            "size": {"y": {"type": "VARIABLE_ALIAS", "id": "VariableID:h"}}
        });
        let mut c = Collector::default();
        let v = flatten_bound_variables(&bv, &mut c, "1:2");
        // Two of four corners known → generic recursion keeps every binding.
        assert_eq!(
            v,
            json!({
                "rectangleCornerRadii.RECTANGLE_TOP_LEFT_CORNER_RADIUS": "v1",
                "rectangleCornerRadii.RECTANGLE_TOP_RIGHT_CORNER_RADIUS": "v1",
                "size.y": "v2"
            })
        );
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);

        let corner = |id: &str| json!({"type": "VARIABLE_ALIAS", "id": id});
        let uniform = json!({"rectangleCornerRadii": {
            "RECTANGLE_TOP_LEFT_CORNER_RADIUS": corner("VariableID:r"),
            "RECTANGLE_TOP_RIGHT_CORNER_RADIUS": corner("VariableID:r"),
            "RECTANGLE_BOTTOM_RIGHT_CORNER_RADIUS": corner("VariableID:r"),
            "RECTANGLE_BOTTOM_LEFT_CORNER_RADIUS": corner("VariableID:r"),
        }});
        let mut c = Collector::default();
        assert_eq!(
            flatten_bound_variables(&uniform, &mut c, "u"),
            json!({"rectangleCornerRadii": "v1"})
        );
        let mixed = json!({"rectangleCornerRadii": {
            "RECTANGLE_TOP_LEFT_CORNER_RADIUS": corner("VariableID:r"),
            "RECTANGLE_TOP_RIGHT_CORNER_RADIUS": corner("VariableID:r"),
            "RECTANGLE_BOTTOM_RIGHT_CORNER_RADIUS": corner("VariableID:s"),
            "RECTANGLE_BOTTOM_LEFT_CORNER_RADIUS": corner("VariableID:s"),
        }});
        let mut c = Collector::default();
        assert_eq!(
            flatten_bound_variables(&mixed, &mut c, "m"),
            json!({"rectangleCornerRadii.top_left": "v1", "rectangleCornerRadii.top_right": "v1",
                   "rectangleCornerRadii.bottom_right": "v2", "rectangleCornerRadii.bottom_left": "v2"})
        );
    }

    #[test]
    fn flatten_bound_variables_handles_arrays_and_objects() {
        let bv = json!({
            "cornerRadius": {"type": "VARIABLE_ALIAS", "id": "VariableID:a"},
            "fills": [
                {"type": "VARIABLE_ALIAS", "id": "VariableID:b"},
                {"type": "VARIABLE_ALIAS", "id": "VariableID:a"}
            ]
        });
        let mut c = Collector::default();
        let v = flatten_bound_variables(&bv, &mut c, "1:2");
        let m = v.as_object().unwrap();
        // Handles are assigned in encounter order and reused for repeats.
        assert_eq!(m["cornerRadius"], "v1");
        assert_eq!(m["fills[0]"], "v2");
        assert_eq!(m["fills[1]"], "v1");
        assert_eq!(c.var_handles, vec!["VariableID:a", "VariableID:b"]);
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
        assert_eq!(c.var_ref("VariableID:42:1"), "v1");
        let block = build_variables_block(Some(&vars_root), &mut c);
        let entry = &block["v1"];
        assert_eq!(entry["id"], "VariableID:42:1");
        assert_eq!(entry["name"], "color/brand/primary");
        assert_eq!(entry["resolved_type"], "COLOR");
        assert_eq!(entry["values_by_mode"]["Light"], "#0a85ff");
        assert_eq!(entry["collection"]["default_mode"], "Light");
    }

    #[test]
    fn variables_block_pulls_in_aliased_variable() {
        // A semantic COLOR variable whose value aliases a primitive variable.
        // Only the semantic id was referenced by a node; the block must still
        // emit the aliased primitive so `{alias: v2}` resolves to a definition.
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
        c.var_ref("VariableID:sem:1");
        let block = build_variables_block(Some(&vars_root), &mut c);
        // The alias value surfaces the target's handle…
        assert_eq!(block["v1"]["values_by_mode"]["Light"]["alias"], "v2");
        // …and the aliased primitive is pulled into the block with its own def.
        assert_eq!(block["v2"]["id"], "VariableID:prim:1");
        assert_eq!(
            block["v2"]["values_by_mode"]["Light"], "#0a85ff",
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

    /// Golden: a FRAME exercising (almost) every section plus a TEXT child at
    /// depth 1. Locks byte-exact output key order AND the final collector
    /// state, so the build_view_recursive split into add_* helpers is provably
    /// behavior-preserving. The child also locks the depth==0 drops: its
    /// `effects` / `layoutGrids` must NOT appear (depth 1).
    #[test]
    fn build_node_view_full_node_golden() {
        let node = json!({
            "id": "10:20",
            "type": "FRAME",
            "name": "Card",
            "visible": false,
            "locked": true,
            "rotation": 0.5,
            "opacity": 0.8,
            "blendMode": "MULTIPLY",
            "preserveRatio": true,
            "isMask": true,
            "maskType": "ALPHA",
            "absoluteBoundingBox": {"x": 0.0, "y": 0.0, "width": 100.0, "height": 50.0},
            "size": {"x": 100.0, "y": 50.0},
            "relativeTransform": [[1.0, 0.0, 5.0], [0.0, 1.0, 7.0]],
            "constraints": {"vertical": "TOP", "horizontal": "LEFT"},
            "minWidth": 10.0,
            "maxWidth": 200.0,
            "rectangleCornerRadii": [4.0, 4.0, 0.0, 0.0],
            "clipsContent": true,
            "fills": [{"type": "SOLID", "color": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}],
            "strokes": [{"type": "SOLID", "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}],
            "strokeWeight": 2.0,
            "effects": [{"type": "DROP_SHADOW", "color": {"r":0.0,"g":0.0,"b":0.0,"a":0.5}, "offset": {"x":0.0,"y":2.0}, "radius": 4.0, "visible": true}],
            "layoutMode": "VERTICAL",
            "layoutGrids": [{"pattern": "GRID", "sectionSize": 8.0}],
            "componentPropertyReferences": {"characters": "prop1"},
            "prototypeStartNodeID": "10:20",
            "styles": {"fill": "S:1", "text": "S:2"},
            "boundVariables": {"fills": [{"type": "VARIABLE_ALIAS", "id": "VariableID:9"}]},
            "explicitVariableModes": {"VariableCollectionId:1": "mode1"},
            "children": [
                {"id": "10:21", "type": "TEXT", "name": "Label", "characters": "Hello",
                 "effects": [{"type": "DROP_SHADOW"}], "layoutGrids": [{"pattern":"GRID"}]}
            ]
        });
        let opts = ViewOptions {
            depth: None,
            max_nodes: 500,
            prototype: false,
            meta: true,
            rich_text: false,
            only: None,
            include_hidden: false,
        };
        let mut c = Collector::default();
        let view = build_node_view(&node, &opts, &mut c);
        let got = serde_json::to_string(&view).unwrap();
        let expected = r##"{"id":"10:20","type":"FRAME","name":"Card","visible":false,"locked":true,"rotation":0.5,"opacity":0.8,"blend_mode":"MULTIPLY","preserve_ratio":true,"is_mask":true,"mask_type":"ALPHA","bounds":{"x":0.0,"y":0.0,"width":100.0,"height":50.0},"relative_transform":[[1.0,0.0,5.0],[0.0,1.0,7.0]],"size_constraints":{"min_width":10.0,"max_width":200.0},"corner":{"rectangle_corner_radii":[4.0,4.0,0.0,0.0]},"clips_content":true,"fills":[{"type":"SOLID","hex":"#ff0000"}],"strokes":[{"type":"SOLID","hex":"#000000"}],"stroke":{"weight":2.0},"effects":[{"type":"DROP_SHADOW","offset":{"x":0.0,"y":2.0},"radius":4.0,"hex":"#00000080"}],"layout":{"mode":"VERTICAL"},"layout_grids":[{"pattern":"GRID","sectionSize":8.0}],"property_refs":{"characters":"prop1"},"prototype":{"is_flow_start":true},"styles":{"fill":"S:1","text":"S:2"},"bound_variables":{"fills[0]":"v1"},"explicit_variable_modes":{"VariableCollectionId:1":"mode1"},"children":[{"id":"10:21","type":"TEXT","name":"Label","text":{"characters":"Hello"}}]}"##;
        assert_eq!(got, expected);
        assert_eq!(
            c.styles.iter().cloned().collect::<Vec<_>>(),
            vec!["S:1", "S:2"]
        );
        assert_eq!(c.var_handles, vec!["VariableID:9"]);
        assert_eq!(
            c.emitted_ids.iter().cloned().collect::<Vec<_>>(),
            vec!["10:20", "10:21"]
        );
        assert_eq!(c.emitted_descendants, 1);
        assert!(c.omitted_ids.is_empty());
    }
    // ── Projection rules (the node-info diet) ─────────────────────────────

    /// An auto-layout parent at (100,200) with: a visible flow child, an
    /// absolutely positioned child, a hidden child that has its own visible
    /// descendant, and a vector leaf.
    fn diet_fixture() -> Value {
        json!({
            "id": "1:0", "type": "FRAME", "name": "Panel",
            "absoluteBoundingBox": {"x": 100.0, "y": 200.0, "width": 400.0, "height": 300.0},
            "size": {"x": 400.0, "y": 300.0},
            "constraints": {"vertical": "TOP", "horizontal": "LEFT"},
            "layoutMode": "VERTICAL",
            "children": [
                {"id": "1:1", "type": "TEXT", "name": "Flow",
                 "absoluteBoundingBox": {"x": 120.0, "y": 210.0, "width": 80.0, "height": 20.0},
                 "constraints": {"vertical": "TOP", "horizontal": "LEFT"},
                 "layoutSizingHorizontal": "HUG", "layoutSizingVertical": "FIXED",
                 "characters": "hi",
                 "style": {"fontFamily": "Inter", "fontPostScriptName": "Inter-Medium",
                           "fontStyle": "Medium", "fontWeight": 500, "fontSize": 16.0,
                           "textAlignHorizontal": "LEFT", "textAlignVertical": "TOP",
                           "lineHeightPx": 24.0, "lineHeightPercent": 150.0,
                           "lineHeightPercentFontSize": 150.0, "lineHeightUnit": "PIXELS",
                           "textAutoResize": "HEIGHT"}},
                {"id": "1:2", "type": "FRAME", "name": "Pinned",
                 "absoluteBoundingBox": {"x": 450.3, "y": 250.0, "width": 40.0, "height": 40.0},
                 "constraints": {"vertical": "TOP", "horizontal": "RIGHT"},
                 "layoutPositioning": "ABSOLUTE"},
                {"id": "1:3", "type": "INSTANCE", "name": "Alt state", "visible": false,
                 "absoluteBoundingBox": {"x": 100.0, "y": 200.0, "width": 10.0, "height": 10.0},
                 "children": [{"id": "1:4", "type": "TEXT", "name": "Inside hidden", "characters": "x"}]},
                {"id": "1:5", "type": "VECTOR", "name": "Icon path",
                 "absoluteBoundingBox": {"x": 100.0, "y": 200.0, "width": 16.0, "height": 16.0},
                 "constraints": {"vertical": "SCALE", "horizontal": "SCALE"},
                 "layoutSizingHorizontal": "FIXED", "layoutSizingVertical": "FIXED",
                 "fills": [{"type": "SOLID", "color": {"r": 0.0, "g": 0.5, "b": 0.0, "a": 0.5},
                            "boundVariables": {"color": {"type": "VARIABLE_ALIAS", "id": "VariableID:icon"}}}]}
            ]
        })
    }

    fn child<'a>(view: &'a Value, id: &str) -> &'a Value {
        view["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no child {id} in {view}"))
    }

    #[test]
    fn hidden_children_are_pruned_and_listed_on_parent() {
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &ViewOptions::default(), &mut c);
        let ids: Vec<&str> = view["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["1:1", "1:2", "1:5"], "hidden subtree must not render");
        assert_eq!(
            view["hidden_children"],
            json!([{"id": "1:3", "name": "Alt state", "type": "INSTANCE"}])
        );
        // Pruned nodes don't consume the node budget.
        assert_eq!(c.emitted_descendants, 3);
    }

    #[test]
    fn include_hidden_restores_hidden_subtrees() {
        let opts = ViewOptions {
            include_hidden: true,
            ..ViewOptions::default()
        };
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &opts, &mut c);
        let alt = child(&view, "1:3");
        assert_eq!(alt["visible"], false, "the flag itself is still surfaced");
        assert_eq!(alt["children"][0]["id"], "1:4");
        assert!(view.get("hidden_children").is_none());
    }

    #[test]
    fn property_driven_hidden_layer_renders_inside_component_definition() {
        let icon = json!({
            "id": "12:41", "type": "INSTANCE", "name": "Leading Icon", "visible": false,
            "componentPropertyReferences": {"visible": "Show Icon#12:3"},
            "absoluteBoundingBox": {"x": 0.0, "y": 0.0, "width": 16.0, "height": 16.0},
            "layoutSizingHorizontal": "FIXED", "layoutSizingVertical": "FIXED"
        });
        let definition = json!({
            "id": "12:40", "type": "COMPONENT", "name": "Button", "layoutMode": "HORIZONTAL",
            "componentPropertyDefinitions": {"Show Icon#12:3": {"type": "BOOLEAN", "defaultValue": false}},
            "children": [icon, {"id": "12:42", "type": "TEXT", "name": "Label", "characters": "Go"}]
        });
        let mut c = Collector::default();
        let view = build_node_view(&definition, &ViewOptions::default(), &mut c);
        let icon_view = child(&view, "12:41");
        assert_eq!(icon_view["visible"], false, "the flag is still surfaced");
        assert_eq!(
            icon_view["property_refs"],
            json!({"visible": "Show Icon#12:3"})
        );
        assert_eq!(icon_view["layout_child"], json!({"sizing": "FIXED/FIXED"}));
        assert!(view.get("hidden_children").is_none());

        // Inside an instance the property is simply off: prune as usual.
        let mut instance = definition.clone();
        instance["type"] = json!("INSTANCE");
        let view = build_node_view(
            &instance,
            &ViewOptions::default(),
            &mut Collector::default(),
        );
        assert!(view["children"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["id"] != "12:41"));
        assert_eq!(view["hidden_children"][0]["id"], "12:41");
    }

    #[test]
    fn hidden_target_is_still_rendered() {
        let node = json!({"id": "9:9", "type": "FRAME", "name": "Off", "visible": false,
                          "children": [{"id": "9:10", "type": "TEXT", "name": "T", "characters": "t"}]});
        let mut c = Collector::default();
        let view = build_node_view(&node, &ViewOptions::default(), &mut c);
        assert_eq!(view["visible"], false);
        assert_eq!(view["children"][0]["id"], "9:10");
    }

    #[test]
    fn default_constraints_elided_non_default_kept() {
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &ViewOptions::default(), &mut c);
        assert!(
            view.get("constraints").is_none(),
            "LEFT/TOP is Figma's default"
        );
        assert!(child(&view, "1:1").get("constraints").is_none());
        assert_eq!(
            child(&view, "1:2")["constraints"],
            json!({"vertical": "TOP", "horizontal": "RIGHT"})
        );
    }

    #[test]
    fn descendant_bounds_are_parent_relative_and_flow_children_drop_xy() {
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &ViewOptions::default(), &mut c);
        // Target keeps absolute canvas coordinates; `size` equal to bounds is dropped.
        assert_eq!(
            view["bounds"],
            json!({"x": 100.0, "y": 200.0, "width": 400.0, "height": 300.0})
        );
        assert!(view.get("size").is_none());
        // Flow child of an auto-layout parent: position is the layout's job.
        assert_eq!(
            child(&view, "1:1")["bounds"],
            json!({"width": 80.0, "height": 20.0})
        );
        // Absolutely positioned child: offset from the parent's top-left.
        assert_eq!(
            child(&view, "1:2")["bounds"],
            json!({"x": 350.3, "y": 50.0, "width": 40.0, "height": 40.0})
        );
    }

    #[test]
    fn relative_bounds_treat_a_boxless_parent_as_the_origin() {
        // A CANVAS has no absoluteBoundingBox; its children's canvas
        // coordinates are their position relative to the page.
        let parent = json!({"id": "0:1", "type": "CANVAS", "name": "Page 1"});
        let node = json!({"id": "n"});
        let abs = json!({"x": -5.0, "y": 6.0, "width": 7.0, "height": 8.0});
        assert_eq!(
            relative_bounds(&abs, &node, &parent),
            json!({"x": -5.0, "y": 6.0, "width": 7.0, "height": 8.0})
        );
    }

    #[test]
    fn geometry_leaf_below_target_drops_only_the_layout_block_that_cannot_apply() {
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &ViewOptions::default(), &mut c);
        // Flow child of an auto-layout parent: constraints are ignored by
        // Figma, but the layout-child settings are exactly what sizes it.
        let vec = child(&view, "1:5");
        assert!(vec.get("constraints").is_none());
        assert_eq!(vec["layout_child"], json!({"sizing": "FIXED/FIXED"}));
        assert_eq!(
            vec["fills"],
            json!([{"type": "SOLID", "hex": "#00800080", "bound_variable": "v1"}])
        );
        // A full-width divider must not degrade to a fixed-width line.
        let card = json!({
            "id": "2:0", "type": "FRAME", "name": "Card", "layoutMode": "VERTICAL",
            "absoluteBoundingBox": {"x": 0.0, "y": 0.0, "width": 328.0, "height": 100.0},
            "children": [{
                "id": "2:1", "type": "LINE", "name": "Divider",
                "absoluteBoundingBox": {"x": 0.0, "y": 50.0, "width": 328.0, "height": 0.0},
                "layoutAlign": "STRETCH", "layoutSizingHorizontal": "FILL",
                "layoutSizingVertical": "FIXED"
            }]
        });
        let view = build_node_view(&card, &ViewOptions::default(), &mut Collector::default());
        assert_eq!(
            child(&view, "2:1")["layout_child"],
            json!({"align": "STRETCH", "sizing": "FILL/FIXED"})
        );
        // In a plain frame the roles flip: constraints decide resizing and
        // there is no auto-layout to read the layout-child settings.
        let plain = json!({
            "id": "3:0", "type": "FRAME", "name": "Plain",
            "absoluteBoundingBox": {"x": 0.0, "y": 0.0, "width": 328.0, "height": 100.0},
            "children": [{
                "id": "3:1", "type": "LINE", "name": "Rule",
                "absoluteBoundingBox": {"x": 0.0, "y": 50.0, "width": 328.0, "height": 0.0},
                "constraints": {"vertical": "TOP", "horizontal": "LEFT_RIGHT"},
                "layoutSizingHorizontal": "FIXED", "layoutSizingVertical": "FIXED"
            }]
        });
        let view = build_node_view(&plain, &ViewOptions::default(), &mut Collector::default());
        let rule = child(&view, "3:1");
        assert_eq!(
            rule["constraints"],
            json!({"vertical": "TOP", "horizontal": "LEFT_RIGHT"})
        );
        assert!(rule.get("layout_child").is_none());
        // The same node as a *target* keeps its constraints.
        let raw_vec = diet_fixture()["children"][3].clone();
        let mut c2 = Collector::default();
        let solo = build_node_view(&raw_vec, &ViewOptions::default(), &mut c2);
        assert_eq!(
            solo["constraints"],
            json!({"vertical": "SCALE", "horizontal": "SCALE"})
        );
    }

    #[test]
    fn text_style_drops_redundant_fields_and_defaults() {
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &ViewOptions::default(), &mut c);
        let style = &child(&view, "1:1")["text"]["style"];
        assert_eq!(
            style,
            &json!({"font_family": "Inter", "font_style": "Medium", "font_weight": 500,
                    "font_size": 16.0, "line_height_px": 24.0, "text_auto_resize": "HEIGHT"})
        );
        // Non-default alignment / unit survive.
        let mut c2 = Collector::default();
        let s = build_type_style(
            &json!({"textAlignHorizontal": "CENTER", "textAlignVertical": "BOTTOM",
                    "lineHeightUnit": "FONT_SIZE_%"}),
            &mut c2,
            "t",
        );
        assert_eq!(s["text_align_horizontal"], "CENTER");
        assert_eq!(s["text_align_vertical"], "BOTTOM");
        assert_eq!(s["line_height_unit"], "FONT_SIZE_%");
    }

    #[test]
    fn layout_child_sizing_is_one_string() {
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &ViewOptions::default(), &mut c);
        assert_eq!(
            child(&view, "1:1")["layout_child"],
            json!({"sizing": "HUG/FIXED"})
        );
    }

    #[test]
    fn component_properties_split_into_variants_and_scalars() {
        let node = json!({
            "id": "c:1", "type": "INSTANCE", "name": "Banner", "componentId": "7:7",
            "componentProperties": {
                "Intent": {"type": "VARIANT", "value": "Success", "boundVariables": {}},
                "Close#1610:0": {"type": "BOOLEAN", "value": false},
                "✏️ Title#3063:150": {"type": "TEXT", "value": "Approved",
                    "boundVariables": {"value": {"type": "VARIABLE_ALIAS", "id": "VariableID:t"}}},
                "Icon#1:1": {"type": "INSTANCE_SWAP", "value": "687:73548",
                    "preferredValues": [{"type": "COMPONENT", "key": "abc"}]},
                "Slot#1:2": {"type": "INSTANCE_SWAP", "value": "687:1", "preferredValues": []},
                "Label#2:1": {"type": "TEXT", "value": "a"},
                "Label#2:2": {"type": "TEXT", "value": "b"}
            }
        });
        let mut c = Collector::default();
        let view = build_node_view(&node, &ViewOptions::default(), &mut c);
        let comp = &view["component"];
        assert_eq!(comp["component_id"], "7:7");
        assert_eq!(comp["variants"], json!({"Intent": "Success"}));
        assert_eq!(
            comp["properties"],
            json!({
                "Close": false,
                "Icon": {"instance": "687:73548", "preferred": [{"type": "COMPONENT", "key": "abc"}]},
                "Slot": {"instance": "687:1"},
                // Colliding stripped names keep the raw `#n:m` suffix.
                "Label#2:1": "a",
                "Label#2:2": "b",
                "✏️ Title": {"value": "Approved", "bound_variables": {"value": "v1"}},
            })
        );
        assert!(comp.get("component_properties").is_none());
    }

    #[test]
    fn strip_property_suffix_only_removes_figma_refs() {
        assert_eq!(strip_property_suffix("Close#1610:0"), "Close");
        assert_eq!(strip_property_suffix("C#1"), "C#1");
        assert_eq!(strip_property_suffix("Item #2"), "Item #2");
        assert_eq!(strip_property_suffix("#1:2"), "#1:2");
        assert_eq!(strip_property_suffix("Plain"), "Plain");
    }

    #[test]
    fn property_refs_only_on_target_or_inside_component_definitions() {
        let refs = json!({"visible": "Close#1:0"});
        let inst = json!({"id": "i", "type": "INSTANCE", "name": "I",
            "componentPropertyReferences": refs,
            "children": [{"id": "i:1", "type": "TEXT", "name": "T", "characters": "t",
                          "componentPropertyReferences": refs}]});
        let mut c = Collector::default();
        let view = build_node_view(&inst, &ViewOptions::default(), &mut c);
        assert_eq!(view["property_refs"], refs, "kept on the target");
        assert!(
            view["children"][0].get("property_refs").is_none(),
            "noise inside an instance"
        );

        let comp = json!({"id": "c", "type": "COMPONENT", "name": "C",
            "children": [{"id": "c:1", "type": "TEXT", "name": "T", "characters": "t",
                          "componentPropertyReferences": refs}]});
        let mut c2 = Collector::default();
        let view = build_node_view(&comp, &ViewOptions::default(), &mut c2);
        assert_eq!(
            view["children"][0]["property_refs"], refs,
            "kept inside a definition"
        );
    }

    #[test]
    fn variables_block_without_sidecar_lists_ids_per_handle() {
        let mut c = Collector::default();
        let view = build_node_view(&diet_fixture(), &ViewOptions::default(), &mut c);
        assert_eq!(child(&view, "1:5")["fills"][0]["bound_variable"], "v1");
        let block = build_variables_block(None, &mut c);
        assert_eq!(block, json!({"v1": {"id": "VariableID:icon"}}));
    }

    #[test]
    fn effect_color_is_hex_only_and_bindings_use_handles() {
        let mut c = Collector::default();
        let e = build_effect(
            &json!({"type": "DROP_SHADOW", "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 0.25},
                    "offset": {"x": 0.0, "y": 1.0}, "radius": 2.0,
                    "boundVariables": {"color": {"type": "VARIABLE_ALIAS", "id": "VariableID:sh"}}}),
            &mut c,
            "n",
        );
        assert!(e.get("color").is_none());
        assert_eq!(e["hex"], "#00000040");
        assert_eq!(e["bound_variables"], json!({"color": "v1"}));
    }

    #[test]
    fn paint_bindings_mirrored_on_paints_are_dropped_from_node_map() {
        let node = json!({
            "id": "1:2", "type": "RECTANGLE", "name": "R",
            "fills": [
                {"type": "SOLID", "color": {"r":1.0,"g":0.0,"b":0.0,"a":1.0},
                 "boundVariables": {"color": {"type": "VARIABLE_ALIAS", "id": "VariableID:a"}}},
                {"type": "GRADIENT_LINEAR", "gradientStops": []}
            ],
            "strokes": [{"type": "SOLID", "color": {"r":0.0,"g":0.0,"b":0.0,"a":1.0}}],
            "boundVariables": {
                "fills": [
                    {"type": "VARIABLE_ALIAS", "id": "VariableID:a"},
                    {"type": "VARIABLE_ALIAS", "id": "VariableID:g"}
                ],
                "strokes": [{"type": "VARIABLE_ALIAS", "id": "VariableID:s"}],
                "itemSpacing": {"type": "VARIABLE_ALIAS", "id": "VariableID:i"}
            }
        });
        let mut c = Collector::default();
        let v = build_node_view(&node, &ViewOptions::default(), &mut c);
        assert_eq!(v["fills"][0]["bound_variable"], "v1");
        let bv = v["bound_variables"].as_object().unwrap();
        assert!(
            !bv.contains_key("fills[0]"),
            "mirrored on fills[0].bound_variable: {bv:?}"
        );
        assert_eq!(
            bv["fills[1]"], "v2",
            "gradient paint carries no bound_variable → kept"
        );
        assert_eq!(
            bv["strokes[0]"], "v3",
            "stroke paint has no binding of its own → kept"
        );
        assert_eq!(bv["itemSpacing"], "v4");

        // Without the paints section the node map is the only carrier → kept.
        let only_vars = ViewOptions {
            only: Some([Section::Variables].into_iter().collect()),
            ..Default::default()
        };
        let v = build_node_view(&node, &only_vars, &mut Collector::default());
        assert_eq!(v["bound_variables"]["fills[0]"], "v1");
    }

    #[test]
    fn floats_are_rounded_to_three_decimals_everywhere() {
        let node = json!({
            "id": "1:2", "type": "FRAME", "name": "F",
            "absoluteBoundingBox": {"x": 0.0, "y": 0.0, "width": 14.40007495880127, "height": 4.800076007843018},
            "opacity": 0.30000001192092896,
            "strokes": [{"type": "SOLID", "color": {"r":0.0,"g":0.0,"b":0.0,"a":1.0}}],
            "strokeWeight": 1.3333333730697632,
            "layoutMode": "HORIZONTAL", "itemSpacing": 6.666666507720947,
            "children": [{
                "id": "1:3", "type": "TEXT", "name": "T",
                "absoluteBoundingBox": {"x": 0.3333, "y": 0.0, "width": 10.0, "height": 10.0},
                "characters": "x", "style": {"letterSpacing": 0.20000000298023224}
            }]
        });
        let v = build_node_view(&node, &ViewOptions::default(), &mut Collector::default());
        assert_eq!(v["bounds"]["width"], 14.4);
        assert_eq!(v["bounds"]["height"], 4.8);
        assert_eq!(v["opacity"], 0.3);
        assert_eq!(v["stroke"]["weight"], 1.333);
        assert_eq!(v["layout"]["item_spacing"], 6.667);
        assert_eq!(v["children"][0]["text"]["style"]["letter_spacing"], 0.2);
    }
}
