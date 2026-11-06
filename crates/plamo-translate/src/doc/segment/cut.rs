//! Cutting a unit, or a candidate that is too large to send, into two pieces.
//!
//! Boundaries are ranked from coarse to fine: blank-line runs, list items and table
//! rows, line breaks, sentence ends, whitespace runs, and (only for oversize
//! candidates) any character boundary. Units with placeholders are cut at a
//! placeholder instead.

use std::ops::Range;

use super::{
    Cand, Format, Limits, Part, Segment, Unit, cand_size, is_delimiter_row, lf, marker, relative,
    sent_text, uses_crlf,
};

type Rewrites = [(Range<usize>, String)];
/// Separator ranges where one boundary level allows a cut.
type Level = fn(&str) -> Vec<Range<usize>>;

/// The left piece is `..left_end`, the right piece `right_start..`; the separator
/// between them stays verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cut {
    left_end: usize,
    right_start: usize,
}

/// Bisects every candidate whose sent size exceeds `max_unit_bytes` until each piece
/// fits or cannot be cut further. Pieces are never merged with their neighbours.
pub(super) fn split_oversize(input: &str, cands: Vec<Cand>, limits: &Limits) -> Vec<Cand> {
    let mut out = Vec::with_capacity(cands.len());
    for cand in cands {
        bisect(input, cand, limits, &mut out);
    }
    out
}

fn bisect(input: &str, cand: Cand, limits: &Limits, out: &mut Vec<Cand>) {
    if cand_size(input, &cand) <= limits.max_unit_bytes {
        out.push(cand);
        return;
    }
    let base = cand.range.start;
    let src = &input[cand.range.clone()];
    let Some(cut) = find_cut(src, &relative(&cand.rewrites, base), true) else {
        out.push(cand);
        return;
    };
    let (left, right): (Vec<_>, Vec<_>) = cand
        .rewrites
        .into_iter()
        .partition(|(r, _)| r.end <= base + cut.left_end);
    let piece = |range: Range<usize>, rewrites| Cand {
        range,
        rewrites,
        heading: false,
        no_merge: true,
    };
    bisect(input, piece(base..base + cut.left_end, left), limits, out);
    bisect(
        input,
        piece(base + cut.right_start..cand.range.end, right),
        limits,
        out,
    );
}

/// One cut of `unit`: at the placeholder nearest the middle if it has any, otherwise
/// at the best text boundary. `None` if no cut leaves translatable text on both sides.
pub(super) fn split_unit(unit: &Unit) -> Option<Vec<Segment>> {
    if unit.parts.iter().any(|p| matches!(p, Part::Hole { .. })) {
        return Some(split_at_hole(unit));
    }
    let [Part::Text { src, rewrites }] = unit.parts.as_slice() else {
        unreachable!("a unit without holes is a single text part");
    };
    let cut = find_cut(src, rewrites, false)?;
    let text_part = |range: Range<usize>| Part::Text {
        src: src[range.clone()].to_owned(),
        rewrites: rewrites
            .iter()
            .filter(|(r, _)| range.start <= r.start && r.end <= range.end)
            .map(|(r, s)| (r.start - range.start..r.end - range.start, s.clone()))
            .collect(),
    };
    let left = text_part(0..cut.left_end);
    let right = text_part(cut.right_start..src.len());
    Some(pieces(
        unit.format,
        vec![left],
        &src[cut.left_end..cut.right_start],
        vec![right],
    ))
}

