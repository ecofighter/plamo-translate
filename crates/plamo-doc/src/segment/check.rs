//! Drift detection between the sent text and the model output: a line-structure
//! signature and a multiset of inline elements the translation should carry over.

use std::fmt::Write as _;
use std::ops::Range;

use super::finish::find_markers;
use super::{Format, Issue};

/// Compares `sent` with the normalized model output `got` (both masked, links in
/// inline form, markers in place).
pub(super) fn issues(format: Format, sent: &str, got: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    if format == Format::Markdown {
        let expected = signature(sent);
        let actual = signature(got);
        if expected != actual {
            let (expected, got) = describe_difference(&expected, &actual);
            issues.push(Issue::Structure { expected, got });
        }
    }
    let (missing, added) =
        multiset_difference(inline_elements(format, sent), inline_elements(format, got));
    if !missing.is_empty() || !added.is_empty() {
        issues.push(Issue::Inline { missing, added });
    }
    issues
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Placeholder(Option<u32>),
    Blank,
    Heading(usize),
    Item,
    Row(usize),
    Fence,
    Text,
}

/// Quote depth and line class. A placeholder entry (depth 0) separates the text on
/// either side of a marker.
type Line = (usize, Class);

/// Signatures of the text between markers, joined by one placeholder entry per
/// marker. Blank and quote-only lines next to a marker are left out because
/// reassembly takes the whitespace around a hole from the source, so only the lines
/// inside each fragment are compared.
fn signature(s: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut cursor = 0;
    for marker in find_markers(s) {
        lines.extend(fragment_signature(&s[cursor..marker.range.start]));
        lines.push((0, Class::Placeholder(marker.id)));
        cursor = marker.range.end;
    }
    lines.extend(fragment_signature(&s[cursor..]));
    lines
}

/// One entry per line, with runs of plain text lines at the same quote depth
/// collapsed into one. A lazy continuation line (unquoted text right after quoted
/// text) belongs to the quote.
fn fragment_signature(fragment: &str) -> Vec<Line> {
    let fragment = trim_fragment(fragment);
    if fragment.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut previous: Option<Line> = None;
    for line in fragment.split('\n') {
        let (depth, rest) = strip_quotes(line);
        let mut entry = (depth, classify(rest));
        if let Some((prev_depth, Class::Text)) = previous
            && entry == (0, Class::Text)
        {
            entry.0 = prev_depth;
        }
        previous = Some(entry);
        if entry.1 == Class::Text && lines.last() == Some(&entry) {
            continue;
        }
        lines.push(entry);
    }
    lines
}

/// Drops whitespace and whole lines holding only spaces, tabs and `>` from both
/// edges.
fn trim_fragment(fragment: &str) -> &str {
    let quote_only = |line: &str| line.chars().all(|c| matches!(c, ' ' | '\t' | '>' | '\r'));
    let mut s = fragment.trim();
    loop {
        let before = s.len();
        let first_end = s.find('\n').unwrap_or(s.len());
        if quote_only(&s[..first_end]) {
            s = s[first_end..].trim();
        }
        let last_start = s.rfind('\n').map_or(0, |i| i + 1);
        if quote_only(&s[last_start..]) {
            s = s[..last_start].trim();
        }
        if s.len() == before {
            return s;
        }
    }
}

/// Strips `^ {0,3}>[ ]?` repeatedly; returns the number stripped and the rest.
fn strip_quotes(mut line: &str) -> (usize, &str) {
    let mut depth = 0;
    loop {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            break;
        }
        let Some(rest) = line[indent..].strip_prefix('>') else {
            break;
        };
        line = rest.strip_prefix(' ').unwrap_or(rest);
        depth += 1;
    }
    (depth, line)
}

fn classify(line: &str) -> Class {
    if line.chars().all(char::is_whitespace) {
        return Class::Blank;
    }
    let rest = line.trim_start_matches(' ');
    let after_hashes = rest.trim_start_matches('#');
    let hashes = rest.len() - after_hashes.len();
    if (1..=6).contains(&hashes) && ends_marker(after_hashes) {
        return Class::Heading(hashes);
    }
    if is_item(rest) {
        return Class::Item;
    }
    let trimmed = line.trim();
    if trimmed.starts_with('|') {
        return Class::Row(count_pipes(trimmed));
    }
    if rest.starts_with("```") || rest.starts_with("~~~") {
        return Class::Fence;
    }
    Class::Text
}

