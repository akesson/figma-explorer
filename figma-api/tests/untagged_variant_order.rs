//! Untagged enums pick the *first* variant that deserializes, and serde
//! ignores unknown fields — so a variant whose fields are a superset of an
//! earlier one's can never be chosen unless it is listed first.
//! `scripts/postprocess.py` reorders such enums; these tests pin the result.
//!
//! The JSON is the exact `client_meta` shape of real
//! `/v1/files/{key}/comments` responses (2026-10-03).

use figma_api::models::{CommentClientMeta, PostCommentRequestClientMeta};
use serde_json::json;

fn variant(m: &CommentClientMeta) -> &'static str {
    match m {
        CommentClientMeta::Vector(_) => "Vector",
        CommentClientMeta::FrameOffset(_) => "FrameOffset",
        CommentClientMeta::Region(_) => "Region",
        CommentClientMeta::FrameOffsetRegion(_) => "FrameOffsetRegion",
    }
}

fn parse(v: serde_json::Value) -> CommentClientMeta {
    serde_json::from_value(v).unwrap()
}

#[test]
fn comment_client_meta_picks_the_most_specific_variant() {
    let region_on_node = parse(json!({
        "node_id": "2:12", "node_offset": {"x": 844, "y": 2254}, "stable_path": ["2:12"],
        "region_width": 406, "region_height": 110, "comment_pin_corner": "bottom-right"
    }));
    assert_eq!(variant(&region_on_node), "FrameOffsetRegion");
    let CommentClientMeta::FrameOffsetRegion(r) = region_on_node else {
        unreachable!()
    };
    assert_eq!((r.region_width, r.region_height), (406.0, 110.0));

    let region_on_canvas = parse(json!({
        "x": 10, "y": 20, "region_width": 300, "region_height": 200
    }));
    assert_eq!(variant(&region_on_canvas), "Region");

    let node = parse(json!({
        "node_id": "237:17351", "node_offset": {"x": 213, "y": 202}, "stable_path": ["237:17351"]
    }));
    assert_eq!(variant(&node), "FrameOffset");

    let point = parse(json!({"x": 1.5, "y": 2.5}));
    assert_eq!(variant(&point), "Vector");
}

#[test]
fn post_comment_client_meta_picks_the_most_specific_variant() {
    let m: PostCommentRequestClientMeta = serde_json::from_value(json!({
        "node_id": "2:12", "node_offset": {"x": 1, "y": 2},
        "region_width": 4, "region_height": 5
    }))
    .unwrap();
    assert!(
        matches!(m, PostCommentRequestClientMeta::FrameOffsetRegion(_)),
        "{m:?}"
    );
    let m: PostCommentRequestClientMeta = serde_json::from_value(json!({
        "x": 1, "y": 2, "region_width": 4, "region_height": 5
    }))
    .unwrap();
    assert!(
        matches!(m, PostCommentRequestClientMeta::Region(_)),
        "{m:?}"
    );
}

#[test]
fn variable_color_keeps_alpha() {
    use figma_api::models::{VariableDataValue, VariableValue};
    let rgba = json!({"r": 0.1, "g": 0.2, "b": 0.3, "a": 0.5});
    let v: VariableValue = serde_json::from_value(rgba.clone()).unwrap();
    assert!(matches!(v, VariableValue::Rgba(_)), "{v:?}");
    let v: VariableDataValue = serde_json::from_value(rgba).unwrap();
    assert!(matches!(v, VariableDataValue::Rgba(_)), "{v:?}");
    let v: VariableValue = serde_json::from_value(json!({"r": 0.1, "g": 0.2, "b": 0.3})).unwrap();
    assert!(matches!(v, VariableValue::Rgb(_)), "{v:?}");
}