fn split_at_hole(unit: &Unit) -> Vec<Segment> {
    let mut offset = 0;
    let mut markers = Vec::new();
    for (index, part) in unit.parts.iter().enumerate() {
        match part {
            Part::Text { src, rewrites } => offset += sent_text(src, rewrites).len(),
            Part::Hole {
                id,
                lead,
                qprefix,
                trail,
                ..
            } => {
                let at = offset + lf(lead).len() + qprefix.len();
                markers.push((index, at));
                offset = at + marker(*id).len() + lf(trail).len();
            }
        }
    }
    let (index, _) = markers
        .into_iter()
        .min_by_key(|&(_, at)| (2 * at).abs_diff(offset))
        .expect("caller checked for a hole");
    let Part::Hole {
        lead, body, trail, ..
    } = &unit.parts[index]
    else {
        unreachable!();
    };
    let separator = format!("{lead}{body}{trail}");
    pieces(
        unit.format,
        unit.parts[..index].to_vec(),
        &separator,
        unit.parts[index + 1..].to_vec(),
    )
}

fn pieces(format: Format, left: Vec<Part>, separator: &str, right: Vec<Part>) -> Vec<Segment> {
    let mut out = vec![Segment::Translate(unit_from_parts(left, format))];
    if !separator.is_empty() {
        out.push(Segment::Keep(separator.to_owned()));
    }
    out.push(Segment::Translate(unit_from_parts(right, format)));
    out
}

fn unit_from_parts(parts: Vec<Part>, format: Format) -> Unit {
    let mut source = String::new();
    for part in &parts {
        match part {
            Part::Text { src, .. } => source.push_str(src),
            Part::Hole {
                lead, body, trail, ..
            } => {
                source.push_str(lead);
                source.push_str(body);
                source.push_str(trail);
            }
        }
    }
    Unit {
        parts,
        format,
        crlf: uses_crlf(&source),
        source,
    }
}

/// Picks the cut of `s` (a trimmed text with `rewrites` relative to it). Among the
/// levels, the coarsest one with a valid cut in the middle half wins; otherwise the
/// coarsest level with any valid cut; otherwise, if allowed, a character boundary.
/// Within a level the cut nearest the middle wins.
fn find_cut(s: &str, rewrites: &Rewrites, allow_any_char: bool) -> Option<Cut> {
    let first_alnum = s.char_indices().find(|(_, c)| c.is_alphanumeric())?.0;
    let last_alnum = s.char_indices().rfind(|(_, c)| c.is_alphanumeric())?.0;
    let guarded = table_guarded_newlines(s);
    let header_rows = table_header_rows(s);
    let row_lines = table_row_lines(s);
    let valid = |separator: Range<usize>| {
        let cut = retrim(s, separator);
        let both_sides_translatable = first_alnum < cut.left_end && last_alnum >= cut.right_start;
        (both_sides_translatable
            && !overlaps_guard(&guarded, cut)
            && !header_rows.iter().any(|rows| cuts_inside(rows, cut))
            && !within_line(&row_lines, cut)
            && !splits_rewrite(rewrites, cut))
        .then_some(cut)
    };
    let len = s.len();
    let nearest = |found: &mut dyn Iterator<Item = (usize, Cut)>| {
        found
            .min_by_key(|&(start, _)| (2 * start).abs_diff(len))
            .map(|(_, cut)| cut)
    };
    let levels: [Level; 5] = [
        blank_line_runs,
        list_or_table_lines,
        newlines,
        sentence_ends,
        whitespace_runs,
    ];
    let found: Vec<Vec<(usize, Cut)>> = levels
        .iter()
        .map(|level| {
            level(s)
                .into_iter()
                .filter_map(|sep| Some((sep.start, valid(sep)?)))
                .collect()
        })
        .collect();
    let in_middle = |&&(start, _): &&(usize, Cut)| len <= 4 * start && 4 * start <= 3 * len;
    found
        .iter()
        .find_map(|level| nearest(&mut level.iter().filter(in_middle).copied()))
        .or_else(|| {
            found
                .iter()
                .find_map(|level| nearest(&mut level.iter().copied()))
        })
        .or_else(|| {
            if !allow_any_char {
                return None;
            }
            nearest(
                &mut s
                    .char_indices()
                    .skip(1)
                    .filter_map(|(at, _)| Some((at, valid(at..at)?))),
            )
        })
}

