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
use serde_json::{json, Map, Value};

use crate::cache::project_to_cache;
use crate::node::{bounds, name as node_name, type_str};
use crate::{assets, screenshot, styles, tree};

pub async fn build(
    cfg: &Configuration,
    file_key: &str,
    file_synth: u32,
    file_resp: &Value,
    frame: &Value,
    out_dir: &Path,
) -> Result<Value> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;
    std::fs::create_dir_all(out_dir.join("styles"))?;
    std::fs::create_dir_all(out_dir.join("assets"))?;

    // tree.txt — flat pipe-rail format, qualified IDs. Matches `ls`/`find`
    // output so an agent can paste any row's first column into another
    // command. Projects the live Value subtree through the same shape the
    // cache uses, so the rendered output is byte-for-byte what `ls` would
    // produce for the same node.
    let projected = project_to_cache(frame);
    let tree_text = tree::render_flat(&projected, file_synth, usize::MAX).join("\n");
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

    let mut frame_obj = Map::new();
    frame_obj.insert("name".into(), json!(node_name(frame).unwrap_or("")));
    frame_obj.insert("type".into(), json!(type_str(frame).unwrap_or("")));
    if let Some(b) = bounds(frame) {
        frame_obj.insert("bounds".into(), json!(b.to_string()));
    }
    Ok(json!({
        "out_dir": out_dir.display().to_string(),
        "frame": Value::Object(frame_obj),
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
    let title = match bounds(frame) {
        Some(b) => format!("# {name} ({b})"),
        None => format!("# {name}"),
    };
    format!(
        "{title}\n\n\
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