/// `( |$)` after a heading or list marker.
fn ends_marker(after: &str) -> bool {
    after.is_empty() || after.starts_with(' ')
}

/// `^([-*+]|\d{1,9}[.)])( |$)` on a line with its indent removed.
fn is_item(rest: &str) -> bool {
    if let Some(after) = rest.strip_prefix(['-', '*', '+']) {
        return ends_marker(after);
    }
    let after_digits = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    let digits = rest.len() - after_digits.len();
    (1..=9).contains(&digits)
        && after_digits
            .strip_prefix(['.', ')'])
            .is_some_and(ends_marker)
}

/// `|` characters outside code spans and not escaped with a backslash.
fn count_pipes(line: &str) -> usize {
    let spans = code_spans(line);
    let bytes = line.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(span) = spans.iter().find(|s| s.start == i) {
            i = span.end;
            continue;
        }
        match bytes[i] {
            b'\\' => i += 2,
            b'|' => {
                count += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    count
}

/// Ranges of code spans: a backtick run closed by the next run of the same length.
fn code_spans(s: &str) -> Vec<Range<usize>> {
    let bytes = s.as_bytes();
    let run_at = |i: usize| bytes[i..].iter().take_while(|&&b| b == b'`').count();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let open = run_at(i);
        let mut j = i + open;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let run = run_at(j);
                if run == open {
                    close = Some(j + run);
                    break;
                }
                j += run;
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                spans.push(i..end);
                i = end;
            }
            None => i += open,
        }
    }
    spans
}

fn describe(lines: &[Line]) -> String {
    let mut out = String::new();
    for (index, (depth, class)) in lines.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(&">".repeat(*depth));
        let _ = match class {
            Class::Placeholder(Some(id)) => write!(out, "k{id}"),
            Class::Placeholder(None) => write!(out, "k?"),
            Class::Blank => write!(out, "blank"),
            Class::Heading(n) => write!(out, "h{n}"),
            Class::Item => write!(out, "item"),
            Class::Row(k) => write!(out, "row{k}"),
            Class::Fence => write!(out, "fence"),
            Class::Text => write!(out, "text"),
        };
    }
    if out.is_empty() {
        out.push_str("nothing");
    }
    out
}

/// Lines worth of context shown around the first difference.
const CONTEXT: usize = 1;
/// Lines shown per side after the first difference.
const SHOWN: usize = 6;

/// Short descriptions of both signatures, starting just before the first difference.
fn describe_difference(expected: &[Line], got: &[Line]) -> (String, String) {
    let first = expected.iter().zip(got).take_while(|(a, b)| a == b).count();
    let from = first.saturating_sub(CONTEXT);
    let window = |lines: &[Line]| {
        let end = lines.len().min(first + SHOWN);
        let mut text = describe(&lines[from..end]);
        if from > 0 {
            text.insert_str(0, "… ");
        }
        if end < lines.len() {
            text.push_str(" …");
        }
        text
    };
    (window(expected), window(got))
}

/// Code spans, link destinations, HTML tags, bare URLs and footnote references in
/// Markdown; only bare URLs in plain text. Placeholders are not counted.
fn inline_elements(format: Format, s: &str) -> Vec<String> {
    let mut rest = blank_out(s, find_markers(s).into_iter().map(|m| m.range));
    let mut found = Vec::new();
    if format == Format::Markdown {
        let spans = code_spans(&rest);
        found.extend(spans.iter().map(|r| rest[r.clone()].to_owned()));
        rest = blank_out(&rest, spans);
        let dests = link_destinations(&rest);
        found.extend(dests.iter().map(|r| format!("]({})", &rest[r.clone()])));
        rest = blank_out(&rest, dests.into_iter().map(|r| r.start - 2..r.end));
        let tags = html_tags(&rest);
        found.extend(tags.iter().map(|r| rest[r.clone()].to_owned()));
        rest = blank_out(&rest, tags);
    }
    let urls = bare_urls(&rest);
    found.extend(urls.iter().map(|r| rest[r.clone()].to_owned()));
    if format == Format::Markdown {
        rest = blank_out(&rest, urls);
        found.extend(footnote_refs(&rest).into_iter().map(|r| rest[r].to_owned()));
    }
    found
}