/// Moves whitespace at the inner edges of both pieces into the separator.
fn retrim(s: &str, separator: Range<usize>) -> Cut {
    Cut {
        left_end: s[..separator.start].trim_end().len(),
        right_start: s.len() - s[separator.end..].trim_start().len(),
    }
}

fn overlaps_guard(guarded: &[usize], cut: Cut) -> bool {
    let next = guarded.partition_point(|&g| g < cut.left_end);
    guarded.get(next).is_some_and(|&g| g < cut.right_start)
}

fn cuts_inside(range: &Range<usize>, cut: Cut) -> bool {
    cut.left_end < range.end && range.start < cut.right_start
}

/// Whether the cut lies inside one of `lines` (sorted line spans without their line
/// breaks), so that both pieces would share that line.
fn within_line(lines: &[Range<usize>], cut: Cut) -> bool {
    let next = lines.partition_point(|line| line.end < cut.left_end);
    lines
        .get(next)
        .is_some_and(|line| line.start < cut.left_end && cut.right_start <= line.end)
}

fn splits_rewrite(rewrites: &Rewrites, cut: Cut) -> bool {
    let next = rewrites.partition_point(|(r, _)| r.end <= cut.left_end);
    rewrites
        .get(next)
        .is_some_and(|(r, _)| r.start < cut.right_start)
}

/// Line breaks that end a table header line or a delimiter row; cutting there would
/// separate a header from its delimiter row or the delimiter row from the body.
fn table_guarded_newlines(s: &str) -> Vec<usize> {
    let mut guarded = Vec::new();
    let mut previous_newline = None;
    let mut pos = 0;
    for line in s.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let newline = line.ends_with('\n').then_some(pos + content.len());
        if is_delimiter_row(content) {
            guarded.extend(previous_newline);
            guarded.extend(newline);
        }
        previous_newline = newline;
        pos += line.len();
    }
    guarded.dedup();
    guarded
}

/// From the first non-blank character of the line before each delimiter row to the
/// end of the delimiter row (its line break excluded): no cut may fall inside, so a
/// table header stays whole and with its delimiter row.
fn table_header_rows(s: &str) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let mut previous_content_start = None;
    let mut pos = 0;
    for line in s.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content_start = pos + (content.len() - content.trim_start().len());
        if is_delimiter_row(content) {
            rows.push(previous_content_start.unwrap_or(content_start)..pos + content.len());
        }
        previous_content_start = Some(content_start);
        pos += line.len();
    }
    rows
}

/// Table rows: lines whose content after `[ \t>]*` starts with `|`, and every line of
/// a table region (the header line, the delimiter row and the non-blank lines after
/// it), which need no leading `|`. A table is cut only between rows, since a row split
/// across two pieces can no longer be checked or rejoined.
fn table_row_lines(s: &str) -> Vec<Range<usize>> {
    let mut rows: Vec<Range<usize>> = Vec::new();
    let mut previous: Option<Range<usize>> = None;
    let mut in_table = false;
    let mut pos = 0;
    for line in s.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let span = pos..pos + content.len();
        let blank = is_blank_or_quote_only(content);
        if is_delimiter_row(content) {
            in_table = true;
            if let Some(header) = previous.take()
                && rows.last() != Some(&header)
            {
                rows.push(header);
            }
        } else if blank {
            in_table = false;
        }
        let pipe_row = content
            .trim_start_matches([' ', '\t', '>'])
            .starts_with('|');
        if in_table || pipe_row {
            rows.push(span.clone());
        }
        previous = (!blank).then_some(span);
        pos += line.len();
    }
    rows
}

fn newline_positions(s: &str) -> impl Iterator<Item = usize> + '_ {
    s.match_indices('\n').map(|(at, _)| at)
}

fn line_at(s: &str, start: usize) -> &str {
    s[start..].split('\n').next().unwrap_or("")
}

fn is_blank_or_quote_only(line: &str) -> bool {
    line.chars().all(|c| c.is_whitespace() || c == '>')
}

