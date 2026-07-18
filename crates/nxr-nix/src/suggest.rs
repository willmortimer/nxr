//! Rank discovered app names for unknown-app hints.

use nxr_core::App;

/// Maximum number of names returned for a not-found hint.
pub const DEFAULT_SUGGESTION_LIMIT: usize = 5;

/// Rank `query` against discovered apps using prefix match and edit distance.
///
/// Prefix matches (case-insensitive) rank first, then substring matches, then
/// names within a length-aware Levenshtein threshold.
#[must_use]
pub fn rank_app_suggestions<'a>(query: &str, apps: &'a [App], limit: usize) -> Vec<&'a str> {
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }

    let query_lower = query.to_ascii_lowercase();

    let mut scored: Vec<(u32, &str)> = apps
        .iter()
        .map(|app| {
            let name = app.name.as_str();
            let name_lower = name.to_ascii_lowercase();
            let score = if name_lower.starts_with(&query_lower) {
                0
            } else if name_lower.contains(&query_lower) {
                1
            } else {
                let distance = levenshtein(&query_lower, &name_lower);
                let threshold = suggestion_distance_threshold(query.len(), name.len());
                if distance <= threshold {
                    2 + distance
                } else {
                    u32::MAX
                }
            };
            (score, name)
        })
        .filter(|(score, _)| *score != u32::MAX)
        .collect();

    scored.sort_by_key(|(score, name)| (*score, *name));

    let mut names = Vec::with_capacity(limit.min(scored.len()));
    for (_, name) in scored {
        if names.len() >= limit {
            break;
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }

    names
}

fn suggestion_distance_threshold(query_len: usize, name_len: usize) -> u32 {
    let longest = query_len.max(name_len).max(1);
    u32::try_from(longest / 2 + 1).unwrap_or(u32::MAX)
}

fn levenshtein(left: &str, right: &str) -> u32 {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    let left_len = left_chars.len();
    let right_len = right_chars.len();

    if left_len == 0 {
        return u32::try_from(right_len).unwrap_or(u32::MAX);
    }
    if right_len == 0 {
        return u32::try_from(left_len).unwrap_or(u32::MAX);
    }

    let mut previous: Vec<u32> = (0..=right_len)
        .map(|index| u32::try_from(index).unwrap_or(u32::MAX))
        .collect();
    let mut current = vec![0; right_len + 1];

    for (row, left_char) in left_chars.iter().enumerate() {
        current[0] = u32::try_from(row + 1).unwrap_or(u32::MAX);
        for (col, right_char) in right_chars.iter().enumerate() {
            let deletion = previous[col + 1] + 1;
            let insertion = current[col] + 1;
            let substitution = previous[col] + u32::from(left_char != right_char);
            current[col + 1] = deletion.min(insertion).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right_len]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{DEFAULT_SUGGESTION_LIMIT, rank_app_suggestions};
    use nxr_core::App;

    fn apps(names: &[&str]) -> Vec<App> {
        names
            .iter()
            .map(|name| App {
                name: (*name).to_owned(),
                attr_path: format!("apps.aarch64-darwin.{name}"),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: None,
                is_default: false,
                metadata: BTreeMap::new(),
            })
            .collect()
    }

    #[test]
    fn prefix_match_ranks_before_edit_distance() {
        let discovered = apps(&["hello", "help", "helium"]);
        let suggestions = rank_app_suggestions("he", &discovered, DEFAULT_SUGGESTION_LIMIT);
        assert_eq!(suggestions, vec!["helium", "hello", "help"]);
    }

    #[test]
    fn edit_distance_suggests_close_typo() {
        let discovered = apps(&["default", "echo-args", "fail", "hello", "pwd", "succeed"]);
        let suggestions = rank_app_suggestions("helo", &discovered, DEFAULT_SUGGESTION_LIMIT);
        assert_eq!(suggestions.first().copied(), Some("hello"));
    }

    #[test]
    fn unrelated_query_returns_no_suggestions() {
        let discovered = apps(&["hello", "pwd"]);
        let suggestions = rank_app_suggestions("zzzzzz", &discovered, DEFAULT_SUGGESTION_LIMIT);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn suggestions_are_limited_and_sorted() {
        let discovered = apps(&["alpha", "beta", "delta", "gamma"]);
        let suggestions = rank_app_suggestions("a", &discovered, 2);
        assert_eq!(suggestions, vec!["alpha", "beta"]);
    }
}