/// Replaces each range with one space so that later scans neither see nor join
/// across it.
fn blank_out(s: &str, ranges: impl IntoIterator<Item = Range<usize>>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    for range in ranges {
        out.push_str(&s[cursor..range.start]);
        out.push(' ');
        cursor = range.end;
    }
    out.push_str(&s[cursor..]);
    out
}

/// After each `](`: the destination up to whitespace or `)`.
fn link_destinations(s: &str) -> Vec<Range<usize>> {
    let mut dests = Vec::new();
    let mut from = 0;
    while let Some(at) = s[from..].find("](").map(|i| from + i) {
        let start = at + 2;
        let len = s[start..]
            .find(|c: char| c.is_whitespace() || c == ')')
            .unwrap_or(s.len() - start);
        dests.push(start..start + len);
        from = start + len;
    }
    dests
}

/// `<[A-Za-z/!][^<>\n]*>`
fn html_tags(s: &str) -> Vec<Range<usize>> {
    let mut tags = Vec::new();
    let mut from = 0;
    while let Some(at) = s[from..].find('<').map(|i| from + i) {
        let body = &s[at + 1..];
        let opens = body
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        let close = body
            .find(['<', '>', '\n'])
            .filter(|&i| body.as_bytes()[i] == b'>');
        match (opens, close) {
            (true, Some(i)) => {
                let end = at + 1 + i + 1;
                tags.push(at..end);
                from = end;
            }
            _ => from = at + 1,
        }
    }
    tags
}

/// `https?://` up to whitespace, `<`, `>` or CJK punctuation, without trailing
/// sentence punctuation.
fn bare_urls(s: &str) -> Vec<Range<usize>> {
    let mut urls = Vec::new();
    let mut from = 0;
    while let Some(at) = s[from..].find("http").map(|i| from + i) {
        let after = &s[at + 4..];
        let scheme = if after.starts_with("://") {
            7
        } else if after.starts_with("s://") {
            8
        } else {
            from = at + 4;
            continue;
        };
        let len = s[at..]
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>' || is_cjk_punctuation(c))
            .unwrap_or(s.len() - at);
        let url =
            s[at..at + len].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"']);
        if url.len() > scheme {
            urls.push(at..at + url.len());
        }
        from = at + len.max(scheme);
    }
    urls
}

fn is_cjk_punctuation(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{303F}'
        | '\u{FF01}'..='\u{FF0F}'
        | '\u{FF1A}'..='\u{FF20}'
        | '\u{FF3B}'..='\u{FF40}'
        | '\u{FF5B}'..='\u{FF65}')
}

/// `[^label]` with no whitespace or `]` in the label.
fn footnote_refs(s: &str) -> Vec<Range<usize>> {
    s.match_indices("[^")
        .filter_map(|(at, _)| {
            let label = &s[at + 2..];
            let end = label.find(|c: char| c == ']' || c.is_whitespace())?;
            (end > 0 && label[end..].starts_with(']')).then(|| at..at + 2 + end + 1)
        })
        .collect()
}

/// Sorted elements of `expected` not in `got`, and of `got` not in `expected`,
/// counting duplicates.
fn multiset_difference(
    mut expected: Vec<String>,
    mut got: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    expected.sort();
    got.sort();
    let (mut missing, mut added) = (Vec::new(), Vec::new());
    let (mut a, mut b) = (expected.into_iter().peekable(), got.into_iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if x == y => {
                a.next();
                b.next();
            }
            (Some(x), Some(y)) if x < y => missing.extend(a.next()),
            (Some(_), Some(_)) => added.extend(b.next()),
            (Some(_), None) => missing.extend(a.next()),
            (None, Some(_)) => added.extend(b.next()),
            (None, None) => break,
        }
    }
    (missing, added)
}

