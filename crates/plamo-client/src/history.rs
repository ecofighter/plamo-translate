use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::protocol::HistoryEntry;

pub const RESERVED_TOKEN_PREFIX: &str = "<|plamo:";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Ineligible {
    #[error("the translation did not finish normally (finish_reason: {})", .0.as_deref().unwrap_or("none"))]
    NotStopped(Option<String>),
    #[error("the translation is empty")]
    Empty,
    #[error("the translation contains a reserved PFML token (<|plamo:)")]
    ReservedToken,
}

/// Check order: finish_reason, empty, reserved.
pub fn check_history_eligible(
    finish_reason: Option<&str>,
    translation: &str,
) -> Result<(), Ineligible> {
    if finish_reason != Some("stop") {
        return Err(Ineligible::NotStopped(finish_reason.map(str::to_owned)));
    }
    if translation.chars().all(is_python_whitespace) {
        return Err(Ineligible::Empty);
    }
    if translation.contains(RESERVED_TOKEN_PREFIX) {
        return Err(Ineligible::ReservedToken);
    }
    Ok(())
}

/// `source` and `translation` are copied byte-for-byte.
pub fn history_entry(
    source: &str,
    finish_reason: Option<&str>,
    translation: &str,
) -> Result<HistoryEntry, Ineligible> {
    check_history_eligible(finish_reason, translation)?;
    Ok(HistoryEntry {
        source: source.to_owned(),
        translation: translation.to_owned(),
    })
}

// Python's str.strip() also strips U+001C..U+001F, which char::is_whitespace does not;
// the daemon's emptiness check is Python's, so match it exactly.
fn is_python_whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("history line {line}: {message}")]
pub struct HistoryError {
    pub line: usize,
    pub message: String,
}

/// JSONL: one {"source": string, "translation": string} object per line, oldest first.
/// A leading U+FEFF is stripped; whitespace-only lines are skipped; no comments.
pub fn parse_history(s: &str) -> Result<Vec<HistoryEntry>, HistoryError> {
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let mut entries = Vec::new();
    for (i, line) in s.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = serde_json::from_str(line).map_err(|e| HistoryError {
            line: i + 1,
            message: format!("expected {{\"source\": string, \"translation\": string}}: {e}"),
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// One write_all of serde_json::to_string(entry) + "\n" to a file opened with create+append;
/// a leading "\n" is written first if the file is non-empty and its last byte is not b'\n'.
pub fn append_history(path: &Path, entry: &HistoryEntry) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let mut buf = String::new();
    if file.metadata()?.len() > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            buf.push('\n');
        }
    }
    buf.push_str(&serde_json::to_string(entry).map_err(std::io::Error::other)?);
    buf.push('\n');
    file.write_all(buf.as_bytes())
}

/// Removes the oldest ceil(n/2) entries and returns how many were removed.
pub fn drop_oldest_half(history: &mut Vec<HistoryEntry>) -> usize {
    let n = history.len().div_ceil(2);
    history.drain(..n);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(s: &str, t: &str) -> HistoryEntry {
        HistoryEntry {
            source: s.into(),
            translation: t.into(),
        }
    }

    #[test]
    fn eligible_only_when_stop_nonblank_and_no_reserved() {
        let cases: [(Option<&str>, &str, Result<(), Ineligible>); 8] = [
            (Some("stop"), "Hi", Ok(())),
            (
                Some("length"),
                "Hi",
                Err(Ineligible::NotStopped(Some("length".into()))),
            ),
            (None, "Hi", Err(Ineligible::NotStopped(None))),
            (Some("stop"), "", Err(Ineligible::Empty)),
            (Some("stop"), " \u{3000}\n", Err(Ineligible::Empty)),
            (Some("stop"), "\u{1f}", Err(Ineligible::Empty)),
            (
                Some("stop"),
                "a<|plamo:tag|>",
                Err(Ineligible::ReservedToken),
            ),
            (
                Some("length"),
                "",
                Err(Ineligible::NotStopped(Some("length".into()))),
            ),
        ];
        for (fr, tr, want) in cases {
            assert_eq!(check_history_eligible(fr, tr), want, "{fr:?} {tr:?}");
        }
    }

    #[test]
    fn history_entry_copies_unchanged() {
        assert_eq!(
            history_entry(" Hi \n", Some("stop"), "会議\n").unwrap(),
            e(" Hi \n", "会議\n")
        );
    }

    #[test]
    fn ineligible_messages() {
        assert_eq!(
            Ineligible::NotStopped(Some("length".into())).to_string(),
            "the translation did not finish normally (finish_reason: length)"
        );
        assert_eq!(
            Ineligible::NotStopped(None).to_string(),
            "the translation did not finish normally (finish_reason: none)"
        );
        assert_eq!(Ineligible::Empty.to_string(), "the translation is empty");
        assert_eq!(
            Ineligible::ReservedToken.to_string(),
            "the translation contains a reserved PFML token (<|plamo:)"
        );
    }

    #[test]
    fn parse_history_accepts_blank_crlf_bom() {
        let s = "\u{feff}{\"source\":\"a\",\"translation\":\"b\"}\r\n\r\n  \t\r\n{\"source\":\"c\",\"translation\":\"d\"}\r\n";
        assert_eq!(parse_history(s).unwrap(), vec![e("a", "b"), e("c", "d")]);
        assert_eq!(parse_history("").unwrap(), vec![]);
    }

    #[test]
    fn parse_history_ignores_extra_keys() {
        let s = r#"{"source":"a","translation":"b","note":1}"#;
        assert_eq!(parse_history(s).unwrap(), vec![e("a", "b")]);
    }

    #[test]
    fn parse_history_reports_line() {
        let s = "{\"source\":\"a\",\"translation\":\"b\"}\n\n{\"source\":\"a\"}\n";
        let err = parse_history(s).unwrap_err();
        assert_eq!(err.line, 3);
        assert!(
            err.message
                .starts_with("expected {\"source\": string, \"translation\": string}: "),
            "{}",
            err.message
        );
        assert!(err.to_string().starts_with("history line 3: expected "));
    }

    #[test]
    fn parse_history_rejects_non_string() {
        assert_eq!(
            parse_history(r#"{"source":1,"translation":"b"}"#)
                .unwrap_err()
                .line,
            1
        );
    }

    #[test]
    fn append_history_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.jsonl");
        append_history(&p, &e("a", "b")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "{\"source\":\"a\",\"translation\":\"b\"}\n"
        );
    }

    #[test]
    fn append_history_adds_missing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.jsonl");
        std::fs::write(&p, "X").unwrap();
        append_history(&p, &e("a", "b")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "X\n{\"source\":\"a\",\"translation\":\"b\"}\n"
        );
    }

    #[test]
    fn drop_oldest_half_keeps_newest() {
        for (n, kept) in [(1usize, 0usize), (2, 1), (3, 1), (4, 2), (5, 2)] {
            let mut h: Vec<_> = (0..n).map(|i| e(&i.to_string(), "t")).collect();
            let removed = drop_oldest_half(&mut h);
            assert_eq!(removed, n - kept, "n={n}");
            assert_eq!(h.len(), kept, "n={n}");
            if kept > 0 {
                assert_eq!(h.last().unwrap().source, (n - 1).to_string());
            }
        }
    }
}
