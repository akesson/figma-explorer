//! File-level summary block for `node-info` on a file target: counts, page
//! list, components, component sets, styles, variable collections, and recent
//! comments. Split out of `node_view` because it shapes a *file* overview, not
//! a node view — `node_view` is strictly per-node shaping.

use serde_json::{json, Value};

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
            let entries: Vec<Value> = m
                .iter()
                .take(STYLES_CAP)
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
