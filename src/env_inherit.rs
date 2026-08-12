//! Validation and matching for `--env-inherit` patterns.
//!
//! `--env-inherit` copies variables from the caller's environment into a freshly
//! spawned agent, which is often an untrusted worker. To bound what a single
//! pattern can sweep up, only two forms are allowed:
//!
//! * an exact variable name, e.g. `EDITOR`
//! * a trailing-wildcard namespace prefix, e.g. `RITE_*`
//!
//! A **leading** wildcard (`*_TOKEN`) is rejected on purpose. It selects
//! variables by role across every vendor namespace at once, which is the shape
//! of a credential-harvesting selector. A trailing wildcard forces the caller to
//! name a concrete namespace, so the blast radius stays inside that namespace.
//!
//! The grammar is deliberately narrow and enforced here, not in the docs:
//!
//! * at most one `*`, and only as the final character;
//! * the literal prefix must be a valid variable name that ends at an `_`
//!   boundary (so `AWS_*` cannot also match `AWSOME`);
//! * the prefix may not be empty or all underscores (`*`, `_*` are rejected).
//!
//! A malformed pattern is a hard error, never a silent skip: a typo that would
//! otherwise expand to nothing must surface, not vanish.
//!
//! A prefix is a scope limiter, not a secret filter. `RITE_*` cannot reach
//! `GITHUB_TOKEN`, but it still passes every `RITE_`-prefixed variable, secrets
//! included. The caller named the namespace, so this is intended; the audit line
//! the caller logs (names only) is how a swept-in secret stays visible.

use std::fmt;

/// Minimum length of the namespace (the wildcard prefix with trailing
/// underscores removed). Keeps a one-letter prefix such as `A_*` from matching a
/// whole letter's worth of variables under the guise of a concrete namespace.
const MIN_NAMESPACE_LEN: usize = 2;

/// A validated `--env-inherit` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvInheritSpec {
    /// Match exactly one variable by name.
    Exact(String),
    /// Match every variable whose name starts with this prefix.
    ///
    /// The prefix always ends with `_`, so `RITE_` matches `RITE_AGENT` but
    /// never the bare name `RITE`.
    Prefix(String),
}

impl EnvInheritSpec {
    /// Report whether `name` is selected by this spec. Case-sensitive, because
    /// environment variable names are case-sensitive on Unix.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(n) => name == n,
            Self::Prefix(prefix) => name.starts_with(prefix),
        }
    }
}

/// Why a `--env-inherit` pattern was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvInheritError {
    /// The pattern was the empty string.
    Empty,
    /// A `*` appeared at the start (`*_TOKEN`, `*`). This is the headline
    /// forbidden case: selecting variables by suffix across all namespaces.
    LeadingWildcard(String),
    /// A `*` appeared in the middle (`RITE_*_TOKEN`, `FO*O`). An internal
    /// wildcard smuggles suffix-selection back in.
    EmbeddedWildcard(String),
    /// More than one `*` (`RITE_*_*`).
    MultipleWildcards(String),
    /// The wildcard prefix does not end at an `_` boundary (`RITE*`).
    UnanchoredPrefix(String),
    /// The namespace (prefix without trailing `_`) is shorter than
    /// [`MIN_NAMESPACE_LEN`] (`_*`, `__*`, `A_*`).
    TrivialPrefix(String),
    /// The name or prefix contains a character outside `[A-Za-z0-9_]`, or
    /// starts with a digit.
    InvalidChar { pattern: String, ch: char },
}

