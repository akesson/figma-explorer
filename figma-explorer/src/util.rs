//! Small cross-module helpers that don't belong to any one subsystem.

/// Turn an arbitrary display name into a filesystem-/token-safe slug:
/// lowercase, every run of non-alphanumeric characters collapsed to a single
/// `-`, trailing dashes trimmed. An empty result falls back to `fallback`.
///
/// Only `char::is_alphanumeric` characters survive, so path separators (`/`,
/// `\`) and dots are stripped — callers that build on-disk paths from
/// Figma-supplied names rely on this to prevent traversal (e.g.
/// `../../etc/passwd` slugifies to `etc-passwd`).
pub fn slugify(s: &str, fallback: &str) -> String {
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
        out.push_str(fallback);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_handles_special_chars() {
        assert_eq!(slugify("Primary Button", "x"), "primary-button");
        assert_eq!(slugify("Color/Brand/Red", "x"), "color-brand-red");
    }

    #[test]
    fn slugify_falls_back_when_empty() {
        assert_eq!(slugify("", "asset"), "asset");
        assert_eq!(slugify("///", "x"), "x");
    }

    #[test]
    fn slugify_strips_path_separators_no_traversal() {
        // The safety invariant the asset writer depends on.
        let s = slugify("../../../etc/passwd", "asset");
        assert!(!s.contains('/') && !s.contains('\\') && !s.contains(".."));
        assert_eq!(s, "etc-passwd");
    }
}
