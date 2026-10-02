//! File-level summary block for `node-info` on a file target: counts, page
//! list, components, component sets, styles, variable collections, and recent
//! comments. Split out of `node_view` because it shapes a *file* overview, not
//! a node view — `node_view` is strictly per-node shaping.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::node::{children, is_visible};
use crate::node_view::style_value;

/// Per-list display caps. The full totals are always reported under `counts`;
/// these only bound how many individual entries are inlined.
const COMPONENTS_CAP: usize = 50;
const COMPONENT_SETS_CAP: usize = 50;
const STYLES_CAP: usize = 100;

/// Build the `file_summary` block for a file target: counts, page list,
/// components, component sets, styles, variable collections, recent comments.
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
                .take(COMPONENTS_CAP)
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
                .take(COMPONENT_SETS_CAP)
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
            let shown: Vec<(&String, &Value)> = m.iter().take(STYLES_CAP).collect();
            let mut values = resolve_style_values(file_root.get("document"), &shown);
            let entries: Vec<Value> = shown
                .iter()
                .map(|(id, v)| {
                    let mut entry = json!({
                        "id": id,
                        "key": v.get("key").cloned().unwrap_or(Value::Null),
                        "name": v.get("name").cloned().unwrap_or(Value::Null),
                        "type": v.get("styleType").cloned().unwrap_or(Value::Null),
                    });
                    // A library copy can share its key and name with a local
                    // style yet carry a different value; flag which is which.
                    if v.get("remote").and_then(Value::as_bool) == Some(true) {
                        entry["remote"] = json!(true);
                    }
                    if let Some(value) = values.remove(id.as_str()) {
                        entry["value"] = value;
                    }
                    entry
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
    // Self-contained node count from the raw document. (The caller's FileMeta
    // also carries a cached node_count; this keeps the summary independent of
    // it.)
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

/// Resolve each listed style's value through the first visible node that
/// applies it (see [`style_value`]). One pre-order walk that skips hidden
/// subtrees and stops once every style has a value; styles no visible node
/// uses are simply absent from the result.
fn resolve_style_values(
    document: Option<&Value>,
    styles: &[(&String, &Value)],
) -> HashMap<String, Value> {
    let wanted: HashMap<&str, &str> = styles
        .iter()
        .filter_map(|(id, v)| Some((id.as_str(), v.get("styleType")?.as_str()?)))
        .collect();
    let mut found = HashMap::new();
    if let Some(doc) = document {
        resolve_style_values_rec(doc, &wanted, &mut found, 0);
    }
    found
}

fn resolve_style_values_rec(
    node: &Value,
    wanted: &HashMap<&str, &str>,
    found: &mut HashMap<String, Value>,
    depth: usize,
) {
    if found.len() == wanted.len() || !is_visible(node) || depth >= crate::MAX_NODE_DEPTH {
        return;
    }
    if let Some(map) = node.get("styles").and_then(Value::as_object) {
        for (slot, sid) in map {
            let Some(sid) = sid.as_str() else { continue };
            let Some(style_type) = wanted.get(sid) else {
                continue;
            };
            if found.contains_key(sid) {
                continue;
            }
            if let Some(value) = style_value(node, slot, style_type) {
                found.insert(sid.to_owned(), value);
            }
        }
    }
    for c in children(node) {
        resolve_style_values_rec(c, wanted, found, depth + 1);
    }
}

fn count_nodes_value(node: Option<&Value>) -> usize {
    count_nodes_value_rec(node, 0)
}

fn count_nodes_value_rec(node: Option<&Value>, depth: usize) -> usize {
    let Some(n) = node else { return 0 };
    if depth >= crate::MAX_NODE_DEPTH {
        eprintln!(
            "file_summary: node tree exceeded max depth {}; truncating count",
            crate::MAX_NODE_DEPTH
        );
        return 1;
    }
    let mut count = 1;
    if let Some(arr) = n.get("children").and_then(Value::as_array) {
        for c in arr {
            count += count_nodes_value_rec(Some(c), depth + 1);
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles_of(file: &Value) -> Vec<Value> {
        build_file_summary(file, None, 0, None)["styles"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn style_named<'a>(styles: &'a [Value], name: &str) -> &'a Value {
        styles.iter().find(|s| s["name"] == name).unwrap()
    }

    #[test]
    fn style_values_resolve_through_using_nodes() {
        let red = json!({ "r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0 });
        let file = json!({
            "styles": {
                "S:fill": { "name": "Red", "styleType": "FILL" },
                "S:line": { "name": "Line", "styleType": "FILL" },
                "S:text": { "name": "Body", "styleType": "TEXT" },
                "S:fx":   { "name": "Shadow", "styleType": "EFFECT" },
            },
            "document": { "id": "0:0", "type": "DOCUMENT", "children": [
                { "id": "1:1", "type": "RECTANGLE",
                  "styles": { "fill": "S:fill", "stroke": "S:line", "effect": "S:fx" },
                  "fills": [{ "type": "SOLID", "color": red }],
                  "strokes": [{ "type": "SOLID", "color": { "r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0 } }],
                  "effects": [
                      { "type": "DROP_SHADOW", "radius": 4.0, "offset": { "x": 0.0, "y": 2.0 },
                        "color": { "r": 0.0, "g": 0.0, "b": 0.0, "a": 0.5 } },
                      { "type": "LAYER_BLUR", "radius": 9.0, "visible": false },
                  ] },
                { "id": "1:2", "type": "TEXT", "styles": { "text": "S:text" },
                  "style": { "fontFamily": "Inter", "fontWeight": 500, "fontSize": 16.0,
                             "lineHeightPx": 24.0, "textAlignHorizontal": "CENTER",
                             "textAutoResize": "HEIGHT" } },
            ]},
        });
        let styles = styles_of(&file);
        assert_eq!(style_named(&styles, "Red")["value"], json!("#ff0000"));
        assert_eq!(style_named(&styles, "Line")["value"], json!("#000000"));
        assert_eq!(
            style_named(&styles, "Body")["value"],
            json!({ "font_family": "Inter", "font_weight": 500, "font_size": 16.0, "line_height_px": 24.0 }),
            "node-specific alignment/auto-resize must not leak into a TEXT style value",
        );
        assert_eq!(
            style_named(&styles, "Shadow")["value"],
            json!([{ "type": "DROP_SHADOW", "offset": { "x": 0.0, "y": 2.0 }, "radius": 4.0, "hex": "#00000080" }]),
        );
    }

    #[test]
    fn style_values_strip_variable_handles_at_any_depth() {
        // The file summary has no `variables` block, so a `vN` handle — here
        // on a gradient stop, nested below the paint — would point nowhere.
        let file = json!({
            "styles": { "S:grad": { "name": "Sunset", "styleType": "FILL" } },
            "document": { "id": "0:0", "type": "DOCUMENT", "children": [
                { "id": "1:1", "type": "RECTANGLE", "styles": { "fill": "S:grad" },
                  "fills": [{ "type": "GRADIENT_LINEAR", "gradientStops": [
                      { "position": 0.0, "color": { "r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0 },
                        "boundVariables": { "color": { "id": "VariableID:1:2" } } },
                      { "position": 1.0, "color": { "r": 0.0, "g": 0.0, "b": 1.0, "a": 1.0 } },
                  ]}] },
            ]},
        });
        let value = style_named(&styles_of(&file), "Sunset")["value"].clone();
        assert_eq!(
            value[0]["stops"],
            json!([{ "position": 0.0, "hex": "#ff0000" }, { "position": 1.0, "hex": "#0000ff" }]),
        );
        assert!(!value.to_string().contains("bound_variable"));
    }

    #[test]
    fn style_values_skip_hidden_and_unused_styles() {
        let file = json!({
            "styles": {
                "S:hidden": { "name": "Hidden", "styleType": "FILL" },
                "S:unused": { "name": "Unused", "styleType": "FILL" },
                "S:grid":   { "name": "Grid", "styleType": "GRID" },
            },
            "document": { "id": "0:0", "type": "DOCUMENT", "children": [
                { "id": "1:1", "type": "FRAME", "visible": false, "children": [
                    { "id": "1:2", "type": "RECTANGLE", "styles": { "fill": "S:hidden" },
                      "fills": [{ "type": "SOLID", "color": { "r": 1.0, "g": 1.0, "b": 1.0 } }] },
                ]},
                { "id": "1:3", "type": "FRAME", "styles": { "grid": "S:grid" },
                  "layoutGrids": [{ "pattern": "COLUMNS" }] },
            ]},
        });
        let styles = styles_of(&file);
        for name in ["Hidden", "Unused", "Grid"] {
            assert!(
                style_named(&styles, name).get("value").is_none(),
                "{name} should carry no value"
            );
        }
    }

    #[test]
    fn count_nodes_value_caps_depth_on_deep_tree() {
        // Large-stack thread: the deep input Value's recursive `Drop` would
        // otherwise overflow the ~2MB test-thread stack. The cap is what
        // bounds the count's own recursion; the assertion proves it engages.
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let mut node = json!({ "id": "leaf", "type": "FRAME" });
                for _ in 0..(crate::MAX_NODE_DEPTH + 50) {
                    node = json!({ "id": "n", "type": "FRAME", "children": [node] });
                }
                // Root at depth 0 … the node at depth MAX_NODE_DEPTH counts
                // itself but descends no further: MAX_NODE_DEPTH + 1 counted.
                assert_eq!(count_nodes_value(Some(&node)), crate::MAX_NODE_DEPTH + 1);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
