//! Render a node as a single image via Figma's `/images` endpoint, then
//! download the bytes. The endpoint returns short-lived S3 URLs (~30 min
//! lifetime); we fetch immediately and either write the bytes to disk or
//! return them in memory.

use anyhow::{anyhow, Context, Result};
use figma_api::apis::configuration::Configuration;
use figma_api::apis::files_api;

use crate::into_anyhow;

#[derive(Clone, Copy, Debug, clap::ValueEnum, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpg,
    Svg,
    Pdf,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Svg => "svg",
            Self::Pdf => "pdf",
        }
    }

    pub fn extension(self) -> &'static str {
        self.as_str()
    }
}

pub struct Rendered {
    pub bytes: Vec<u8>,
    pub source_url: String,
}

/// Request a render for `node_id`, then download the rendered bytes.
pub async fn render_node(
    cfg: &Configuration,
    file_key: &str,
    node_id: &str,
    scale: f64,
    format: Format,
) -> Result<Rendered> {
    let params = files_api::GetImagesParams {
        file_key: file_key.to_owned(),
        ids: node_id.to_owned(),
        version: None,
        scale: Some(scale),
        format: Some(format.as_str().to_owned()),
        svg_outline_text: None,
        svg_include_id: None,
        svg_include_node_id: None,
        svg_simplify_stroke: None,
        contents_only: None,
        use_absolute_bounds: None,
    };
    let resp = files_api::get_images(cfg, params)
        .await
        .map_err(into_anyhow)?;
    let url = resp.images.get(node_id).cloned().ok_or_else(|| {
        anyhow!(
            "Figma returned no image URL for node {} (the node may not be exportable)",
            node_id
        )
    })?;
    let bytes = reqwest::get(&url)
        .await
        .with_context(|| format!("downloading rendered image from {url}"))?
        .error_for_status()
        .context("rendered image URL returned a non-success status")?
        .bytes()
        .await
        .context("reading rendered image body")?
        .to_vec();
    Ok(Rendered {
        bytes,
        source_url: url,
    })
}

/// Max node ids per `/images` request. Figma silently truncates (or 414s) on
/// very large `ids` lists, so callers' nodes would go missing and surface as
/// "no render URL returned by Figma". We chunk and merge instead.
const IMAGE_BATCH: usize = 100;

/// Render multiple nodes. Returns a map of node_id → URL (caller is
/// responsible for downloading bytes). Requests are chunked at `IMAGE_BATCH`
/// and the per-chunk maps are merged.
pub async fn render_urls(
    cfg: &Configuration,
    file_key: &str,
    node_ids: &[String],
    scale: f64,
    format: Format,
) -> Result<std::collections::HashMap<String, String>> {
    if node_ids.is_empty() {
        return Ok(Default::default());
    }
    let mut out = std::collections::HashMap::new();
    for chunk in node_ids.chunks(IMAGE_BATCH) {
        let params = files_api::GetImagesParams {
            file_key: file_key.to_owned(),
            ids: chunk.join(","),
            version: None,
            scale: Some(scale),
            format: Some(format.as_str().to_owned()),
            svg_outline_text: None,
            svg_include_id: None,
            svg_include_node_id: None,
            svg_simplify_stroke: None,
            contents_only: None,
            use_absolute_bounds: None,
        };
        let resp = files_api::get_images(cfg, params)
            .await
            .map_err(into_anyhow)?;
        out.extend(resp.images);
    }
    Ok(out)
}
