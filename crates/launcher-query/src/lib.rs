//! Allocation-free matching shared by Launcher catalog and application results.
//!
//! Matching reads borrowed display fields without constructing lowercase copies.
//! The caller bounds its catalog and chooses how many ranked results to retain.
//! Case comparison is scalar based; it does not normalize text or apply locale-dependent collation.

#![forbid(unsafe_code)]

/// Match quality ordered from an exact field to a sparse subsequence.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum LauncherQueryMatchRank {
    /// The whole field equals the trimmed query under the supported case rules.
    Exact = 0,
    /// The field starts with the query.
    Prefix = 1,
    /// A whitespace-, hyphen-, or underscore-delimited word starts with the query.
    WordPrefix = 2,
    /// The query occurs as one contiguous part of the field.
    Contains = 3,
    /// Query characters occur in order, possibly separated by other characters.
    Subsequence = 4,
}

/// Rank a borrowed display field without allocating a normalized copy.
#[must_use]
pub fn launcher_query_match_rank(field: &str, raw_query: &str) -> Option<LauncherQueryMatchRank> {
    // Whitespace-only input has no filtering meaning; the catalog owns home rows.
    let query = raw_query.trim();
    if query.is_empty() {
        // Leave empty-query discovery to the caller instead of matching every field.
        return None;
    }
    if launcher_fields_equal_case_insensitive(field, query) {
        // Exact identities outrank every looser form of discovery.
        return Some(LauncherQueryMatchRank::Exact);
    }
    if launcher_field_starts_with_query(field, query) {
        // Preserve the familiar prefix behavior before considering word fragments.
        return Some(LauncherQueryMatchRank::Prefix);
    }
    if field
        .split(|character: char| {
            // Treat display-name separators as word boundaries without allocating.
            character.is_whitespace() || matches!(character, '-' | '_')
        })
        .any(|word| {
            // A word prefix is more specific than an arbitrary interior substring.
            launcher_field_starts_with_query(word, query)
        })
    {
        // Promote a named word such as Chrome in Google Chrome.
        return Some(LauncherQueryMatchRank::WordPrefix);
    }
    if launcher_field_contains_query(field, query) {
        // Keep contiguous matches ahead of sparse abbreviation matches.
        return Some(LauncherQueryMatchRank::Contains);
    }
    // The final fallback supports abbreviations while preserving character order.
    launcher_field_contains_query_subsequence(field, query)
        .then_some(LauncherQueryMatchRank::Subsequence)
}

/// Compare fields without allocating lowercase or normalized strings.
#[must_use]
pub fn launcher_fields_equal_case_insensitive(left: &str, right: &str) -> bool {
    if left.is_ascii() && right.is_ascii() {
        // ASCII can compare bytes directly and avoids Unicode case iterators.
        return left.eq_ignore_ascii_case(right);
    }

    // Consume Unicode scalar pairs together so unequal lengths cannot match.
    let mut left_characters = left.chars();
    let mut right_characters = right.chars();
    loop {
        // Every iteration consumes a scalar or reaches a terminal outcome.
        match (left_characters.next(), right_characters.next()) {
            (Some(left), Some(right))
                if launcher_characters_equal_case_insensitive(left, right) =>
            {
                // This pair matches; both iterators have already advanced.
            }
            (None, None) => {
                // Equal contents and simultaneous exhaustion establish equality.
                return true;
            }
            _ => {
                // A differing scalar or one-sided exhaustion rejects equality.
                return false;
            }
        }
    }
}

/// Check only the field prefix, respecting scalar boundaries for non-ASCII input.
fn launcher_field_starts_with_query(field: &str, query: &str) -> bool {
    if query.is_ascii()
        && let Some(prefix) = field
            .as_bytes()
            .get(..query.len())
            .filter(|prefix| prefix.is_ascii())
    {
        // Only the compared prefix needs to be ASCII; do not rescan the entire field at each
        // substring candidate. Unicode case pairs fall through to the shared scalar comparison.
        return prefix.eq_ignore_ascii_case(query.as_bytes());
    }

    // Pair query scalars with only the corresponding field prefix, leaving the remaining suffix unread.
    let mut field_characters = field.chars();
    query.chars().all(|query_character| {
        // Advance once per requested scalar, rejecting a shorter field naturally.
        field_characters.next().is_some_and(|field_character| {
            // Unicode comparison preserves the original borrowed field.
            launcher_characters_equal_case_insensitive(field_character, query_character)
        })
    })
}

/// Search contiguous candidates; callers guarantee that the query is nonempty.
fn launcher_field_contains_query(field: &str, query: &str) -> bool {
    if query.is_ascii() && field.is_ascii() {
        // Both ASCII operands permit borrowed byte windows. Mixed-script fields must use the
        // same scalar comparison as equality or typing an ASCII alias can lose a valid result.
        return field.as_bytes().windows(query.len()).any(|candidate| {
            // Each borrowed window is checked without creating a substring.
            candidate.eq_ignore_ascii_case(query.as_bytes())
        });
    }

    // Unicode candidates start at scalar boundaries, making every slice valid UTF-8.
    field.char_indices().any(|(start, _)| {
        // Reuse scalar-aware prefix matching for this valid suffix.
        launcher_field_starts_with_query(&field[start..], query)
    })
}

