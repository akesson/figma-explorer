//! Tagged IDs — the one positional argument shape across every command.
//!
//! Grammar (see cli-sketch.html for the full UX rationale):
//!
//! ```text
//! ID    ::= TAG ":" REST | NODE | URL
//! TAG   ::= "proj" | "file"                   // active
//!         | "user" | "var"                    // reserved (future)
//!         | "style" | "comp"
//! REST  ::= NUM                               // e.g. file:2          → a file
//!         | NUM ":" NODE                      // e.g. file:2:1094:66591 → a node in a file
//!         | NUM ":" "comm" ":" NUM            // e.g. file:2:comm:34   → a comment in a file
//! NODE  ::= ["I"] SEG (";" SEG)*              // native Figma node id, e.g. 1094:66591
//!                                             // or I880:3606;2816:36646 (instance descendant)
//! SEG   ::= NUM ":" NUM
//! NUM   ::= [0-9]+
//! URL   ::= "https://" .* figma.com .*
//! ```
//!
//! A bare `NODE` is treated as a native Figma node ID — resolution is
//! delegated to the cache layer (lenient lookup; ambiguous IDs error with
//! a candidate list). Instance-descendant ids (the `I…;…` form) appear
//! verbatim in `ls`/`node-info` output, so everything the CLI prints
//! round-trips back through this parser.

use std::fmt;
use std::str::FromStr;

use crate::url::{self, ParsedUrl};

/// One resolved-or-resolvable reference into the Figma world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id {
    /// `proj:N` — a project by synthetic index.
    Project(u32),
    /// `file:N` — a file by synthetic index.
    File(u32),
    /// `file:N:x:y` — a node inside a file, both parts known. The node part
    /// is any native Figma node id, including the instance-descendant form
    /// `I880:3606;2816:36646`.
    Node { file: u32, node: String },
    /// `file:N:comm:M` — a comment in a file, both synth indexes known. The
    /// parser accepts this form so paste-ready IDs round-trip; the resolver
    /// currently bails ("not resolvable yet") because no command consumes
    /// comment scope yet.
    Comment { file: u32, comm: u32 },
    /// `x:y` (or `I…;…`) — a native Figma node id with no file scope. Caller
    /// (Resolver) must look it up against the cache's node index. Multiple
    /// matches → error.
    BareNode(String),
    /// Full Figma URL (file_key + optional node-id parsed out).
    Url(ParsedUrl),
}

/// Tags we accept but haven't implemented. Parsing returns
/// [`IdParseError::ReservedTag`] so the user gets a clear error rather than a
/// silent "not found".
const RESERVED_TAGS: &[&str] = &["user", "var", "style", "comp"];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdParseError {
    #[error("empty id")]
    Empty,
    #[error("unknown tag {tag:?}; expected proj or file (or a numeric node id)")]
    UnknownTag { tag: String },
    #[error("tag {tag:?} is reserved for future use, not implemented yet")]
    ReservedTag { tag: String },
    #[error("malformed id {input:?}: {reason}")]
    Malformed { input: String, reason: String },
    #[error("url parse failed: {0}")]
    Url(String),
}