impl fmt::Display for EnvInheritError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => {
                write!(f, "--env-inherit: empty pattern")
            }
            Self::LeadingWildcard(p) => write!(
                f,
                "--env-inherit '{p}': a leading wildcard is not allowed. A '*' may \
                 appear only as the last character of a namespace prefix (e.g. \
                 RITE_*). Suffix selection like *_TOKEN would match secrets across \
                 every namespace at once."
            ),
            Self::EmbeddedWildcard(p) => write!(
                f,
                "--env-inherit '{p}': '*' is allowed only as the final character \
                 (e.g. RITE_*), not inside the pattern."
            ),
            Self::MultipleWildcards(p) => write!(
                f,
                "--env-inherit '{p}': only a single trailing '*' is allowed."
            ),
            Self::UnanchoredPrefix(p) => {
                let prefix = p.trim_end_matches('*');
                write!(
                    f,
                    "--env-inherit '{p}': a wildcard prefix must end at an underscore \
                     boundary (e.g. {prefix}_*), so it cannot match unrelated names."
                )
            }
            Self::TrivialPrefix(p) => write!(
                f,
                "--env-inherit '{p}': the prefix before '*' is too broad. Name a \
                 concrete namespace of at least {MIN_NAMESPACE_LEN} characters, \
                 such as RITE_*."
            ),
            Self::InvalidChar { pattern, ch } => write!(
                f,
                "--env-inherit '{pattern}': invalid character '{ch}' in variable \
                 name. Use letters, digits, and underscores, and do not start with \
                 a digit."
            ),
        }
    }
}

impl std::error::Error for EnvInheritError {}

/// Validate one variable-name character run. Env var names are restricted to the
/// POSIX portable set `[A-Za-z_][A-Za-z0-9_]*`, which is what shells and callers
/// actually use and keeps the matcher unambiguous.
fn validate_name_chars(s: &str) -> Result<(), EnvInheritError> {
    for (i, ch) in s.char_indices() {
        let valid_char = ch == '_' || ch.is_ascii_alphanumeric();
        let leading_digit = i == 0 && ch.is_ascii_digit();
        if !valid_char || leading_digit {
            return Err(EnvInheritError::InvalidChar {
                pattern: s.to_string(),
                ch,
            });
        }
    }
    Ok(())
}

/// Parse and validate a single `--env-inherit` pattern.
pub fn parse_spec(pattern: &str) -> Result<EnvInheritSpec, EnvInheritError> {
    if pattern.is_empty() {
        return Err(EnvInheritError::Empty);
    }

    let star_count = pattern.matches('*').count();
    if star_count == 0 {
        validate_name_chars(pattern)?;
        return Ok(EnvInheritSpec::Exact(pattern.to_string()));
    }
    if star_count > 1 {
        return Err(EnvInheritError::MultipleWildcards(pattern.to_string()));
    }
    // Exactly one '*'. Reject it anywhere but the final position.
    if pattern.starts_with('*') {
        return Err(EnvInheritError::LeadingWildcard(pattern.to_string()));
    }
    if !pattern.ends_with('*') {
        return Err(EnvInheritError::EmbeddedWildcard(pattern.to_string()));
    }

    let prefix = &pattern[..pattern.len() - '*'.len_utf8()];
    validate_name_chars(prefix)?;
    // The namespace is the prefix without its trailing underscore(s). Require at
    // least two characters so a one-letter namespace like `A_*` (which would
    // match every `A_`-prefixed variable) cannot slip through as "anchored".
    let namespace = prefix.trim_end_matches('_');
    if namespace.len() < MIN_NAMESPACE_LEN {
        return Err(EnvInheritError::TrivialPrefix(pattern.to_string()));
    }
    if !prefix.ends_with('_') {
        return Err(EnvInheritError::UnanchoredPrefix(pattern.to_string()));
    }
    Ok(EnvInheritSpec::Prefix(prefix.to_string()))
}