#[cfg(test)]
mod tests {
    use super::super::tests::units;
    use super::super::{Issue, Limits, Unit, split};
    use super::*;

    fn only_unit(input: &str) -> Unit {
        let split = split(input, Format::Markdown, &Limits::default());
        let units = units(&split.segments);
        assert_eq!(units.len(), 1, "{:?}", split.segments);
        assert_eq!(units[0].text(), input);
        units[0].clone()
    }

    fn issues_of(unit: &Unit, raw: &str) -> Vec<Issue> {
        unit.finish(raw).unwrap().issues
    }

    fn is_structure(issues: &[Issue]) -> bool {
        matches!(issues, [Issue::Structure { .. }])
    }

    #[test]
    fn inline_elements_that_changed_are_reported() {
        let unit = only_unit("See `cfg` at https://x.y/a?b=1.");
        assert_eq!(
            issues_of(&unit, "`設定` は https://x.y/a を参照"),
            [Issue::Inline {
                missing: vec!["`cfg`".into(), "https://x.y/a?b=1".into()],
                added: vec!["`設定`".into(), "https://x.y/a".into()],
            }]
        );
    }

    #[test]
    fn heading_and_blank_line_changes_are_structure_issues() {
        let unit = only_unit("## Title\n\n- a\n- b");
        assert_eq!(issues_of(&unit, "## タイトル\n\n- あ\n- い"), []);
        assert!(is_structure(&issues_of(&unit, "タイトル\n\n- あ\n- い")));
        assert!(is_structure(&issues_of(&unit, "## タイトル\n- あ\n- い")));
    }

    #[test]
    fn lost_table_column_is_a_structure_issue() {
        let unit = only_unit("| a | b | c |\n|---|---|---|");
        assert_eq!(issues_of(&unit, "| あ | い | う |\n|---|---|---|"), []);
        let issues = issues_of(&unit, "| あ | い | う |\n|---|---|");
        assert!(is_structure(&issues), "{issues:?}");
    }

    #[test]
    fn line_classes() {
        use Class::*;
        let sig = signature(
            "# A\n####### no\n  - x\n10) y\n1234567890. z\n> | a \\| `b|c` |\n```rs\n>> text\n>> more\n> <!-- pt:k3 -->\n \n#",
        );
        assert_eq!(
            sig,
            [
                (0, Heading(1)),
                (0, Text),
                (0, Item),
                (0, Item),
                (0, Text),
                (1, Row(2)),
                (0, Fence),
                (2, Text),
                (0, Placeholder(Some(3))),
                (0, Heading(1)),
            ]
        );
        assert_eq!(strip_quotes("    > x"), (0, "    > x"));
        assert_eq!(strip_quotes(" >> >x"), (3, "x"));
    }

    #[test]
    fn whitespace_and_quote_prefixes_next_to_markers_are_ignored() {
        let split = split(
            "> text\n> ```\n> code\n> ```\n> more",
            Format::Markdown,
            &Limits::default(),
        );
        let unit = units(&split.segments)[0];
        assert_eq!(unit.text(), "> text\n> <!--pt:k1-->\n> more");
        assert_eq!(issues_of(unit, "> a\n<!--pt:k1-->\n> b"), []);
        assert_eq!(issues_of(unit, "> a\n\n> <!--pt:k1-->\n\n> b"), []);
        assert_eq!(issues_of(unit, "> a <!--pt:k1--> > b"), []);
        assert!(is_structure(&issues_of(unit, "a\n> <!--pt:k1-->\n> b")));
        assert!(is_structure(&issues_of(
            unit,
            "> a\n>\n> c\n<!--pt:k1-->\n> b"
        )));
    }

    #[test]
    fn structure_is_compared_per_fragment() {
        let sent = "a\n\n<!--pt:k1-->\n\nb\n\nc";
        assert_eq!(
            issues(Format::Markdown, sent, "x\n<!--pt:k1-->\ny\n\nz"),
            []
        );
        assert_eq!(
            issues(Format::Markdown, sent, "x\n<!--pt:k1-->\ny\nz"),
            [Issue::Structure {
                expected: "… text blank text".into(),
                got: "… text".into(),
            }]
        );
    }