/// Match an ordered abbreviation in one pass with constant iterator state.
fn launcher_field_contains_query_subsequence(field: &str, query: &str) -> bool {
    if query.is_ascii() && field.is_ascii() {
        // This fast path needs two ASCII operands; otherwise a Unicode scalar can satisfy an
        // ASCII query character through the case rules used by every other quality tier.
        let mut query_bytes = query.bytes();
        let Some(mut expected) = query_bytes.next() else {
            // Defend the private helper even if called without the public guard.
            return false;
        };
        for candidate in field.bytes() {
            // Only matching bytes advance the query; intervening bytes are skipped.
            if candidate.eq_ignore_ascii_case(&expected) {
                // This candidate satisfies the next abbreviation character.
                let Some(next) = query_bytes.next() else {
                    // The final requested byte has matched; later field bytes do not matter.
                    return true;
                };
                // Keep scanning for the next outstanding query byte.
                expected = next;
            }
        }
        // The field ended before all query bytes were satisfied.
        return false;
    }

    // Non-ASCII abbreviations advance scalar iterators to avoid split UTF-8 input.
    let mut query_characters = query.chars();
    let Some(mut expected) = query_characters.next() else {
        // An empty helper query cannot be a meaningful abbreviation.
        return false;
    };
    for candidate in field.chars() {
        // Preserve character order while skipping unrelated field characters.
        if launcher_characters_equal_case_insensitive(candidate, expected) {
            // The current requested scalar has been found.
            let Some(next) = query_characters.next() else {
                // Every query scalar is satisfied, regardless of the remaining suffix.
                return true;
            };
            // Retain only the next required scalar, without a query allocation.
            expected = next;
        }
    }
    // At least one query scalar remained when the field was exhausted.
    false
}

/// Compare one scalar pair using ASCII fast paths and Unicode case iterators.
fn launcher_characters_equal_case_insensitive(left: char, right: char) -> bool {
    // Exact/ASCII comparisons short-circuit first. Scalar case iterators also cover expanding case mappings
    // without allocating strings; uppercase comparison covers pairs whose lowercase forms differ.
    left == right
        || (left.is_ascii() && right.is_ascii() && left.eq_ignore_ascii_case(&right))
        || left.to_lowercase().eq(right.to_lowercase())
        || left.to_uppercase().eq(right.to_uppercase())
}

#[cfg(test)]
mod tests {
    //! Behavioral ranking, Unicode, and empty-query boundaries.
    use super::{LauncherQueryMatchRank, launcher_query_match_rank};

    /// Quality tiers remain ordered so a sparse abbreviation cannot outrank a complete application name.
    #[test]
    fn ranks_case_insensitive_exact_prefix_contains_and_subsequence_matches() {
        // Exercise the public quality ordering through realistic app-name queries.
        for (query, expected) in [
            ("google chrome", LauncherQueryMatchRank::Exact),
            ("GOOGLE", LauncherQueryMatchRank::Prefix),
            ("cHrOmE", LauncherQueryMatchRank::WordPrefix),
            ("gle chr", LauncherQueryMatchRank::Contains),
            ("gch", LauncherQueryMatchRank::Subsequence),
        ] {
            // Each fixture selects a distinct quality tier on the same display field.
            assert_eq!(
                launcher_query_match_rank("Google Chrome", query),
                Some(expected),
                "query {query:?}"
            );
        }
    }

    /// Non-ASCII case pairs and abbreviations use scalar boundaries without a normalized text buffer.
    #[test]
    fn compares_unicode_case_without_normalized_allocations() {
        // Greek case pairs must match exactly while Cyrillic abbreviations remain ordered.
        assert_eq!(
            launcher_query_match_rank("ΔELTA", "δelta"),
            Some(LauncherQueryMatchRank::Exact)
        );
        assert_eq!(
            launcher_query_match_rank("Пример Заметок", "пз"),
            Some(LauncherQueryMatchRank::Subsequence)
        );
        // Unrelated localized fields must stay absent rather than matching through lossy normalization.
        assert_eq!(launcher_query_match_rank("笔记", "文件"), None);
    }

    /// ASCII input must retain the same Unicode case rules at every discovery quality tier.
    #[test]
    fn ascii_queries_match_unicode_case_pairs_in_application_names() {
        // These are display names, not normalized copies. Prefix, word, interior and abbreviation
        // matching must preserve the original borrowed text and the existing scalar case policy.
        for (field, query, expected) in [
            ("Keynote", "key", LauncherQueryMatchRank::Prefix),
            ("工具 Keynote", "key", LauncherQueryMatchRank::WordPrefix),
            ("myKeynote", "key", LauncherQueryMatchRank::Contains),
            ("ſelected Notes", "sn", LauncherQueryMatchRank::Subsequence),
            ("ıdea Board", "idea", LauncherQueryMatchRank::Prefix),
        ] {
            assert_eq!(launcher_query_match_rank(field, query), Some(expected));
        }
    }

    /// Empty discovery and insufficient field text never manufacture a match or persistent query state.
    #[test]
    fn rejects_empty_and_longer_queries_without_match_state() {
        // Home discovery belongs to the catalog, and unavailable characters cannot match.
        assert_eq!(launcher_query_match_rank("Markdown", " \t"), None);
        assert_eq!(launcher_query_match_rank("md", "markdown"), None);
        assert_eq!(launcher_query_match_rank("", "m"), None);
    }
}
