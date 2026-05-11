use clap::Args;
use figma_api::apis::configuration::Configuration;
use figma_api::apis::files_api as api;

use crate::finalize;

#[derive(Args, Debug)]
pub struct FileArgs {
    /// File key or branch key.
    #[arg(long)]
    pub file_key: String,
    /// Specific version ID. Defaults to the current version.
    #[arg(long)]
    pub version: Option<String>,
    /// Comma-separated node IDs to include in the response.
    #[arg(long)]
    pub ids: Option<String>,
    /// Tree depth (positive integer). Returns all nodes if omitted.
    #[arg(long)]
    pub depth: Option<f64>,
    /// Set to "paths" to export vector data.
    #[arg(long)]
    pub geometry: Option<String>,
    /// Comma-separated plugin IDs (or "shared") to include plugin data for.
    #[arg(long)]
    pub plugin_data: Option<String>,
    /// Include branch metadata in the response.
    #[arg(long)]
    pub branch_data: Option<bool>,
}

impl FileArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFileParams {
            file_key: self.file_key,
            version: self.version,
            ids: self.ids,
            depth: self.depth,
            geometry: self.geometry,
            plugin_data: self.plugin_data,
            branch_data: self.branch_data,
        };
        finalize(api::get_file(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct FileMetaArgs {
    /// File key or branch key.
    #[arg(long)]
    pub file_key: String,
}

impl FileMetaArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFileMetaParams {
            file_key: self.file_key,
        };
        finalize(api::get_file_meta(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct FileNodesArgs {
    /// File key or branch key.
    #[arg(long)]
    pub file_key: String,
    /// Comma-separated node IDs to retrieve.
    #[arg(long)]
    pub ids: String,
    /// Specific version ID. Defaults to the current version.
    #[arg(long)]
    pub version: Option<String>,
    /// Tree depth (positive integer), starting from each desired node.
    #[arg(long)]
    pub depth: Option<f64>,
    /// Set to "paths" to export vector data.
    #[arg(long)]
    pub geometry: Option<String>,
    /// Comma-separated plugin IDs (or "shared") to include plugin data for.
    #[arg(long)]
    pub plugin_data: Option<String>,
}

impl FileNodesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFileNodesParams {
            file_key: self.file_key,
            ids: self.ids,
            version: self.version,
            depth: self.depth,
            geometry: self.geometry,
            plugin_data: self.plugin_data,
        };
        finalize(api::get_file_nodes(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct FileVersionsArgs {
    /// File key or branch key.
    #[arg(long)]
    pub file_key: String,
    /// Page size. Defaults to 30.
    #[arg(long)]
    pub page_size: Option<f64>,
    /// Cursor for versions before this ID.
    #[arg(long)]
    pub before: Option<f64>,
    /// Cursor for versions after this ID.
    #[arg(long)]
    pub after: Option<f64>,
}

impl FileVersionsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetFileVersionsParams {
            file_key: self.file_key,
            page_size: self.page_size,
            before: self.before,
            after: self.after,
        };
        finalize(api::get_file_versions(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct ImageFillsArgs {
    /// File key or branch key.
    #[arg(long)]
    pub file_key: String,
}

impl ImageFillsArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetImageFillsParams {
            file_key: self.file_key,
        };
        finalize(api::get_image_fills(cfg, params).await)
    }
}

#[derive(Args, Debug)]
pub struct ImagesArgs {
    /// File key or branch key.
    #[arg(long)]
    pub file_key: String,
    /// Comma-separated node IDs to render.
    #[arg(long)]
    pub ids: String,
    /// Specific version ID.
    #[arg(long)]
    pub version: Option<String>,
    /// Image scaling factor between 0.01 and 4.
    #[arg(long)]
    pub scale: Option<f64>,
    /// Image format: jpg, png, svg, or pdf.
    #[arg(long = "img-format")]
    pub img_format: Option<String>,
    /// Render text as vector outlines in SVG output.
    #[arg(long)]
    pub svg_outline_text: Option<bool>,
    /// Include id attributes on all SVG elements.
    #[arg(long)]
    pub svg_include_id: Option<bool>,
    /// Include node-id data attributes on SVG elements.
    #[arg(long)]
    pub svg_include_node_id: Option<bool>,
    /// Simplify inside/outside strokes in SVG.
    #[arg(long)]
    pub svg_simplify_stroke: Option<bool>,
    /// Exclude content overlapping the node from rendering.
    #[arg(long)]
    pub contents_only: Option<bool>,
    /// Use full node dimensions regardless of cropping.
    #[arg(long)]
    pub use_absolute_bounds: Option<bool>,
}

impl ImagesArgs {
    pub async fn run(self, cfg: &Configuration) -> anyhow::Result<serde_json::Value> {
        let params = api::GetImagesParams {
            file_key: self.file_key,
            ids: self.ids,
            version: self.version,
            scale: self.scale,
            format: self.img_format,
            svg_outline_text: self.svg_outline_text,
            svg_include_id: self.svg_include_id,
            svg_include_node_id: self.svg_include_node_id,
            svg_simplify_stroke: self.svg_simplify_stroke,
            contents_only: self.contents_only,
            use_absolute_bounds: self.use_absolute_bounds,
        };
        finalize(api::get_images(cfg, params).await)
    }
}