impl FromStr for Id {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(IdParseError::Empty);
        }

        // URL form takes priority — the rest of the grammar is colon-based,
        // and URLs contain colons too.
        if s.starts_with("https://") || s.starts_with("http://") {
            let parsed = url::parse(s).map_err(|e| IdParseError::Url(format!("{e:#}")))?;
            return Ok(Id::Url(parsed));
        }

        let (head, rest) = s.split_once(':').ok_or_else(|| IdParseError::Malformed {
            input: s.to_owned(),
            reason: "expected a colon".into(),
        })?;
        if head.is_empty() {
            return Err(IdParseError::Malformed {
                input: s.to_owned(),
                reason: "missing tag or leading number before ':'".into(),
            });
        }

        // Node-like head → bare native node id, nothing more allowed. A head
        // is node-like when it is all digits (`1094:…`) or an `I` followed by
        // digits (`I880:…`, the instance-descendant form) — anything else
        // falls through to the tagged lane so unknown tags keep their error.
        let head_is_node_like = head.chars().all(|c| c.is_ascii_digit())
            || (head.len() > 1
                && head.starts_with('I')
                && head[1..].chars().all(|c| c.is_ascii_digit()));
        if head_is_node_like {
            if !is_native_node_id(s) {
                return Err(IdParseError::Malformed {
                    input: s.to_owned(),
                    reason: "bare node id must be NUM:NUM, optionally I-prefixed with \
                             ;-separated NUM:NUM segments (e.g. 1:2 or I880:3606;2816:36646)"
                        .into(),
                });
            }
            return Ok(Id::BareNode(s.to_owned()));
        }

        // Tagged form.
        if RESERVED_TAGS.contains(&head) {
            return Err(IdParseError::ReservedTag {
                tag: head.to_owned(),
            });
        }
        match head {
            "proj" => parse_num(rest, s).map(Id::Project),
            "file" => parse_file_rest(rest, s),
            other => Err(IdParseError::UnknownTag {
                tag: other.to_owned(),
            }),
        }
    }
}

/// Shape-check a native Figma node id: `NUM:NUM`, optionally `I`-prefixed
/// with one or more `;`-separated `NUM:NUM` segments (the instance-descendant
/// form, e.g. `I880:3606;2816:36646`). Figma only ever emits a single plain
/// segment or `I` + two-or-more segments, but we accept the permissive union
/// (`I1:2`, `1:2;3:4`) — downstream lookups are exact-string, so a shape
/// Figma never mints just resolves to a clean not-found. Digits are not
/// range-checked: node ids can exceed u32.
fn is_native_node_id(s: &str) -> bool {
    let body = s.strip_prefix('I').unwrap_or(s);
    !body.is_empty()
        && body.split(';').all(|seg| {
            seg.split_once(':').is_some_and(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && b.chars().all(|c| c.is_ascii_digit())
            })
        })
}

fn parse_num(s: &str, full: &str) -> Result<u32, IdParseError> {
    s.parse::<u32>().map_err(|_| IdParseError::Malformed {
        input: full.to_owned(),
        reason: format!("expected unsigned integer, got {s:?}"),
    })
}

