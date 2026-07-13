//! Fuzzy search functionality for branch names

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

/// Result of a fuzzy search match
#[derive(Debug, Clone)]
pub struct FuzzySearchResult {
    /// Index into branch_positions
    pub branch_idx: usize,
    /// Fuzzy match score (higher is better)
    pub score: i64,
    /// Character indices that matched (for highlighting)
    pub matched_indices: Vec<usize>,
}

/// Performs fuzzy search on branch names
///
/// Returns results sorted by score (descending), then by `branch_idx` —
/// the branch's position in the input `branches` slice — ascending for ties.
pub fn fuzzy_search_branches(query: &str, branches: &[(usize, String)]) -> Vec<FuzzySearchResult> {
    if query.is_empty() {
        return Vec::new();
    }

    let matcher = SkimMatcherV2::default();
    let mut results: Vec<FuzzySearchResult> = branches
        .iter()
        .enumerate()
        .filter_map(|(idx, (_, name))| {
            matcher
                .fuzzy_indices(name, query)
                .map(|(score, indices)| FuzzySearchResult {
                    branch_idx: idx,
                    score,
                    matched_indices: indices,
                })
        })
        .collect();

    // Sort by score descending, then by branch_idx ascending (newer branches first)
    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.branch_idx.cmp(&b.branch_idx))
    });

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_query_returns_empty() {
        let branches = vec![(0, "main".to_string()), (1, "feature/test".to_string())];
        let results = fuzzy_search_branches("", &branches);
        assert!(results.is_empty());
    }

    #[test]
    fn test_exact_match() {
        let branches = vec![(0, "main".to_string()), (1, "feature/test".to_string())];
        let results = fuzzy_search_branches("main", &branches);
        assert!(!results.is_empty());
        assert_eq!(results[0].branch_idx, 0);
    }

    #[test]
    fn test_fuzzy_match() {
        let branches = vec![
            (0, "main".to_string()),
            (1, "feature/auth".to_string()),
            (2, "feature/search".to_string()),
        ];
        // "feat" should match feature branches
        let results = fuzzy_search_branches("feat", &branches);
        assert!(results.len() >= 2);
        // feature branches should be in results
        assert!(results.iter().any(|r| r.branch_idx == 1));
        assert!(results.iter().any(|r| r.branch_idx == 2));
    }

    #[test]
    fn test_typo_tolerance() {
        let branches = vec![(0, "main".to_string()), (1, "feature/auth".to_string())];
        // "featre" (typo) should still match "feature"
        let results = fuzzy_search_branches("featre", &branches);
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.branch_idx == 1));
    }

    #[test]
    fn test_no_match() {
        let branches = vec![(0, "main".to_string()), (1, "develop".to_string())];
        let results = fuzzy_search_branches("xyz123", &branches);
        assert!(results.is_empty());
    }

    #[test]
    fn test_empty_branch_list_returns_empty() {
        let branches: Vec<(usize, String)> = Vec::new();
        let results = fuzzy_search_branches("main", &branches);
        assert!(results.is_empty());
    }

    #[test]
    fn test_whitespace_only_query_matches_names_containing_a_space() {
        // A whitespace-only query is not filtered by the empty-query check,
        // so it's handed to the fuzzy matcher like any other query. Pinning
        // the actual (not assumed) behavior: branch names without a space
        // character don't match a " " query...
        let branches = vec![(0, "main".to_string()), (1, "feature/test".to_string())];
        let results = fuzzy_search_branches(" ", &branches);
        assert!(results.is_empty());

        // ...but a name that does contain a literal space is matched.
        let branches_with_space = vec![(0, "weird branch".to_string())];
        let results = fuzzy_search_branches(" ", &branches_with_space);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].branch_idx, 0);
    }

    #[test]
    fn test_ties_broken_by_branch_idx_ascending() {
        // Three branches with an identical name produce identical fuzzy
        // scores, so the sort must fall back to `branch_idx` — the index
        // into the `branches` slice (assigned via `.enumerate()`), NOT the
        // branch name and NOT the tuple's first field — ascending.
        let branches = vec![
            (99, "abc".to_string()),
            (5, "abc".to_string()),
            (42, "abc".to_string()),
        ];
        let results = fuzzy_search_branches("abc", &branches);
        assert_eq!(results.len(), 3);
        let indices: Vec<usize> = results.iter().map(|r| r.branch_idx).collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn test_results_sorted_by_score() {
        let branches = vec![
            (0, "abc".to_string()),
            (1, "abcd".to_string()),
            (2, "abcde".to_string()),
        ];
        let results = fuzzy_search_branches("abc", &branches);
        // Results should be sorted by score (higher first)
        for i in 1..results.len() {
            assert!(results[i - 1].score >= results[i].score);
        }
    }
}