/// A `\n` followed by lines of only whitespace and `>`, up to the next other line.
fn blank_line_runs(s: &str) -> Vec<Range<usize>> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(newline_positions(s).map(|at| at + 1))
        .collect();
    // One backward pass: for each line, the start of the first line at or after it
    // that is not a complete blank line, and whether that line has other content.
    let mut stop = vec![(s.len(), false); line_starts.len()];
    let mut runs = Vec::new();
    for k in (0..line_starts.len()).rev() {
        let start = line_starts[k];
        let complete_blank = line_starts
            .get(k + 1)
            .is_some_and(|&next| is_blank_or_quote_only(&s[start..next - 1]));
        stop[k] = if complete_blank {
            stop[k + 1]
        } else {
            (start, !is_blank_or_quote_only(line_at(s, start)))
        };
        let (next, other) = stop[k];
        if k > 0 && complete_blank && other {
            runs.push(start - 1..next);
        }
    }
    runs.reverse();
    runs
}

/// A `\n` before a line matching `^[ \t>]*([-*+]|\d{1,9}[.)])[ \t]` or `^[ \t>]*\|`.
fn list_or_table_lines(s: &str) -> Vec<Range<usize>> {
    newline_positions(s)
        .filter(|&at| starts_list_item_or_table_row(line_at(s, at + 1)))
        .map(|at| at..at + 1)
        .collect()
}

fn starts_list_item_or_table_row(line: &str) -> bool {
    let rest = line.trim_start_matches([' ', '\t', '>']);
    if rest.starts_with('|') {
        return true;
    }
    let after_marker = if let Some(after) = rest.strip_prefix(['-', '*', '+']) {
        after
    } else {
        let after = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        let digits = rest.len() - after.len();
        if !(1..=9).contains(&digits) {
            return false;
        }
        match after.strip_prefix(['.', ')']) {
            Some(after) => after,
            None => return false,
        }
    };
    after_marker.starts_with([' ', '\t'])
}

fn newlines(s: &str) -> Vec<Range<usize>> {
    newline_positions(s).map(|at| at..at + 1).collect()
}

/// `[.!?]` plus closers and a required whitespace run, or `[。！？]` plus closers and
/// an optional whitespace run; the run is the separator.
fn sentence_ends(s: &str) -> Vec<Range<usize>> {
    const LATIN_CLOSERS: &[char] = &['"', '\'', ')', ']', '\u{2019}', '\u{201D}'];
    const CJK_CLOSERS: &[char] = &['\u{300D}', '\u{300F}', '\u{FF09}', '\u{201D}'];
    s.char_indices()
        .filter_map(|(at, c)| {
            let (closers, needs_space) = match c {
                '.' | '!' | '?' => (LATIN_CLOSERS, true),
                '\u{3002}' | '\u{FF01}' | '\u{FF1F}' => (CJK_CLOSERS, false),
                _ => return None,
            };
            let rest = s[at + c.len_utf8()..].trim_start_matches(closers);
            let start = s.len() - rest.len();
            let end = s.len() - rest.trim_start().len();
            (end > start || !needs_space).then_some(start..end)
        })
        .collect()
}