/// Parse what comes after `file:`. Three accepted shapes:
/// - `<N>` (file alone) → `Id::File`
/// - `<N>:x:y` (node-in-file, where x:y is the native two-part node id)
/// - `<N>:comm:<M>` (comment-in-file, both synth indexes)
fn parse_file_rest(rest: &str, full: &str) -> Result<Id, IdParseError> {
    // file:N — no inner colon.
    if !rest.contains(':') {
        return parse_num(rest, full).map(Id::File);
    }

    let (file_part, tail) = rest.split_once(':').expect("contains ':' just checked");
    let file = parse_num(file_part, full)?;

    // file:N:comm:M — comment scope. Recognized before the node path so the
    // literal `comm` segment isn't misparsed as a node-id digit.
    if let Some(comm_rest) = tail.strip_prefix("comm:") {
        if comm_rest.is_empty() || !comm_rest.chars().all(|c| c.is_ascii_digit()) {
            return Err(IdParseError::Malformed {
                input: full.to_owned(),
                reason: format!("comment synth must be a positive integer, got {comm_rest:?}"),
            });
        }
        let comm = parse_num(comm_rest, full)?;
        return Ok(Id::Comment { file, comm });
    }
    // Reject ambiguous half-formed comment ids like `file:2:comm` (no `:M`).
    if tail == "comm" {
        return Err(IdParseError::Malformed {
            input: full.to_owned(),
            reason: "comment id must be file:N:comm:M".into(),
        });
    }

    // file:N:<node> — split off the leading file synth, the remainder must be
    // a native Figma node id (plain or instance-descendant form).
    if !is_native_node_id(tail) {
        return Err(IdParseError::Malformed {
            input: full.to_owned(),
            reason: format!(
                "node part {tail:?} is not a native Figma node id (NUM:NUM, or I…;… instance form)"
            ),
        });
    }

    Ok(Id::Node {
        file,
        node: tail.to_owned(),
    })
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Project(n) => write!(f, "proj:{n}"),
            Id::File(n) => write!(f, "file:{n}"),
            Id::Node { file, node } => write!(f, "file:{file}:{node}"),
            Id::Comment { file, comm } => write!(f, "file:{file}:comm:{comm}"),
            Id::BareNode(s) => write!(f, "{s}"),
            Id::Url(p) => {
                // Display URLs as their canonical tagged form when we can —
                // a Url is just an alias for a file or node we haven't synthed
                // yet, but at Display time we don't have a SynthState to consult,
                // so emit the raw key:node form prefixed so it can't be mistaken
                // for a tagged id. Use a sentinel that's clearly a URL stand-in.
                if let Some(node) = &p.node_id {
                    write!(f, "url:{}:{}", p.file_key, node)
                } else {
                    write!(f, "url:{}", p.file_key)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Id, IdParseError> {
        s.parse()
    }

    #[test]
    fn parses_project() {
        assert_eq!(parse("proj:1").unwrap(), Id::Project(1));
        assert_eq!(parse("proj:42").unwrap(), Id::Project(42));
    }

    #[test]
    fn parses_file() {
        assert_eq!(parse("file:2").unwrap(), Id::File(2));
    }

    #[test]
    fn parses_qualified_node() {
        assert_eq!(
            parse("file:2:1094:66591").unwrap(),
            Id::Node {
                file: 2,
                node: "1094:66591".into()
            }
        );
    }

    #[test]
    fn parses_bare_node() {
        assert_eq!(
            parse("1094:66591").unwrap(),
            Id::BareNode("1094:66591".into())
        );
        assert_eq!(parse("0:0").unwrap(), Id::BareNode("0:0".into()));
    }

    #[test]
    fn parses_bare_instance_node() {
        assert_eq!(
            parse("I880:3606;2816:36646").unwrap(),
            Id::BareNode("I880:3606;2816:36646".into())
        );
        // Three segments (doubly-nested instance descendant).
        assert_eq!(
            parse("I880:3608;805:95894;786:9275").unwrap(),
            Id::BareNode("I880:3608;805:95894;786:9275".into())
        );
        // Permissive union: shapes Figma never mints still parse and fail
        // later as a clean not-found.
        assert_eq!(parse("I1:2").unwrap(), Id::BareNode("I1:2".into()));
        assert_eq!(parse("1:2;3:4").unwrap(), Id::BareNode("1:2;3:4".into()));
    }

    #[test]
    fn parses_qualified_instance_node() {
        assert_eq!(
            parse("file:7:I880:3606;2816:36646").unwrap(),
            Id::Node {
                file: 7,
                node: "I880:3606;2816:36646".into()
            }
        );
    }

    #[test]
    fn instance_lookalikes_keep_erroring() {
        // Non-digit after `I` is not node-like → unknown-tag lane.
        match parse("Ifoo:bar") {
            Err(IdParseError::UnknownTag { tag }) => assert_eq!(tag, "Ifoo"),
            other => panic!("expected UnknownTag, got {other:?}"),
        }
        // A lone `I` head is not node-like either.
        match parse("I:2") {
            Err(IdParseError::UnknownTag { tag }) => assert_eq!(tag, "I"),
            other => panic!("expected UnknownTag, got {other:?}"),
        }
        // Node-like heads with malformed tails stay Malformed.
        assert!(matches!(parse("I880"), Err(IdParseError::Malformed { .. })));
        assert!(matches!(
            parse("I880:"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse("I880:3606;"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse("file:2:I880"),
            Err(IdParseError::Malformed { .. })
        ));
        // Extra colon inside a segment is not a valid node id.
        assert!(matches!(
            parse("1:2:3"),
            Err(IdParseError::Malformed { .. })
        ));
    }

    #[test]
    fn parses_comment() {
        assert_eq!(
            parse("file:2:comm:34").unwrap(),
            Id::Comment { file: 2, comm: 34 }
        );
        assert_eq!(
            parse("file:99:comm:1").unwrap(),
            Id::Comment { file: 99, comm: 1 }
        );
    }

    #[test]
    fn malformed_comment_ids_error() {
        // No comm synth at all.
        assert!(matches!(
            parse("file:2:comm"),
            Err(IdParseError::Malformed { .. })
        ));
        // Empty / non-numeric synth.
        assert!(matches!(
            parse("file:2:comm:"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse("file:2:comm:abc"),
            Err(IdParseError::Malformed { .. })
        ));
    }

    #[test]
    fn parses_url() {
        let parsed = parse("https://www.figma.com/design/AbCdEf123/Foo?node-id=5-12").unwrap();
        match parsed {
            Id::Url(p) => {
                assert_eq!(p.file_key, "AbCdEf123");
                assert_eq!(p.node_id.as_deref(), Some("5:12"));
            }
            other => panic!("expected Url, got {other:?}"),
        }
    }

    #[test]
    fn parses_url_with_instance_node() {
        let parsed =
            parse("https://www.figma.com/design/AbCdEf123/Foo?node-id=I880-3606%3B2816-36646")
                .unwrap();
        match parsed {
            Id::Url(p) => {
                assert_eq!(p.file_key, "AbCdEf123");
                assert_eq!(p.node_id.as_deref(), Some("I880:3606;2816:36646"));
            }
            other => panic!("expected Url, got {other:?}"),
        }
    }

    #[test]
    fn reserved_tags_error_with_clear_message() {
        // Note: `comm` is no longer reserved — it's a real variant under
        // `file:N:comm:M`. Bare `comm:N` (without a file scope) hits the
        // unknown-tag path instead.
        for tag in ["user", "var", "style", "comp"] {
            let s = format!("{tag}:1");
            match parse(&s) {
                Err(IdParseError::ReservedTag { tag: got }) => assert_eq!(got, tag),
                other => panic!("expected ReservedTag for {s}, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_tag_errors() {
        match parse("xyz:1") {
            Err(IdParseError::UnknownTag { tag }) => assert_eq!(tag, "xyz"),
            other => panic!("expected UnknownTag, got {other:?}"),
        }
    }

    #[test]
    fn malformed_inputs_error() {
        assert!(matches!(parse(""), Err(IdParseError::Empty)));
        assert!(matches!(
            parse("nocolon"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse("proj:abc"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse("file:2:nope"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse("file:2:1094"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse("file:2:1094:"),
            Err(IdParseError::Malformed { .. })
        ));
        assert!(matches!(parse(":1"), Err(IdParseError::Malformed { .. })));
        assert!(matches!(parse("1:"), Err(IdParseError::Malformed { .. })));
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(parse("  file:2  ").unwrap(), Id::File(2));
    }

    #[test]
    fn display_roundtrips_tagged_forms() {
        for input in [
            "proj:1",
            "file:2",
            "file:2:1094:66591",
            "file:2:comm:34",
            "1094:66591",
            "0:0",
            "I880:3606;2816:36646",
            "file:7:I880:3606;2816:36646",
        ] {
            let parsed: Id = input.parse().unwrap();
            assert_eq!(parsed.to_string(), input, "roundtrip failed for {input}");
        }
    }

    #[test]
    fn url_display_uses_url_prefix() {
        let id: Id = "https://www.figma.com/design/AbCdEf123/Foo?node-id=5-12"
            .parse()
            .unwrap();
        assert_eq!(id.to_string(), "url:AbCdEf123:5:12");
    }

    #[test]
    fn url_without_node_id() {
        let id: Id = "https://www.figma.com/design/AbCdEf123/Foo"
            .parse()
            .unwrap();
        assert_eq!(id.to_string(), "url:AbCdEf123");
    }
}
