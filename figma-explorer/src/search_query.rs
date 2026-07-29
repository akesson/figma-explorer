//! Web-search-style query grammar for `find`.
//!
//! The syntax mirrors what Google/GitHub/Slack search taught everyone (and
//! every LLM), so agents use it correctly without reading docs:
//!
//! - bare words        → fuzzy tokens, implicit AND (unchanged behavior)
//! - `"quoted phrase"` → exact case-insensitive *contiguous* substring
//! - `OR` (uppercase)  → alternation between the adjacent terms; binds
//!   tighter than the implicit AND (`a b OR c` = a AND (b OR c))
//! - `-term`, `-"…"`   → exclusion (substring, case-insensitive)
//! - `AND` (uppercase) → accepted as a no-op separator
//!
//! Lowercase `or`/`and` are ordinary fuzzy tokens. A `-` only negates at the
//! start of a term (`night-shift` stays one literal token).
//!
//! Note for agents: the shell eats the outer quotes, so exact phrases need
//! nesting — `find '"Approved by you" wallchart'`.

/// One positive or negative search term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    /// Bare word — fuzzy-matched (nucleo) against names and text content.
    Fuzzy(String),
    /// `"quoted"` — exact case-insensitive contiguous substring.
    Phrase(String),
}

impl Term {
    /// The raw term text, without quote decoration.
    pub fn text(&self) -> &str {
        match self {
            Term::Fuzzy(s) | Term::Phrase(s) => s,
        }
    }

    /// Display form for output attribution — phrases get their quotes back.
    pub fn display(&self) -> String {
        match self {
            Term::Fuzzy(s) => s.clone(),
            Term::Phrase(s) => format!("\"{s}\""),
        }
    }
}

/// One AND-clause: a set of OR-alternatives, at least one of which must
/// match. A plain term is a group with a single alternative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub alts: Vec<Term>,
}

/// Parsed query: `groups` are ANDed (each must match a distinct ancestor,
/// same constraint as the old token list); `excludes` reject a node when any
/// of them matches anywhere on its ancestor chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub groups: Vec<Group>,
    pub excludes: Vec<Term>,
}

/// Lexer unit: a (possibly negated) term, or the `OR` operator.
#[derive(Debug)]
enum Unit {
    Term { term: Term, negated: bool },
    Or,
}

impl Query {
    pub fn parse(input: &str) -> Result<Query, String> {
        let units = lex(input)?;
        let mut groups: Vec<Group> = Vec::new();
        let mut excludes: Vec<Term> = Vec::new();
        let mut pending_or = false;
        for unit in units {
            match unit {
                Unit::Or => {
                    if groups.is_empty() {
                        return Err("`OR` needs a positive term before it".to_owned());
                    }
                    if pending_or {
                        return Err("two `OR`s in a row".to_owned());
                    }
                    pending_or = true;
                }
                Unit::Term { term, negated } => {
                    if negated {
                        if pending_or {
                            return Err(format!("cannot `OR` a negated term (-{})", term.text()));
                        }
                        excludes.push(term);
                    } else if pending_or {
                        // `groups` non-empty — enforced when pending_or was set.
                        groups.last_mut().expect("non-empty").alts.push(term);
                        pending_or = false;
                    } else {
                        groups.push(Group { alts: vec![term] });
                    }
                }
            }
        }
        if pending_or {
            return Err("`OR` needs a term after it".to_owned());
        }
        if groups.is_empty() {
            return Err("query needs at least one positive term".to_owned());
        }
        Ok(Query { groups, excludes })
    }

