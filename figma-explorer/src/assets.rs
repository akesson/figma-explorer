//! Asset extraction: walk a frame, classify each leaf as icon / image /
//! composite, batch-render via the Figma image endpoints, and write the
//! files into a directory tree on disk.
//!
//! Layout (matches figma-mcp):
//!   <out>/icons/{slug}.svg
//!   <out>/images/{slug}.png
//!   <out>/images/composites/{slug}.png
//!   <out>/manifest.json

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use figma_api::apis::configuration::Configuration;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};

use crate::node::{bounds, children, has_image_fill, id, is_visible, name, type_str};
use crate::screenshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum AssetKind {
    Icon,
    Image,
    Composite,
}

impl AssetKind {
    fn subdir(self) -> &'static str {
        match self {
            Self::Icon => "icons",
            Self::Image => "images",
            Self::Composite => "images/composites",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Icon => "svg",
            Self::Image | Self::Composite => "png",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AssetSpec {
    pub node_id: String,
    pub original_name: String,
    pub kind: AssetKind,
}

#[derive(Serialize)]
pub struct ManifestEntry {
    pub path: String,
    pub original_name: String,
    pub node_id: String,
    pub kind: AssetKind,
}

#[derive(Serialize)]
pub struct Manifest {
    pub icons: Vec<ManifestEntry>,
    pub images: Vec<ManifestEntry>,
    pub composites: Vec<ManifestEntry>,
    pub failed: Vec<FailureEntry>,
}

#[derive(Serialize)]
pub struct FailureEntry {
    pub node_id: String,
    pub original_name: String,
    pub kind: AssetKind,
    pub error: String,
}

/// Walk `frame` and produce the list of nodes to export, classified.
///
/// Heuristics (mirroring figma-mcp):
/// * Icon: VECTOR, BOOLEAN_OPERATION, or an INSTANCE/COMPONENT whose name
///   contains "icon", AND whose bounding box is ≤ 64×64.
/// * Composite: GROUP/FRAME containing ≥ 2 image-bearing descendants, or
///   whose name ends with `_composite`.
/// * Image: any node carrying a visible IMAGE paint that isn't itself a
///   composite root.
///
/// Returns specs in deterministic DFS order. Duplicates across kinds are
/// avoided (a node won't appear twice).
pub fn classify(frame: &Value) -> Vec<AssetSpec> {
    let mut out: Vec<AssetSpec> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    walk(frame, &mut out, &mut seen);
    out
}

fn walk(node: &Value, out: &mut Vec<AssetSpec>, seen: &mut HashSet<String>) {
    if !is_visible(node) {
        return;
    }

    let kind = classify_node(node);
    if let Some(kind) = kind {
        if let Some(id_str) = id(node) {
            if seen.insert(id_str.to_owned()) {
                out.push(AssetSpec {
                    node_id: id_str.to_owned(),
                    original_name: name(node).unwrap_or("untitled").to_owned(),
                    kind,
                });
                // A composite/icon is exported as a single asset — don't
                // recurse into its children (they're part of the asset).
                if matches!(kind, AssetKind::Composite | AssetKind::Icon) {
                    return;
                }
            }
        }
    }

    for c in children(node) {
        walk(c, out, seen);
    }
}

fn classify_node(node: &Value) -> Option<AssetKind> {
    let ty = type_str(node)?;
    let nm = name(node).unwrap_or("");
    let small = bounds(node)
        .map(|b| b.width <= 64.0 && b.height <= 64.0)
        .unwrap_or(false);
    let name_says_icon = nm.to_lowercase().contains("icon");
    let name_says_composite = nm.to_lowercase().ends_with("_composite");

    // Composite first — it wins over the per-child image classification.
    if matches!(ty, "GROUP" | "FRAME") {
        let image_descendants = count_image_descendants(node);
        if name_says_composite || image_descendants >= 2 {
            return Some(AssetKind::Composite);
        }
    }

    // Icon-shaped nodes.
    match ty {
        "VECTOR" | "BOOLEAN_OPERATION" => return Some(AssetKind::Icon),
        "COMPONENT" | "INSTANCE" if name_says_icon && small => return Some(AssetKind::Icon),
        _ => {}
    }

    // Plain images (single image fill).
    if has_image_fill(node) {
        return Some(AssetKind::Image);
    }

    None
}

fn count_image_descendants(node: &Value) -> usize {
    let mut count = 0;
    crate::node::walk_visible(node, |n| {
        if has_image_fill(n) {
            count += 1;
        }
    });
    count
}

/// Extract all assets in `frame` and write them into `out_dir`. Returns the
/// manifest describing what was written.
pub async fn extract(
    cfg: &Configuration,
    file_key: &str,
    frame: &Value,
    out_dir: &Path,
) -> Result<Manifest> {
    let specs = classify(frame);

    let icons_dir = out_dir.join("icons");
    let images_dir = out_dir.join("images");
    let composites_dir = images_dir.join("composites");
    for d in [&icons_dir, &images_dir, &composites_dir] {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }

    // Resolve filename collisions deterministically.
    let mut paths: HashMap<String, PathBuf> = HashMap::new();
    let mut used: HashSet<PathBuf> = HashSet::new();
    for spec in &specs {
        let base = format!("{}.{}", slugify(&spec.original_name), spec.kind.extension());
        let dir = out_dir.join(spec.kind.subdir());
        let mut candidate = dir.join(&base);
        let mut counter = 1;
        while used.contains(&candidate) {
            let stem = slugify(&spec.original_name);
            candidate = dir.join(format!("{}-{}.{}", stem, counter, spec.kind.extension()));
            counter += 1;
        }
        used.insert(candidate.clone());
        paths.insert(spec.node_id.clone(), candidate);
    }

    // Render URLs in two batches (icons → SVG, images+composites → PNG).
    let (icon_specs, raster_specs): (Vec<_>, Vec<_>) =
        specs.iter().partition(|s| s.kind == AssetKind::Icon);

    let icon_ids: Vec<String> = icon_specs.iter().map(|s| s.node_id.clone()).collect();
    let raster_ids: Vec<String> = raster_specs.iter().map(|s| s.node_id.clone()).collect();

    let svg_urls =
        screenshot::render_urls(cfg, file_key, &icon_ids, 1.0, screenshot::Format::Svg).await?;
    let png_urls =
        screenshot::render_urls(cfg, file_key, &raster_ids, 2.0, screenshot::Format::Png).await?;

    let client = reqwest::Client::new();
    let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
    for spec in &specs {
        let urls = if spec.kind == AssetKind::Icon {
            &svg_urls
        } else {
            &png_urls
        };
        let path = paths.get(&spec.node_id).cloned();
        let spec_clone = spec.clone();
        match urls.get(&spec.node_id).cloned() {
            None => {
                tasks.push(Box::pin(async move {
                    FetchResult::failure(
                        spec_clone,
                        "no render URL returned by Figma".to_string(),
                        path,
                    )
                })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = FetchResult> + Send>,
                    >);
            }
            Some(url) => {
                let client = client.clone();
                tasks.push(Box::pin(async move {
                    match download_to(&client, &url, path.as_deref()).await {
                        Ok(()) => FetchResult::success(spec_clone, path),
                        Err(e) => FetchResult::failure(spec_clone, e.to_string(), path),
                    }
                }));
            }
        }
    }

    let mut icons = Vec::new();
    let mut images = Vec::new();
    let mut composites = Vec::new();
    let mut failed = Vec::new();
    while let Some(res) = tasks.next().await {
        match res {
            FetchResult::Ok { spec, path } => {
                let entry = ManifestEntry {
                    path: path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    original_name: spec.original_name,
                    node_id: spec.node_id,
                    kind: spec.kind,
                };
                match spec.kind {
                    AssetKind::Icon => icons.push(entry),
                    AssetKind::Image => images.push(entry),
                    AssetKind::Composite => composites.push(entry),
                }
            }
            FetchResult::Err { spec, error, .. } => failed.push(FailureEntry {
                node_id: spec.node_id,
                original_name: spec.original_name,
                kind: spec.kind,
                error,
            }),
        }
    }

    let manifest = Manifest {
        icons,
        images,
        composites,
        failed,
    };
    let manifest_json = serde_json::to_string_pretty(&json!(&manifest))?;
    std::fs::write(out_dir.join("manifest.json"), manifest_json)
        .with_context(|| format!("writing manifest to {}", out_dir.display()))?;
    Ok(manifest)
}

enum FetchResult {
    Ok {
        spec: AssetSpec,
        path: Option<PathBuf>,
    },
    Err {
        spec: AssetSpec,
        error: String,
        #[allow(dead_code)]
        path: Option<PathBuf>,
    },
}

impl FetchResult {
    fn success(spec: AssetSpec, path: Option<PathBuf>) -> Self {
        Self::Ok { spec, path }
    }
    fn failure(spec: AssetSpec, error: String, path: Option<PathBuf>) -> Self {
        Self::Err { spec, error, path }
    }
}

async fn download_to(client: &reqwest::Client, url: &str, path: Option<&Path>) -> Result<()> {
    let bytes = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .bytes()
        .await?;
    if let Some(path) = path {
        std::fs::write(path, &bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

// Provide the helper expected by `screenshot::render_urls` (we use HashMap above).
#[allow(dead_code)]
fn _ensure_compile() {
    let _: HashMap<String, String> = HashMap::new();
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("asset");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_is_classified_as_icon() {
        let n = json!({
            "id": "1", "type": "VECTOR", "name": "chevron",
            "absoluteBoundingBox": { "x": 0.0, "y": 0.0, "width": 24.0, "height": 24.0 }
        });
        assert_eq!(classify_node(&n), Some(AssetKind::Icon));
    }

    #[test]
    fn image_fill_is_classified_as_image() {
        let n = json!({
            "id": "1", "type": "RECTANGLE", "name": "hero-bg",
            "fills": [{ "type": "IMAGE", "imageRef": "abc", "scaleMode": "FILL", "blendMode": "NORMAL" }]
        });
        assert_eq!(classify_node(&n), Some(AssetKind::Image));
    }

    #[test]
    fn group_with_two_images_is_composite() {
        let n = json!({
            "id": "1", "type": "GROUP", "name": "card",
            "children": [
                { "id": "2", "type": "RECTANGLE", "name": "img1",
                  "fills": [{ "type": "IMAGE", "imageRef": "x", "scaleMode": "FILL", "blendMode": "NORMAL" }] },
                { "id": "3", "type": "RECTANGLE", "name": "img2",
                  "fills": [{ "type": "IMAGE", "imageRef": "y", "scaleMode": "FILL", "blendMode": "NORMAL" }] }
            ]
        });
        assert_eq!(classify_node(&n), Some(AssetKind::Composite));
    }

    #[test]
    fn invisible_node_excluded_from_specs() {
        let frame = json!({
            "id": "F", "type": "FRAME", "name": "root", "children": [
                { "id": "1", "type": "VECTOR", "name": "shown" },
                { "id": "2", "type": "VECTOR", "name": "hidden", "visible": false }
            ]
        });
        let specs = classify(&frame);
        assert!(specs.iter().any(|s| s.node_id == "1"));
        assert!(!specs.iter().any(|s| s.node_id == "2"));
    }
}
