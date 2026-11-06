use plamo_client::HistoryEntry;

/// Picks the history sent with one request: the newest pair always (unless the budget is 0),
/// then older pairs while the total `source.len() + translation.len()` stays within
/// `budget_bytes`. The result is a contiguous suffix of `pool`, oldest first.
pub fn select_history(pool: &[HistoryEntry], budget_bytes: usize) -> Vec<HistoryEntry> {
    if budget_bytes == 0 {
        return Vec::new();
    }
    let mut used = 0usize;
    let mut start = pool.len();
    for (i, entry) in pool.iter().enumerate().rev() {
        let size = entry.source.len() + entry.translation.len();
        if start < pool.len() && used + size > budget_bytes {
            break;
        }
        used += size;
        start = i;
    }
    pool[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(source: &str, translation: &str) -> HistoryEntry {
        HistoryEntry {
            source: source.into(),
            translation: translation.into(),
        }
    }

    #[test]
    fn select_history_keeps_newest_suffix_within_budget() {
        let pool = vec![pair("aaaa", "bbbb"), pair("cc", "dd"), pair("e", "f")];

        assert_eq!(select_history(&pool, 0), vec![]);
        assert_eq!(select_history(&pool, 100), pool);
        assert_eq!(select_history(&pool, 6), pool[1..].to_vec());
        assert_eq!(select_history(&[], 10), vec![]);

        let big = vec![pair("x", "y"), pair("0123456789", "0123456789")];
        assert_eq!(select_history(&big, 5), vec![big[1].clone()]);

        let gap = vec![
            pair("x", "y"),
            pair("long source", "long"),
            pair("ab", "cd"),
        ];
        assert_eq!(select_history(&gap, 10), vec![gap[2].clone()]);
    }
}
