//! Aggregator: dump everything an agent needs to implement a frame in code.
//!
//! Composes the tree renderer, styles extractor, asset extractor, and
//! screenshot module to produce a directory tree:
//!
//!   <out>/tree.txt
//!   <out>/screenshot.png
//!   <out>/styles/tokens.json
//!   <out>/styles/tokens.css
//!   <out>/styles/tailwind.json
//!   <out>/assets/icons/*, images/*, manifest.json
//!   <out>/README.md          (a one-pager pointing at all of the above)

use std::path::Path;

use anyhow::{Context, Result};
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Value};

use crate::node::{bounds, name as node_name, type_str};
use crate::{assets, screenshot, styles, tree};

pub async fn build(
    cfg: &Configuration,
    file_key: &str,
    file_resp: &Value,
    frame: &Value,
    out_dir: &Path,
) -> Result<Value> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;
    std::fs::create_dir_all(out_dir.join("styles"))?;
    std::fs::create_dir_all(out_dir.join("assets"))?;

    // tree.txt
    let tree_text = tree::render(frame, usize::MAX);
    std::fs::write(out_dir.join("tree.txt"), &tree_text)?;

    // screenshot.png (scale=2 by default, good balance of size and clarity)
    let frame_id = crate::node::id(frame).context("frame has no id")?;
    let shot = screenshot::render_node(
        cfg,
        file_key,
        frame_id,
        2.0,
        screenshot::Format::Png,
    )
    .await?;
    std::fs::write(out_dir.join("screenshot.png"), &shot.bytes)?;

    // styles in all three formats.
    let mut tokens = styles::Tokens::default();
    styles::collect_from_target(frame, &mut tokens);
    styles::merge_published(file_resp, &mut tokens);
    let tokens_json = styles::render(&tokens, styles::Format::Tokens);
    let tokens_css = styles::render(&tokens, styles::Format::Css);
    let tokens_tw = styles::render(&tokens, styles::Format::Tailwind);
    std::fs::write(
        out_dir.join("styles/tokens.json"),
        serde_json::to_string_pretty(&tokens_json)?,
    )?;
    std::fs::write(
        out_dir.join("styles/tokens.css"),
        tokens_css.as_str().unwrap_or_default(),
    )?;
    std::fs::write(
        out_dir.join("styles/tailwind.json"),
        serde_json::to_string_pretty(&tokens_tw)?,
    )?;

    // assets/
    let manifest = assets::extract(cfg, file_key, frame, &out_dir.join("assets")).await?;

    // README.md — a brief pointer for whoever opens the directory.
    let readme = render_readme(frame, &manifest);
    std::fs::write(out_dir.join("README.md"), readme)?;

    Ok(json!({
        "out_dir": out_dir.display().to_string(),
        "frame": {
            "id": frame_id,
            "name": node_name(frame).unwrap_or(""),
            "type": type_str(frame).unwrap_or(""),
            "bounds": bounds(frame).map(|b| json!({
                "width": b.width, "height": b.height
            })),
        },
        "wrote": {
            "tree_txt_bytes": tree_text.len(),
            "screenshot_png_bytes": shot.bytes.len(),
            "icons": manifest.icons.len(),
            "images": manifest.images.len(),
            "composites": manifest.composites.len(),
            "failed": manifest.failed.len(),
        }
    }))
}

fn render_readme(frame: &Value, manifest: &assets::Manifest) -> String {
    let name = node_name(frame).unwrap_or("");
    let dim = bounds(frame)
        .map(|b| format!("{}×{}", b.width.round() as i64, b.height.round() as i64))
        .unwrap_or_else(|| "?".into());
    format!(
        "# {name} ({dim})\n\n\
         ## Contents\n\
         - `tree.txt` — full hierarchy (skipping invisible nodes)\n\
         - `screenshot.png` — 2x render of the frame\n\
         - `styles/tokens.json` — raw design tokens\n\
         - `styles/tokens.css` — CSS variables\n\
         - `styles/tailwind.json` — `theme.extend` shape for tailwind.config.js\n\
         - `assets/icons/` — {icons} SVG icons\n\
         - `assets/images/` — {images} PNG images\n\
         - `assets/images/composites/` — {composites} composite PNGs\n\
         {failed_block}",
        icons = manifest.icons.len(),
        images = manifest.images.len(),
        composites = manifest.composites.len(),
        failed_block = if manifest.failed.is_empty() {
            String::new()
        } else {
            format!(
                "\n## Failures\n{} asset(s) failed to export — see `assets/manifest.json`.\n",
                manifest.failed.len()
            )
        }
    )
}