    /// All positive term texts joined with spaces — what mark search and the
    /// comment-mention hint should see (operators and exclusions stripped).
    pub fn positive_text(&self) -> String {
        self.groups
            .iter()
            .flat_map(|g| g.alts.iter())
            .map(Term::text)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether the query uses any syntax beyond plain fuzzy tokens.
    pub fn is_plain(&self) -> bool {
        self.excludes.is_empty()
            && self
                .groups
                .iter()
                .all(|g| g.alts.len() == 1 && matches!(g.alts[0], Term::Fuzzy(_)))
    }
}

fn lex(input: &str) -> Result<Vec<Unit>, String> {
    let mut units = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        // `-` negates only when it prefixes a term (not a bare dash).
        let mut negated = false;
        if c == '-' {
            let mut ahead = chars.clone();
            ahead.next();
            if ahead.peek().is_some_and(|n| !n.is_whitespace()) {
                negated = true;
                chars.next();
            }
        }
        let c = match chars.peek() {
            Some(&c) => c,
            None => break,
        };
        if c == '"' {
            chars.next();
            let mut phrase = String::new();
            let mut closed = false;
            for ch in chars.by_ref() {
                if ch == '"' {
                    closed = true;
                    break;
                }
                phrase.push(ch);
            }
            if !closed {
                return Err(format!("unbalanced quote in query: {input}"));
            }
            if phrase.trim().is_empty() {
                return Err("empty phrase (\"\") in query".to_owned());
            }
            units.push(Unit::Term {
                term: Term::Phrase(phrase),
                negated,
            });
        } else {
            let mut word = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || ch == '"' {
                    break;
                }
                word.push(ch);
                chars.next();
            }
            match word.as_str() {
                // Operators are exact-uppercase and never negated (`-OR` is
                // a literal exclusion of the text "OR").
                "OR" if !negated => units.push(Unit::Or),
                "AND" if !negated => {} // implicit anyway — accepted, ignored
                _ => units.push(Unit::Term {
                    term: Term::Fuzzy(word),
                    negated,
                }),
            }
        }
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fuzzy(s: &str) -> Term {
        Term::Fuzzy(s.to_owned())
    }
    fn phrase(s: &str) -> Term {
        Term::Phrase(s.to_owned())
    }

    #[test]
    fn plain_tokens_parse_as_singleton_groups() {
        let q = Query::parse("wallchart grid button").unwrap();
        assert_eq!(q.groups.len(), 3);
        assert!(q.excludes.is_empty());
        assert!(q.is_plain());
        assert_eq!(q.groups[0].alts, vec![fuzzy("wallchart")]);
    }

    #[test]
    fn quoted_phrase_is_one_term() {
        let q = Query::parse("\"Approved by you\" wallchart").unwrap();
        assert_eq!(q.groups.len(), 2);
        assert_eq!(q.groups[0].alts, vec![phrase("Approved by you")]);
        assert_eq!(q.groups[1].alts, vec![fuzzy("wallchart")]);
        assert!(!q.is_plain());
    }

    #[test]
    fn or_binds_adjacent_terms_google_style() {
        // a b OR c d  =  a AND (b OR c) AND d
        let q = Query::parse("a b OR c d").unwrap();
        assert_eq!(q.groups.len(), 3);
        assert_eq!(q.groups[1].alts, vec![fuzzy("b"), fuzzy("c")]);
        // Chained: b OR c OR d is one group of three.
        let q = Query::parse("a OR b OR c").unwrap();
        assert_eq!(q.groups.len(), 1);
        assert_eq!(q.groups[0].alts.len(), 3);
    }

    #[test]
    fn minus_excludes_and_lowercase_or_is_literal() {
        let q = Query::parse("leave bar -mobile -\"For the future\"").unwrap();
        assert_eq!(q.groups.len(), 2);
        assert_eq!(q.excludes, vec![fuzzy("mobile"), phrase("For the future")]);
        // Lowercase `or` is just a token; hyphen mid-word stays literal.
        let q = Query::parse("night-shift or day").unwrap();
        assert_eq!(q.groups.len(), 3);
        assert_eq!(q.groups[1].alts, vec![fuzzy("or")]);
        assert!(q.excludes.is_empty());
    }

    #[test]
    fn and_is_accepted_as_noop() {
        let q = Query::parse("approved AND declined").unwrap();
        assert_eq!(q.groups.len(), 2);
    }

    #[test]
    fn positive_text_strips_operators_and_excludes() {
        let q = Query::parse("\"leave bar\" OR tooltip -mobile").unwrap();
        assert_eq!(q.positive_text(), "leave bar tooltip");
    }

    #[test]
    fn parse_errors_are_actionable() {
        for (input, needle) in [
            ("\"unbalanced", "unbalanced quote"),
            ("a OR OR b", "two `OR`s"),
            ("OR a", "before it"),
            ("a OR", "after it"),
            ("a OR -b", "negated term"),
            ("-only -negatives", "at least one positive"),
            ("\"\"", "empty phrase"),
        ] {
            let err = Query::parse(input).unwrap_err();
            assert!(
                err.contains(needle),
                "input {input:?}: expected {needle:?} in {err:?}"
            );
        }
    }

    #[test]
    fn bare_dash_is_a_literal_token() {
        let q = Query::parse("a - b").unwrap();
        assert_eq!(q.groups.len(), 3);
        assert_eq!(q.groups[1].alts, vec![fuzzy("-")]);
    }
}
