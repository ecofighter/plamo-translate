use crate::protocol::GlossaryEntry;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("glossary line {line}: {message}")]
pub struct GlossaryError {
    pub line: usize,
    pub message: String,
}

pub fn parse_glossary(s: &str) -> Result<Vec<GlossaryEntry>, GlossaryError> {
    let mut entries = Vec::new();
    for (i, raw) in s.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() != 2 || cols[0].is_empty() || cols[1].is_empty() {
            return Err(GlossaryError {
                line: i + 1,
                message: "expected 2 tab-separated non-empty columns".into(),
            });
        }
        entries.push(GlossaryEntry {
            source: cols[0].into(),
            target: cols[1].into(),
        });
    }
    Ok(entries)
}

pub fn merge_glossaries(lists: impl IntoIterator<Item = Vec<GlossaryEntry>>) -> Vec<GlossaryEntry> {
    let mut merged: Vec<GlossaryEntry> = Vec::new();
    for entry in lists.into_iter().flatten() {
        match merged.iter_mut().find(|e| e.source == entry.source) {
            Some(existing) => existing.target = entry.target,
            None => merged.push(entry),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(s: &str, t: &str) -> GlossaryEntry {
        GlossaryEntry {
            source: s.into(),
            target: t.into(),
        }
    }

    #[test]
    fn parses_tsv_skipping_comments_and_blank() {
        assert_eq!(
            parse_glossary("# c\n\n推論\tinference\n").unwrap(),
            vec![e("推論", "inference")]
        );
    }
    #[test]
    fn reports_line_number_on_bad_row() {
        assert_eq!(parse_glossary("a\tb\nbad\n").unwrap_err().line, 2);
    }
    #[test]
    fn rejects_extra_columns() {
        assert_eq!(parse_glossary("a\tb\tc\n").unwrap_err().line, 1);
    }
    #[test]
    fn merge_later_wins_keeps_position() {
        let m = merge_glossaries([vec![e("a", "1"), e("b", "2")], vec![e("a", "3")]]);
        assert_eq!(m, vec![e("a", "3"), e("b", "2")]);
    }
}