/// Expand `patterns` against a snapshot of environment variables.
///
/// Every pattern is validated first; the first malformed pattern aborts the whole
/// call so a typo never silently expands to nothing. Matches are returned in the
/// order they appear in `env`, de-duplicated by name (first occurrence wins), so
/// the same variable is never passed twice even if two patterns select it.
///
/// Any name present in `already_set` is skipped. These are the keys the caller
/// already set explicitly (via `--env`), which must win. A child shell that sees
/// a key twice resolves it in an unspecified way (last-wins on glibc/dash), so the
/// only safe rule is to never let an inherited match reach the child alongside an
/// explicit value for the same key.
///
/// The caller owns logging. Log the matched *names* for auditability, never the
/// values.
pub fn resolve<S: std::hash::BuildHasher>(
    patterns: &[String],
    env: &[(String, String)],
    already_set: &std::collections::HashSet<&str, S>,
) -> Result<Vec<(String, String)>, EnvInheritError> {
    let specs = patterns
        .iter()
        .map(|p| parse_spec(p))
        .collect::<Result<Vec<_>, _>>()?;

    let mut seen = std::collections::HashSet::new();
    let mut matched = Vec::new();
    for (name, value) in env {
        let selected = specs.iter().any(|spec| spec.matches(name));
        if selected && !already_set.contains(name.as_str()) && seen.insert(name.as_str()) {
            matched.push((name.clone(), value.clone()));
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(String, String)> {
        [
            ("RITE_AGENT", "vessel-dev"),
            ("RITE_CHANNEL", "vessel"),
            ("RITE_TOKEN", "s3cret"),
            ("RITEOUS", "not-in-namespace"),
            ("EDITOR", "vim"),
            ("GITHUB_TOKEN", "gh-secret"),
            ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    // --- exact names ---

    #[test]
    fn exact_name_parses() {
        assert_eq!(
            parse_spec("EDITOR"),
            Ok(EnvInheritSpec::Exact("EDITOR".into()))
        );
    }

    #[test]
    fn exact_name_matches_only_itself() {
        let spec = parse_spec("RITE_AGENT").unwrap();
        assert!(spec.matches("RITE_AGENT"));
        assert!(!spec.matches("RITE_AGENTS"));
        assert!(!spec.matches("RITE_AGEN"));
    }

    #[test]
    fn empty_is_rejected() {
        assert_eq!(parse_spec(""), Err(EnvInheritError::Empty));
    }

    #[test]
    fn invalid_char_is_rejected() {
        assert!(matches!(
            parse_spec("FOO.BAR"),
            Err(EnvInheritError::InvalidChar { .. })
        ));
        assert!(matches!(
            parse_spec("FOO=BAR"),
            Err(EnvInheritError::InvalidChar { .. })
        ));
    }

    #[test]
    fn leading_digit_is_rejected() {
        assert!(matches!(
            parse_spec("1FOO"),
            Err(EnvInheritError::InvalidChar { .. })
        ));
    }

    // --- trailing wildcard (allowed) ---

    #[test]
    fn trailing_wildcard_parses_to_prefix() {
        assert_eq!(
            parse_spec("RITE_*"),
            Ok(EnvInheritSpec::Prefix("RITE_".into()))
        );
    }

    #[test]
    fn prefix_matches_namespace_but_not_bare_name() {
        let spec = parse_spec("RITE_*").unwrap();
        assert!(spec.matches("RITE_AGENT"));
        assert!(spec.matches("RITE_CHANNEL"));
        // The trailing underscore anchors the namespace: RITEOUS is excluded.
        assert!(!spec.matches("RITEOUS"));
        assert!(!spec.matches("RITE"));
    }

    #[test]
    fn multi_segment_prefix_is_allowed() {
        assert_eq!(
            parse_spec("MY_APP_"),
            Ok(EnvInheritSpec::Exact("MY_APP_".into()))
        );
        assert_eq!(
            parse_spec("MY_APP_*"),
            Ok(EnvInheritSpec::Prefix("MY_APP_".into()))
        );
    }

    // --- forbidden wildcard shapes ---

    #[test]
    fn leading_wildcard_is_rejected() {
        assert_eq!(
            parse_spec("*_TOKEN"),
            Err(EnvInheritError::LeadingWildcard("*_TOKEN".into()))
        );
        assert_eq!(
            parse_spec("*"),
            Err(EnvInheritError::LeadingWildcard("*".into()))
        );
    }

    #[test]
    fn embedded_wildcard_is_rejected() {
        // The sneaky one: looks prefix-anchored but still selects by _TOKEN role.
        assert_eq!(
            parse_spec("RITE_*_TOKEN"),
            Err(EnvInheritError::EmbeddedWildcard("RITE_*_TOKEN".into()))
        );
        assert_eq!(
            parse_spec("FO*O"),
            Err(EnvInheritError::EmbeddedWildcard("FO*O".into()))
        );
    }

    #[test]
    fn multiple_wildcards_are_rejected() {
        assert_eq!(
            parse_spec("RITE_*_*"),
            Err(EnvInheritError::MultipleWildcards("RITE_*_*".into()))
        );
    }

    #[test]
    fn unanchored_prefix_is_rejected() {
        // AWS* would also grab AWSOME; require the underscore boundary.
        assert_eq!(
            parse_spec("AWS*"),
            Err(EnvInheritError::UnanchoredPrefix("AWS*".into()))
        );
    }

    #[test]
    fn trivial_prefix_is_rejected() {
        assert_eq!(
            parse_spec("_*"),
            Err(EnvInheritError::TrivialPrefix("_*".into()))
        );
        assert_eq!(
            parse_spec("__*"),
            Err(EnvInheritError::TrivialPrefix("__*".into()))
        );
    }

    #[test]
    fn one_letter_namespace_is_rejected() {
        // A_* stays anchored but is too broad to count as a concrete namespace.
        assert_eq!(
            parse_spec("A_*"),
            Err(EnvInheritError::TrivialPrefix("A_*".into()))
        );
        // Two characters is the floor and is accepted.
        assert_eq!(parse_spec("AB_*"), Ok(EnvInheritSpec::Prefix("AB_".into())));
    }

    // --- resolve() end to end ---

    fn no_explicit() -> std::collections::HashSet<&'static str> {
        std::collections::HashSet::new()
    }

    #[test]
    fn resolve_expands_namespace_and_exact() {
        let matched = resolve(&["RITE_*".into(), "EDITOR".into()], &env(), &no_explicit()).unwrap();
        let names: Vec<&str> = matched.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["RITE_AGENT", "RITE_CHANNEL", "RITE_TOKEN", "EDITOR"]
        );
    }

    #[test]
    fn resolve_dedupes_overlapping_patterns() {
        let matched = resolve(
            &["RITE_*".into(), "RITE_AGENT".into()],
            &env(),
            &no_explicit(),
        )
        .unwrap();
        let count = matched.iter().filter(|(n, _)| n == "RITE_AGENT").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn resolve_aborts_on_any_malformed_pattern() {
        // A good pattern paired with a forbidden one fails the whole call.
        let result = resolve(&["RITE_*".into(), "*_TOKEN".into()], &env(), &no_explicit());
        assert!(matches!(result, Err(EnvInheritError::LeadingWildcard(_))));
    }

    #[test]
    fn resolve_does_not_reach_cross_namespace_secrets() {
        // The point of the feature: RITE_* must not pull GITHUB_TOKEN etc.
        let matched = resolve(&["RITE_*".into()], &env(), &no_explicit()).unwrap();
        let names: Vec<&str> = matched.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!names.contains(&"GITHUB_TOKEN"));
        assert!(!names.contains(&"AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn resolve_skips_names_already_set_explicitly() {
        // An explicit `--env RITE_AGENT=...` must win over an inherited RITE_*
        // match, so the inherited value is dropped and never reaches the child.
        let explicit: std::collections::HashSet<&str> = ["RITE_AGENT"].into_iter().collect();
        let matched = resolve(&["RITE_*".into()], &env(), &explicit).unwrap();
        let names: Vec<&str> = matched.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!names.contains(&"RITE_AGENT"));
        assert!(names.contains(&"RITE_CHANNEL"));
    }
}
