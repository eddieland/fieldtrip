//! Finding the names a caller probably meant.

/// Returns up to `limit` of `candidates` that are close to `target`, nearest first.
///
/// A candidate is close when it is within a small edit distance of `target`, or when one contains the other.
/// The comparison ignores case.
pub(crate) fn nearest<'a>(target: &str, candidates: impl IntoIterator<Item = &'a str>, limit: usize) -> Vec<&'a str> {
    let wanted = target.to_lowercase();
    let allowed = (wanted.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, &str)> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let lowered = candidate.to_lowercase();
            let distance = edit_distance(&wanted, &lowered);
            let overlaps = !wanted.is_empty() && (lowered.contains(&wanted) || wanted.contains(&lowered));
            (distance <= allowed || overlaps).then_some((distance, candidate))
        })
        .collect();
    scored.sort_unstable();
    scored.dedup();
    scored.into_iter().take(limit).map(|(_, candidate)| candidate).collect()
}

/// Formats names as `` `a`, `b` ``.
pub(crate) fn backticked<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    names
        .into_iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The Levenshtein distance between two strings, counted in characters.
fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, left_char) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(row + 1);
        for (column, right_char) in right.iter().enumerate() {
            let substitution =
                previous.get(column).copied().unwrap_or(usize::MAX) + usize::from(left_char != *right_char);
            let deletion = previous
                .get(column + 1)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            let insertion = current.last().copied().unwrap_or(usize::MAX).saturating_add(1);
            current.push(substitution.min(deletion).min(insertion));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}