fn whitespace_runs(s: &str) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut run_start = None;
    for (at, c) in s.char_indices() {
        match (c.is_whitespace(), run_start) {
            (true, None) => run_start = Some(at),
            (false, Some(start)) => {
                runs.push(start..at);
                run_start = None;
            }
            _ => {}
        }
    }
    runs.extend(run_start.map(|start| start..s.len()));
    runs
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::{S, check_invariants, check_unit, hole_ids, shape, units};
    use super::super::{assemble, split, split_unit};
    use super::*;

    /// Checks round-trip, shape and hole ids for one cut and returns the pieces.
    pub(in super::super) fn split_unit_checked(unit: &Unit) -> Option<Vec<Segment>> {
        let segments = split_unit(unit)?;
        let joined: String = segments.iter().map(Segment::source).collect();
        assert_eq!(joined, unit.source(), "pieces must reassemble the unit");
        assert!(matches!(segments.first(), Some(Segment::Translate(_))));
        assert!(matches!(segments.last(), Some(Segment::Translate(_))));
        let mut ids = Vec::new();
        for segment in &segments {
            match segment {
                Segment::Keep(k) => assert!(!k.is_empty(), "empty keep"),
                Segment::Translate(piece) => {
                    check_unit(piece);
                    assert_eq!(piece.format, unit.format);
                    ids.extend(hole_ids(piece));
                }
            }
        }
        // A unit with holes is cut at one of them; that hole becomes the Keep and every
        // other hole keeps its id.
        let mut expected = hole_ids(unit);
        if !expected.is_empty() {
            let Some(Segment::Keep(separator)) = segments.get(1) else {
                panic!("a cut at a hole has a Keep: {segments:?}");
            };
            let left = units(&segments)[0];
            let cut = expected.remove(hole_ids(left).len());
            assert!(separator.contains(&*hole_body(unit, cut)));
        }
        assert_eq!(ids, expected, "hole ids must be preserved");
        Some(segments)
    }

    fn hole_body(unit: &Unit, wanted: u32) -> String {
        unit.parts
            .iter()
            .find_map(|p| match p {
                Part::Hole { id, body, .. } if *id == wanted => Some(body.clone()),
                _ => None,
            })
            .unwrap()
    }

    fn assert_cut(unit: &Unit, expected: Option<&[S<'_>]>) {
        match (split_unit_checked(unit), expected) {
            (Some(segments), Some(expected)) => assert_eq!(shape(&segments), expected),
            (None, None) => {}
            (got, expected) => panic!("expected {expected:?}, got {got:?}"),
        }
    }

    fn text_unit(input: &str) -> Unit {
        let split = split(input, Format::Text, &Limits::default());
        let units = units(&split.segments);
        assert_eq!(units.len(), 1, "{:?}", split.segments);
        units[0].clone()
    }

    fn cand(range: Range<usize>, rewrites: Vec<(Range<usize>, String)>) -> Cand {
        Cand {
            range,
            rewrites,
            heading: false,
            no_merge: false,
        }
    }

    /// A unit built from the given translatable slices of `input`, with everything
    /// between them verbatim.
    fn unit_of(input: &str, format: Format, texts: &[&str]) -> Unit {
        let mut from = 0;
        let cands: Vec<Cand> = texts
            .iter()
            .map(|text| {
                let start = from + input[from..].find(text).unwrap();
                from = start + text.len();
                cand(start..from, Vec::new())
            })
            .collect();
        let segments = assemble(input, format, &cands, &Limits::default());
        let units = units(&segments);
        assert_eq!(units.len(), 1);
        units[0].clone()
    }

    fn split_text_checked(input: &str, limits: &Limits) -> Vec<Segment> {
        let split = split(input, Format::Text, limits);
        check_invariants(input, &split, limits);
        split.segments
    }

    #[test]
    fn oversize_paragraph_is_cut_at_sentence_ends() {
        let sentence = format!("{}.", "a".repeat(999));
        let input = vec![sentence; 10].join(" ");
        let segments = split_text_checked(&input, &Limits::default());
        assert!(units(&segments).len() >= 2);
        for segment in &segments {
            match segment {
                Segment::Translate(unit) => {
                    assert!(unit.text().len() <= 6000);
                    assert!(unit.source().ends_with('.'));
                }
                Segment::Keep(k) => assert_eq!(k, " "),
            }
        }
    }

    #[test]
    fn list_is_cut_between_items() {
        let input = "- a\n- b\n- c\n- d";
        let unit = unit_of(input, Format::Markdown, &[input]);
        assert_cut(
            &unit,
            Some(&[S::T("- a\n- b"), S::K("\n"), S::T("- c\n- d")]),
        );
    }

    #[test]
    fn table_is_not_cut_around_the_delimiter_row() {
        let input = "| h |\n|---|\n| a |\n| b |";
        let unit = unit_of(input, Format::Markdown, &[input]);
        assert_cut(
            &unit,
            Some(&[S::T("| h |\n|---|\n| a |"), S::K("\n"), S::T("| b |")]),
        );
    }

    #[test]
    fn sentences_and_uncuttable_words() {
        assert_cut(
            &text_unit("One. Two."),
            Some(&[S::T("One."), S::K(" "), S::T("Two.")]),
        );
        assert_cut(
            &text_unit("一文目。二文目。"),
            Some(&[S::T("一文目。"), S::T("二文目。")]),
        );
        assert_cut(&text_unit("Hello"), None);
    }

    #[test]
    fn units_with_holes_are_cut_at_the_hole() {
        let input = "- step:\n  ```sh\n  cargo build\n  ```\n- next";
        let unit = unit_of(input, Format::Markdown, &["- step:", "- next"]);
        assert_eq!(unit.text(), "- step:\n  <!--pt:k1-->\n- next");
        assert_cut(
            &unit,
            Some(&[
                S::T("- step:"),
                S::K("\n  ```sh\n  cargo build\n  ```\n"),
                S::T("- next"),
            ]),
        );
        let input = "> text\n> ```\n> code\n> ```\n> more";
        let unit = unit_of(input, Format::Markdown, &["> text", "> more"]);
        assert_eq!(unit.text(), "> text\n> <!--pt:k1-->\n> more");
        assert_cut(
            &unit,
            Some(&[
                S::T("> text"),
                S::K("\n> ```\n> code\n> ```\n"),
                S::T("> more"),
            ]),
        );
    }

    #[test]
    fn unbroken_text_is_cut_anywhere_only_when_oversize() {
        let input = "x".repeat(7000);
        let segments = split_text_checked(&input, &Limits::default());
        assert!(segments.len() >= 2);
        for segment in &segments {
            let Segment::Translate(unit) = segment else {
                panic!("unexpected keep: {segments:?}");
            };
            assert!(unit.text().len() <= 6000);
        }
        assert_cut(&text_unit(&"x".repeat(100)), None);
    }

    #[test]
    fn hole_nearest_the_middle_of_the_sent_text_is_chosen() {
        let unit = text_unit("aaaa\n\n--\n\nbbbb\n\n--\n\ncccc");
        assert_eq!(
            unit.text(),
            "aaaa\n\n<!--pt:k1-->\n\nbbbb\n\n<!--pt:k2-->\n\ncccc"
        );
        let segments = split_unit_checked(&unit).unwrap();
        assert_eq!(
            shape(&segments),
            [S::T("aaaa\n\n--\n\nbbbb"), S::K("\n\n--\n\n"), S::T("cccc")]
        );
        let pieces = units(&segments);
        assert_eq!(hole_ids(pieces[0]), [1]);
        assert_eq!(pieces[0].text(), "aaaa\n\n<!--pt:k1-->\n\nbbbb");
    }

    #[test]
    fn blank_lines_win_over_finer_boundaries_in_the_middle_half() {
        assert_cut(
            &text_unit("Aaa aaa.\nBbb bbb.\n\nCcc ccc.\nDdd"),
            Some(&[
                S::T("Aaa aaa.\nBbb bbb."),
                S::K("\n\n"),
                S::T("Ccc ccc.\nDdd"),
            ]),
        );
        assert_cut(
            &text_unit("Aaa aaa.\n>\n> Bbb"),
            Some(&[S::T("Aaa aaa."), S::K("\n>\n"), S::T("> Bbb")]),
        );
    }

    #[test]
    fn finer_level_in_the_middle_beats_coarser_level_at_the_edge() {
        // The only line break sits in the first quarter; a sentence end is central.
        assert_cut(
            &text_unit("Hi\nfirst sentence. second sentence here"),
            Some(&[
                S::T("Hi\nfirst sentence."),
                S::K(" "),
                S::T("second sentence here"),
            ]),
        );
    }

    #[test]
    fn coarsest_level_is_used_when_nothing_is_in_the_middle() {
        // A line break at the edge beats a sentence end at the other edge.
        assert_cut(
            &text_unit("a\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb. c"),
            Some(&[
                S::T("a"),
                S::K("\n"),
                S::T("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb. c"),
            ]),
        );
    }

    #[test]
    fn nested_list_items_and_crlf_move_whitespace_into_the_separator() {
        let unit = text_unit("- a\r\n  - b\r\n- c\r\n  - d");
        let segments = split_unit_checked(&unit).unwrap();
        assert_eq!(
            shape(&segments),
            [S::T("- a\r\n  - b"), S::K("\r\n"), S::T("- c\r\n  - d")]
        );
        assert!(units(&segments).iter().all(|p| p.crlf));
        let unit = text_unit("aaaa\r\nbbbb");
        let segments = split_unit_checked(&unit).unwrap();
        assert_eq!(shape(&segments), [S::T("aaaa"), S::K("\r\n"), S::T("bbbb")]);
        assert!(units(&segments).iter().all(|p| !p.crlf));
    }

    #[test]
    fn sentence_closers_stay_with_the_sentence() {
        assert_cut(
            &text_unit("He said \"Go.\" Then left."),
            Some(&[S::T("He said \"Go.\""), S::K(" "), S::T("Then left.")]),
        );
        assert_cut(
            &text_unit("「はい。」と言った。その後、帰った。"),
            Some(&[S::T("「はい。」と言った。"), S::T("その後、帰った。")]),
        );
    }

    #[test]
    fn table_guard_skips_breaks_around_the_delimiter_row() {
        let lines = ["| a | b |", "|---|---|", "| 1 | 2 |"];
        assert_eq!(table_guarded_newlines(&lines.join("\n")), [9, 19]);
        assert_eq!(table_guarded_newlines("|-|\n|-|\n|-|"), [3, 7]);
        // Both line breaks are guarded and there is no other boundary.
        assert_cut(&text_unit("|hh|\n|--|\n|x|"), None);
    }

    #[test]
    fn table_header_line_and_delimiter_row_are_never_cut_inside() {
        let input = "| hh hh |\n|--|\n| x |";
        let unit = unit_of(input, Format::Markdown, &[input]);
        if let Some(segments) = split_unit_checked(&unit) {
            let left = units(&segments)[0];
            assert!(
                left.source().starts_with("| hh hh |\n|--|\n"),
                "{segments:?}"
            );
        }
        assert_eq!(table_header_rows(input), vec![0..14]);
        assert_eq!(
            table_header_rows("|-|\nx\n  | a b |\n | - |"),
            [0..3, 8..22]
        );
        // Character cuts of oversize candidates respect the same rule.
        let input = "| hhhhhhhhhh |\n|--|\n| x |";
        let pieces = split_oversize(
            input,
            vec![cand(0..input.len(), Vec::new())],
            &Limits {
                target_bytes: 5,
                max_unit_bytes: 5,
                max_placeholders: 16,
            },
        );
        assert!(
            pieces
                .iter()
                .all(|c| c.range.start == 0 || c.range.start >= 20),
            "{pieces:?}"
        );
    }

    #[test]
    fn table_rows_are_never_cut_inside() {
        let input = "| Name | Value |\n|---|---|\n| foo bar | baz qux |";
        let unit = unit_of(input, Format::Markdown, &[input]);
        assert_cut(&unit, None);
        assert_eq!(
            table_row_lines("| a |\n> | b |\r\ntext | c\n  |"),
            [0..5, 6..14, 24..27]
        );
    }

    #[test]
    fn tables_without_leading_pipes_are_never_cut_inside_a_row() {
        let input = "Name | Value\n---|---\nfoo bar | baz qux";
        let unit = unit_of(input, Format::Markdown, &[input]);
        assert_cut(&unit, None);
        assert_eq!(table_row_lines(input), [0..12, 13..20, 21..38]);

        // The region ends at a blank line; a later paragraph can be cut anywhere.
        let input = "a | b\n> -|-\n> x y | z\n>\nplain text\n| p |";
        assert_eq!(table_row_lines(input), [0..5, 6..11, 12..21, 35..40]);

        let input = "Intro words.\n\nName | Value\n---|---\nfoo bar | baz qux\nnext row | here";
        let unit = unit_of(input, Format::Markdown, &[input]);
        let rows = table_row_lines(input);
        let segments = split_unit_checked(&unit).expect("a cut");
        let left_end = units(&segments)[0].source().len();
        assert!(
            rows.iter()
                .all(|r| left_end <= r.start || r.end <= left_end),
            "{segments:?}"
        );
    }

    #[test]
    fn blank_line_runs_span_to_the_next_other_line() {
        assert_eq!(blank_line_runs("a\n\n>\n \nb"), [1..7, 2..7, 4..7]);
        assert_eq!(blank_line_runs("a\n\n"), Vec::<Range<usize>>::new());
        assert_eq!(blank_line_runs("a\n\n\n"), Vec::<Range<usize>>::new());
        assert_eq!(blank_line_runs("a\nb\n\r\nc"), vec![3..6]);
        assert_eq!(blank_line_runs("\n\nx"), vec![0..2]);
    }

    #[test]
    fn rewrite_spans_are_never_cut_and_follow_their_piece() {
        let input = "Go [a b c d e f][r] then [g][r].";
        let original = cand(
            0..input.len(),
            vec![
                (3..19, "[a b c d e f](http://x)".to_owned()),
                (25..31, "[g](http://x)".to_owned()),
            ],
        );
        let segments = assemble(
            input,
            Format::Markdown,
            std::slice::from_ref(&original),
            &Limits::default(),
        );
        let unit = units(&segments)[0].clone();
        // The spaces inside `[a b c d e f]` are as near the middle, but lie inside a
        // rewrite.
        let segments = split_unit_checked(&unit).unwrap();
        assert_eq!(
            shape(&segments),
            [S::T("Go [a b c d e f][r]"), S::K(" "), S::T("then [g][r].")]
        );
        let pieces = units(&segments);
        assert_eq!(pieces[0].text(), "Go [a b c d e f](http://x)");
        assert_eq!(pieces[1].text(), "then [g](http://x).");

        let limits = Limits {
            max_unit_bytes: 30,
            ..Limits::default()
        };
        let pieces = split_oversize(input, vec![original], &limits);
        let ranges: Vec<_> = pieces.iter().map(|c| c.range.clone()).collect();
        assert_eq!(ranges, [0..19, 20..32]);
        assert!(pieces.iter().all(|c| c.no_merge && !c.heading));
        assert_eq!(
            pieces[0].rewrites,
            [(3..19, "[a b c d e f](http://x)".to_owned())]
        );
        assert_eq!(pieces[1].rewrites, [(25..31, "[g](http://x)".to_owned())]);
    }

    #[test]
    fn split_unit_never_cuts_inside_a_word_or_off_symbols() {
        assert_cut(&text_unit("supercalifragilistic"), None);
        assert_cut(&text_unit("ab --"), None);
    }

    #[test]
    fn oversize_pieces_stay_unmerged() {
        let limits = Limits {
            target_bytes: 1000,
            max_unit_bytes: 10,
            max_placeholders: 16,
        };
        let input = "short\n\nfive words in this one\n\nend\n";
        let segments = split_text_checked(input, &limits);
        assert_eq!(
            shape(&segments),
            [
                S::T("short"),
                S::K("\n\n"),
                S::T("five words"),
                S::K(" "),
                S::T("in this"),
                S::K(" "),
                S::T("one"),
                S::K("\n\n"),
                S::T("end"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn character_cuts_respect_char_boundaries() {
        let input = "あ".repeat(3000);
        let segments = split_text_checked(&input, &Limits::default());
        assert_eq!(
            shape(&segments),
            [S::T(&input[..4500]), S::T(&input[4500..])]
        );
    }
}
