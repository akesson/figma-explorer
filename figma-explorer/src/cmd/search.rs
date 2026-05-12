use anyhow::Result;
use clap::Args as ClapArgs;
use figma_api::apis::configuration::Configuration;
use serde_json::{json, Map, Value};

use crate::cache::{self, CacheNode};
use crate::cmd::LocatorArgs;
use crate::resolve;
use crate::{print, Output};

/// Locate nodes by a multi-token ancestor-chain hint.
///
/// Unlike `find` (single-token nucleo against names), `search` treats the
/// query as a sequence of tokens and ranks candidates by how the tokens line
/// up along the root → node path. A query like
/// `wallchart grid filter button` finds nodes whose ancestry includes
/// "wallchart", "grid", "filter", and "button" (anywhere in the chain), with
/// the leaf token weighted most heavily and a bonus for tokens that hit
/// consecutive path positions.
#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(flatten)]
    pub locator: LocatorArgs,

    /// Query phrase (one or more words). Each whitespace-separated token must
    /// fuzzy-match some ancestor name for a node to be a hit.
    #[arg(required = true, num_args = 1..)]
    pub query: Vec<String>,

    /// Comma-separated node types to keep (e.g. `FRAME,INSTANCE`). Default:
    /// all types.
    #[arg(long, value_delimiter = ',')]
    pub r#type: Vec<String>,

    /// Page name to scope the search to. Mutually compatible with --in.
    #[arg(long)]
    pub page: Option<String>,

    /// Node id whose subtree the search should be limited to.
    #[arg(long = "in")]
    pub in_node: Option<String>,

    /// Maximum number of hits to report.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
}

impl Args {
    pub async fn run(self, cfg: &Configuration, format: Output) -> Result<()> {
        let (file_key, url_node_id) = self.locator.resolve()?;
        let file = cache::load_file(cfg, &file_key).await?;

        let root: &CacheNode = if let Some(nid) = self
            .in_node
            .as_deref()
            .or(url_node_id.as_deref())
            .or(self.locator.node_id.as_deref())
        {
            resolve::resolve_node_id_cache(&file.document, nid)
                .ok_or_else(|| anyhow::anyhow!("no node with id {nid}"))?
        } else if let Some(page_query) = self.page.as_deref() {
            resolve::resolve_page_cache(&file.document, page_query)
                .ok_or_else(|| anyhow::anyhow!("no page matching {page_query:?}"))?
        } else {
            &file.document
        };

        // Pre-split the query for nice display + per-token matching.
        let joined = self.query.join(" ");
        let tokens: Vec<&str> = joined.split_whitespace().collect();
        if tokens.is_empty() {
            anyhow::bail!("query is empty");
        }

        let type_refs: Vec<&str> = self.r#type.iter().map(String::as_str).collect();
        let type_filter = if type_refs.is_empty() {
            None
        } else {
            Some(type_refs.as_slice())
        };

        let hits = resolve::multi_token_search(root, &tokens, type_filter, self.limit);

        let value = match format {
            Output::Yaml => {
                let lines: Vec<Value> = hits
                    .iter()
                    .map(|h| {
                        let path = path_string(&h.path);
                        let matches: Vec<String> = h
                            .matches
                            .iter()
                            .map(|m| format!("{}→{}", m.token, m.matched_name))
                            .collect();
                        let mut obj = Map::new();
                        obj.insert("id".into(), json!(h.node.id));
                        obj.insert("type".into(), json!(h.node.type_));
                        // Score is f64; round for compact YAML display.
                        obj.insert("score".into(), json!((h.score * 10.0).round() / 10.0));
                        obj.insert("path".into(), json!(path));
                        obj.insert("matches".into(), json!(matches));
                        Value::Object(obj)
                    })
                    .collect();
                json!(lines)
            }
            Output::Json => json!({
                "file_key": file_key,
                "query": joined,
                "tokens": tokens,
                "hits": hits.iter().map(|h| json!({
                    "node_id": h.node.id,
                    "name": h.node.name,
                    "type": h.node.type_,
                    "score": h.score,
                    "path": h.path.iter().map(|n| json!({"id": n.id, "name": n.name})).collect::<Vec<_>>(),
                    "matches": h.matches,
                })).collect::<Vec<_>>(),
            }),
        };
        print(&value, format)
    }
}

fn path_string(path: &[&CacheNode]) -> String {
    path.iter()
        .map(|n| n.name.as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" > ")
}