    #[test]
    fn structure_description_starts_near_the_first_difference() {
        let issues = issues(
            Format::Markdown,
            "# A\n\nt\n\n- a\n- b",
            "# A\n\nt\n- a\n- b",
        );
        assert_eq!(
            issues,
            [Issue::Structure {
                expected: "… text blank item item".into(),
                got: "… text item item".into(),
            }]
        );
        let lines: Vec<Line> = (0..10).map(|_| (0, Class::Item)).collect();
        let (expected, got) = describe_difference(&lines, &[]);
        assert_eq!(expected, "item item item item item item …");
        assert_eq!(got, "nothing");
    }

    #[test]
    fn inline_element_kinds() {
        let found = inline_elements(
            Format::Markdown,
            "a [x](http://y/z \"t\") <b>c</b> <!--pt:k1--> <https://u.v/w> \
             `x https://no` see https://p.q/r), 「https://c.d/e」 [^n] [^ no] 1 < 2 > 0",
        );
        assert_eq!(
            found,
            [
                "`x https://no`",
                "](http://y/z)",
                "<b>",
                "</b>",
                "<https://u.v/w>",
                "https://p.q/r",
                "https://c.d/e",
                "[^n]",
            ]
        );
        assert_eq!(
            inline_elements(Format::Text, "`a` <b> [^n] https://x.y/z."),
            ["https://x.y/z"]
        );
    }

    #[test]
    fn text_format_checks_urls_but_not_structure() {
        let issues = issues(Format::Text, "# A\nsee https://a.b", "A\nsee https://a.c");
        assert_eq!(
            issues,
            [Issue::Inline {
                missing: vec!["https://a.b".into()],
                added: vec!["https://a.c".into()],
            }]
        );
        assert_eq!(
            issues[0].to_string(),
            "inline elements changed (missing: https://a.b; added: https://a.c)"
        );
    }

    #[test]
    fn duplicates_count_in_the_multiset() {
        let v = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            multiset_difference(v(&["b", "a", "a"]), v(&["a", "c"])),
            (v(&["a", "b"]), v(&["c"]))
        );
    }

    #[test]
    fn link_destinations_never_overlap() {
        assert_eq!(link_destinations("](a](b) ](c"), [2..6, 10..11]);
    }

    #[test]
    fn blank_quote_lines_next_to_a_marker_are_not_structure() {
        let split = split(
            "> para\n>\n> ```\n> c\n> ```\n>\n> more\n",
            Format::Markdown,
            &Limits::default(),
        );
        let unit = units(&split.segments)[0];
        assert_eq!(unit.text(), "> para\n>\n> <!--pt:k1-->\n>\n> more");
        for raw in [
            "> 段落\n> <!--pt:k1-->\n>\n> 続き",
            "> 段落\n>\n> <!--pt:k1-->\n> 続き",
        ] {
            assert_eq!(issues_of(unit, raw), [], "{raw:?}");
        }
        assert!(is_structure(&issues_of(
            unit,
            "> 段落\n>\n> 段落2\n> <!--pt:k1-->\n> 続き"
        )));
        assert_eq!(trim_fragment("\n>\n> > \n> a\n>\n> b\n >\n"), "> a\n>\n> b");
        assert_eq!(trim_fragment(">\n> \n"), "");
    }

    #[test]
    fn lazy_continuation_lines_belong_to_the_quote() {
        let split = split("> a\nb", Format::Markdown, &Limits::default());
        let unit = units(&split.segments)[0];
        assert_eq!(unit.text(), "> a\nb");
        assert_eq!(issues_of(unit, "> あ\n> い"), []);
        assert_eq!(
            signature("> a\nb\n\nc"),
            [(1, Class::Text), (0, Class::Blank), (0, Class::Text)]
        );
        assert_eq!(signature("> - a\nb"), [(1, Class::Item), (0, Class::Text)]);
    }
}
