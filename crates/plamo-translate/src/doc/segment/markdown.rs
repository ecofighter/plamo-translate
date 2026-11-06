//! Markdown splitting. pulldown-cmark supplies only block ranges and link metadata;
//! every byte that is kept or sent is a slice of the input.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag};

use std::collections::HashSet;

use super::{BOM, Builder, Note, NoteKind, front_matter_len};

type Rewrite = (Range<usize>, String);

pub(super) fn split(input: &str, builder: &mut Builder) {
    let prefix = prefix_len(input);
    let events: Vec<(Event, Range<usize>)> = Parser::new_ext(&input[prefix..], options())
        .into_offset_iter()
        .map(|(event, range)| (event, range.start + prefix..range.end + prefix))
        .collect();
    let doc = Doc {
        input,
        ends: matching_ends(&events),
        events,
    };
    let mut cursor = prefix;
    let mut at = 0;
    while at < doc.events.len() {
        let range = doc.events[at].1.clone();
        let start = range.start.max(cursor);
        let end = range.end.max(start);
        let first_note = builder.notes.len();
        let translatable = match &doc.events[at].0 {
            Event::Start(_) => match doc.verbatim(at) {
                Some(note) => {
                    if let Some(kind) = note {
                        builder.note(range.start, kind);
                    }
                    false
                }
                None => true,
            },
            _ => false,
        };
        if translatable && end > start {
            doc.translatable_block(at, start..end, builder);
        }
        // Identical notes can only come from one depth-0 block (its reserved-marker
        // slices all report the block's start line).
        dedup_from(&mut builder.notes, first_note);
        cursor = cursor.max(end);
        at = doc.ends[at] + 1;
    }
}

/// The BOM and front matter, which stay verbatim and are not parsed.
fn prefix_len(input: &str) -> usize {
    let bom = if input.starts_with(BOM) {
        BOM.len_utf8()
    } else {
        0
    };
    bom + front_matter_len(&input[bom..]).unwrap_or(0)
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
}

/// For each `Start` event the index of its `End`; every other event maps to itself.
fn matching_ends(events: &[(Event, Range<usize>)]) -> Vec<usize> {
    let mut ends: Vec<usize> = (0..events.len()).collect();
    let mut open = Vec::new();
    for (index, (event, _)) in events.iter().enumerate() {
        match event {
            Event::Start(_) => open.push(index),
            Event::End(_) => {
                let start = open.pop().expect("balanced events");
                ends[start] = index;
            }
            _ => {}
        }
    }
    ends
}

struct Doc<'a> {
    input: &'a str,
    events: Vec<(Event<'a>, Range<usize>)>,
    ends: Vec<usize>,
}

impl Doc<'_> {
    /// `Some` if the block starting at event `at` is never sent, with the note it
    /// raises if any.
    fn verbatim(&self, at: usize) -> Option<Option<NoteKind>> {
        let Event::Start(tag) = &self.events[at].0 else {
            return Some(None);
        };
        match tag {
            Tag::CodeBlock(CodeBlockKind::Indented) => Some(Some(NoteKind::IndentedCode)),
            Tag::CodeBlock(CodeBlockKind::Fenced(_)) => Some(None),
            Tag::HtmlBlock => {
                let html = &self.input[self.events[at].1.clone()];
                Some(html_has_text(html).then_some(NoteKind::HtmlText))
            }
            Tag::Paragraph if self.math_only(at) => Some(None),
            _ => None,
        }
    }

    fn math_only(&self, at: usize) -> bool {
        self.events[at + 1..self.ends[at]]
            .iter()
            .all(|(event, _)| match event {
                Event::DisplayMath(_) | Event::SoftBreak | Event::HardBreak => true,
                Event::Text(text) => text.chars().all(char::is_whitespace),
                _ => false,
            })
    }

    /// Sends a depth-0 block raw, with nested verbatim blocks and lines no child block
    /// covers cut out as holes, and reference-style links rewritten inline.
    ///
    /// Verbatim blocks become holes and their bodies are never sent: the model translates
    /// comments inside code, and a placeholder keeps the surrounding prose in one request.
    /// Reference links are sent inline because the model translates link labels
    /// (`[guide][setup]` became `[ガイド][セットアップ]`), which breaks reference links.
    fn translatable_block(&self, at: usize, block: Range<usize>, builder: &mut Builder) {
        let heading = matches!(self.events[at].0, Event::Start(Tag::Heading { .. }));
        // Taken before the nested notes so that line lookups only move forward.
        let block_line = builder.line_at(block.start);
        let mut holes = self.uncovered_lines(at, &block);
        let mut rewrites = Vec::new();
        let mut inner = at + 1;
        while inner < self.ends[at] {
            if matches!(self.events[inner].0, Event::Start(_)) {
                if let Some(note) = self.verbatim(inner) {
                    let range = self.events[inner].1.clone();
                    if let Some(kind) = note {
                        builder.note(range.start, kind);
                    }
                    holes.extend(self.hole(range, &block));
                    inner = self.ends[inner] + 1;
                    continue;
                }
                if let Some(rewrite) = self.rewrite(inner) {
                    rewrites.push(rewrite);
                    // The replacement already carries the link text verbatim, so
                    // nested links (an image inside a link) are left alone.
                    inner = self.ends[inner] + 1;
                    continue;
                }
                holes.extend(self.uncovered_lines(inner, &block));
            }
            inner += 1;
        }
        let mut text_start = block.start;
        for hole in union(holes) {
            self.text_slice(
                text_start..hole.start,
                &rewrites,
                heading,
                block_line,
                builder,
            );
            text_start = hole.end;
        }
        self.text_slice(
            text_start..block.end,
            &rewrites,
            heading,
            block_line,
            builder,
        );
    }

    fn text_slice(
        &self,
        range: Range<usize>,
        rewrites: &[Rewrite],
        heading: bool,
        block_line: usize,
        builder: &mut Builder,
    ) {
        if range.is_empty() {
            return;
        }
        let inside = rewrites
            .iter()
            .filter(|(r, _)| range.start <= r.start && r.end <= range.end)
            .cloned()
            .collect();
        builder.candidate(range, inside, heading, block_line);
    }

    /// Widens a nested verbatim range to whole lines within `block`.
    fn hole(&self, range: Range<usize>, block: &Range<usize>) -> Option<Range<usize>> {
        let start = range.start.clamp(block.start, block.end);
        let end = range.end.clamp(start, block.end);
        let start = self.input[block.start..start]
            .rfind('\n')
            .map_or(block.start, |at| block.start + at + 1);
        let ends_line = end > block.start && self.input.as_bytes()[end - 1] == b'\n';
        let end = if ends_line {
            end
        } else {
            self.input[end..block.end]
                .find('\n')
                .map_or(block.end, |at| end + at)
        };
        (start < end).then_some(start..end)
    }

    /// Holes for the lines of a quote, list item or footnote definition at event `at`
    /// whose content no child event covers. pulldown-cmark emits nothing for link
    /// reference definitions, so without this they would be sent as prose.
    fn uncovered_lines(&self, at: usize, block: &Range<usize>) -> Vec<Range<usize>> {
        let Event::Start(Tag::BlockQuote(_) | Tag::Item | Tag::FootnoteDefinition(_)) =
            &self.events[at].0
        else {
            return Vec::new();
        };
        let mut children = Vec::new();
        let mut child = at + 1;
        while child < self.ends[at] {
            children.push(self.events[child].1.clone());
            child = self.ends[child] + 1;
        }
        let range = &self.events[at].1;
        let end = range.end.min(block.end);
        let mut holes = Vec::new();
        let mut line_start = range.start.max(block.start);
        while line_start < end {
            let line_end = self.input[line_start..end]
                .find('\n')
                .map_or(end, |at| line_start + at);
            let line = &self.input[line_start..line_end];
            let content_start = line_end - strip_container_markers(line).len();
            let content_end = line_start + line.trim_end().len();
            if content_start < content_end {
                let next = children.partition_point(|r| r.end <= content_start);
                let covered = children.get(next).is_some_and(|r| r.start < content_end);
                if !covered {
                    holes.extend(self.hole(content_start..content_end, block));
                }
            }
            line_start = line_end + 1;
        }
        holes
    }

    /// The inline form of a reference, collapsed or shortcut link or image starting at
    /// event `at`, as a replacement for its whole source span.
    fn rewrite(&self, at: usize) -> Option<Rewrite> {
        let (Event::Start(tag), span) = &self.events[at] else {
            return None;
        };
        let (opener, link_type, dest_url, title) = match tag {
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            } => ("[", link_type, dest_url, title),
            Tag::Image {
                link_type,
                dest_url,
                title,
                ..
            } => ("![", link_type, dest_url, title),
            _ => return None,
        };
        if !matches!(
            link_type,
            LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut
        ) {
            return None;
        }
        // Text from a definition elsewhere must not bring markers or reserved text
        // into what is sent.
        if dest_url.contains(['<', '>', '\n']) || title.contains(['<', '\n']) {
            return None;
        }
        // A backslash in the title could escape the closing quote, and a `|` would
        // split a table cell.
        if title.contains('\\') || dest_url.contains('|') || title.contains('|') {
            return None;
        }
        let mut span = span.clone();
        if *link_type == LinkType::Collapsed && self.input[span.end..].starts_with("[]") {
            span.end += 2;
        }
        let text_start = span.start + opener.len();
        let text_end = self.events[at + 1..self.ends[at]]
            .iter()
            .map(|(_, r)| r.end)
            .max()?;
        if text_end < text_start || text_end > span.end {
            return None;
        }
        let dest = if dest_url.is_empty()
            || dest_url.contains(|c: char| c.is_whitespace() || c == '(' || c == ')')
        {
            format!("<{dest_url}>")
        } else {
            dest_url.to_string()
        };
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", title.replace('"', "\\\""))
        };
        let text = &self.input[text_start..text_end];
        Some((span, format!("{opener}{text}]({dest}{title})")))
    }
}

/// Sorts ranges and merges overlapping ones.
fn union(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

/// The rest of `line` after quote markers, list markers and indentation.
fn strip_container_markers(line: &str) -> &str {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t', '>']);
        let after_marker = match trimmed.strip_prefix(['-', '*', '+']) {
            Some(after) => Some(after),
            None => {
                let after = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
                let digits = trimmed.len() - after.len();
                if (1..=9).contains(&digits) {
                    after.strip_prefix(['.', ')'])
                } else {
                    None
                }
            }
        };
        rest = match after_marker {
            Some(after) if after.is_empty() || after.starts_with(char::is_whitespace) => after,
            _ => return trimmed,
        };
    }
}

fn dedup_from(notes: &mut Vec<Note>, from: usize) {
    if notes.len() - from < 2 {
        return;
    }
    let mut seen = HashSet::new();
    let tail = notes.split_off(from);
    notes.extend(tail.into_iter().filter(|note| seen.insert(note.clone())));
}

/// Whether an HTML block carries visible text: not a comment, and something
/// alphanumeric is left once quote prefixes and every `<…>` span are removed.
fn html_has_text(html: &str) -> bool {
    let stripped: Vec<&str> = html
        .lines()
        .map(|line| line.trim_start_matches([' ', '>']))
        .collect();
    if stripped.first().is_some_and(|l| l.starts_with("<!--")) {
        return false;
    }
    let joined = stripped.join("\n");
    let mut rest = joined.as_str();
    let mut outside = String::new();
    while let Some(open) = rest.find('<') {
        outside.push_str(&rest[..open]);
        rest = match rest[open..].find('>') {
            Some(close) => &rest[open + close + 1..],
            None => "",
        };
    }
    outside.push_str(rest);
    outside.chars().any(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::super::cut::tests::split_unit_checked;
    use super::super::tests::{S, check_invariants, hole_ids, shape, units};
    use super::super::{Format, Limits, Note, NoteKind, Part, Segment, Split, Unit, split};

    fn md(input: &str, limits: &Limits) -> Split {
        let split = split(input, Format::Markdown, limits);
        check_invariants(input, &split, limits);
        split
    }

    fn target(bytes: usize) -> Limits {
        Limits {
            target_bytes: bytes,
            ..Limits::default()
        }
    }

    fn only_unit(split: &Split) -> &Unit {
        let units = units(&split.segments);
        assert_eq!(units.len(), 1, "{:?}", split.segments);
        units[0]
    }

    fn note(line: usize, kind: NoteKind) -> Note {
        Note { line, kind }
    }

    fn rewrite_count(unit: &Unit) -> usize {
        unit.parts
            .iter()
            .map(|p| match p {
                Part::Text { rewrites, .. } => rewrites.len(),
                Part::Hole { .. } => 0,
            })
            .sum()
    }

    const ROW_6: &str = "Intro\n\n```rs\nlet x = 1;\n```\n\nOutro\n";
    const ROW_9: &str = "- step:\n  ```sh\n  cargo build\n  ```\n- next\n";
    const ROW_11: &str = "> text\n> ```\n> code\n> ```\n> more\n";

    #[test]
    fn code_block_between_paragraphs_becomes_a_hole() {
        let split = md(ROW_6, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::T(&ROW_6[..ROW_6.len() - 1]), S::K("\n")]
        );
        let unit = only_unit(&split);
        assert_eq!(unit.text(), "Intro\n\n<!--pt:k1-->\n\nOutro");
        assert!(split.notes.is_empty());
    }

    #[test]
    fn code_block_is_kept_between_separate_units() {
        let split = md(ROW_6, &target(10));
        assert_eq!(
            shape(&split.segments),
            [
                S::T("Intro"),
                S::K("\n\n```rs\nlet x = 1;\n```\n\n"),
                S::T("Outro"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn code_block_inside_a_list_item() {
        let split = md(ROW_9, &Limits::default());
        let unit = only_unit(&split);
        assert_eq!(unit.text(), "- step:\n  <!--pt:k1-->\n- next");
        assert_eq!(unit.source(), &ROW_9[..ROW_9.len() - 1]);
        let done = unit.finish("- 手順:<!--pt:k1--> - 次へ").unwrap();
        assert_eq!(done.text, "- 手順:\n  ```sh\n  cargo build\n  ```\n- 次へ");
        assert_eq!(done.issues, []);
    }

    #[test]
    fn list_item_code_block_is_kept_between_units() {
        let split = md(ROW_9, &target(5));
        assert_eq!(
            shape(&split.segments),
            [
                S::T("- step:"),
                S::K("\n  ```sh\n  cargo build\n  ```\n"),
                S::T("- next"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn code_block_inside_a_quote_keeps_the_quote_prefix() {
        let split = md(ROW_11, &Limits::default());
        let unit = only_unit(&split);
        assert_eq!(unit.text(), "> text\n> <!--pt:k1-->\n> more");
        for raw in [
            "> テキスト\n> <!--pt:k1-->\n> 続き",
            "> テキスト\n<!--pt:k1-->\n> 続き",
        ] {
            let done = unit.finish(raw).unwrap();
            assert_eq!(done.text, "> テキスト\n> ```\n> code\n> ```\n> 続き");
            assert_eq!(done.issues, [], "{raw:?}");
        }
    }

    #[test]
    fn quoted_code_block_is_kept_between_units() {
        let split = md(ROW_11, &target(5));
        assert_eq!(
            shape(&split.segments),
            [
                S::T("> text"),
                S::K("\n> ```\n> code\n> ```\n"),
                S::T("> more"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn front_matter_is_kept() {
        let split = md("---\ntitle: x\n---\n\n# T\n", &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::K("---\ntitle: x\n---\n\n"), S::T("# T"), S::K("\n")]
        );
    }

    #[test]
    fn dashes_later_in_the_document_are_not_front_matter() {
        let split = md("Intro\n\n---\nfoo: bar\n---\n", &target(5));
        assert_eq!(
            shape(&split.segments),
            [
                S::T("Intro"),
                S::K("\n\n---\n"),
                S::T("foo: bar\n---"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn display_math_paragraph_is_kept_and_inline_math_is_sent() {
        let split = md("$$\nx^2\n$$\n\nEnergy $E=mc^2$ here.\n\n$x$\n", &target(25));
        assert_eq!(
            shape(&split.segments),
            [
                S::K("$$\nx^2\n$$\n\n"),
                S::T("Energy $E=mc^2$ here."),
                S::K("\n\n"),
                S::T("$x$"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn indented_code_is_kept_with_a_note() {
        let split = md("Para\n\n    code\n", &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::T("Para"), S::K("\n\n    code\n")]
        );
        assert_eq!(split.notes, [note(3, NoteKind::IndentedCode)]);
    }

    #[test]
    fn html_block_with_text_is_kept_with_a_note() {
        let split = md(
            "<details>\n<summary>Show options</summary>\n\nBody.\n",
            &Limits::default(),
        );
        assert_eq!(
            shape(&split.segments),
            [
                S::K("<details>\n<summary>Show options</summary>\n\n"),
                S::T("Body."),
                S::K("\n")
            ]
        );
        assert_eq!(split.notes, [note(1, NoteKind::HtmlText)]);
    }

    #[test]
    fn markup_only_html_and_comments_raise_no_note() {
        let input = "<p align=\"center\">\n<img src=\"a.png\">\n</p>\n\n<!-- hi there -->\n";
        let split = md(input, &Limits::default());
        assert_eq!(shape(&split.segments), [S::K(input)]);
        assert!(split.notes.is_empty());
    }

    #[test]
    fn heading_moves_to_its_section() {
        let split = md("# A\n\naaaa\n\n# B\n\nbbbb\n", &target(16));
        assert_eq!(
            shape(&split.segments),
            [
                S::T("# A\n\naaaa"),
                S::K("\n\n"),
                S::T("# B\n\nbbbb"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn heading_carry_keeps_the_rule_outside_units() {
        let split = md("# A\n\naaaa\n\n---\n\n# B\n\nbbbb\n", &target(40));
        assert_eq!(
            shape(&split.segments),
            [
                S::T("# A\n\naaaa"),
                S::K("\n\n---\n\n"),
                S::T("# B\n\nbbbb"),
                S::K("\n")
            ]
        );
        assert!(
            units(&split.segments)
                .iter()
                .all(|u| hole_ids(u).is_empty())
        );
    }

    #[test]
    fn hole_ids_follow_document_order_across_units() {
        let block = |n: u32| format!("```\nc{n}\n```");
        let input = format!(
            "aaaa\n\n{}\n\nbbbb\n\n{}\n\ncccc\n\n{}\n\ndddd\n",
            block(1),
            block(2),
            block(3)
        );
        let split = md(&input, &target(40));
        let ids: Vec<Vec<u32>> = units(&split.segments).into_iter().map(hole_ids).collect();
        assert_eq!(ids, [vec![1], vec![2]]);

        // With one more block and room for two holes per unit, ids run 1, 2, 3.
        let input = format!(
            "a\n\n{}\n\nb\n\n{}\n\nc\n\n{}\n\nd\n\n{}\n\ne\n",
            block(1),
            block(2),
            block(3),
            block(4)
        );
        let split = md(&input, &target(60));
        let ids: Vec<Vec<u32>> = units(&split.segments).into_iter().map(hole_ids).collect();
        assert_eq!(ids, [vec![1, 2], vec![3]]);
        let first_unit_end = input.find("\n\n```\nc3").unwrap();
        let second_unit_start = input.find("d\n\n").unwrap();
        assert_eq!(
            shape(&split.segments),
            [
                S::T(&input[..first_unit_end]),
                S::K("\n\n```\nc3\n```\n\n"),
                S::T(&input[second_unit_start..input.len() - 1]),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn reserved_text_is_kept_with_a_note() {
        for input in ["Odd <|\u{2060}plamo: x\n", "See <!--pt:k1--> x\n"] {
            let split = md(input, &Limits::default());
            assert_eq!(shape(&split.segments), [S::K(input)]);
            assert_eq!(split.notes, [note(1, NoteKind::ReservedMarker)]);
        }
    }

    #[test]
    fn reserved_text_in_a_later_slice_reports_the_block_start_line() {
        let input = "- step:\n  ```sh\n  cargo build\n  ```\n- see <!--pt:k1--> x\n";
        let split = md(input, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [
                S::T("- step:"),
                S::K("\n  ```sh\n  cargo build\n  ```\n- see <!--pt:k1--> x\n")
            ]
        );
        assert_eq!(split.notes, [note(1, NoteKind::ReservedMarker)]);
    }

    #[test]
    fn reserved_text_in_two_slices_of_one_block_raises_one_note() {
        let input = "- see <!--pt:k1--> x\n  ```\n  c\n  ```\n- and <!--pt:k2--> y\n";
        let split = md(input, &Limits::default());
        assert_eq!(split.notes, [note(1, NoteKind::ReservedMarker)]);
    }

    #[test]
    fn note_lines_follow_document_order_and_repeat_once() {
        let input = "Intro.\n\n    code one\n\n\
                     - see <!--pt:k1--> x\n  <div>html text</div>\n- and <!--pt:k2--> y\n\n\
                     Tail.\n\n    code two\n";
        let split = md(input, &Limits::default());
        assert_eq!(
            split.notes,
            [
                note(3, NoteKind::IndentedCode),
                note(6, NoteKind::HtmlText),
                note(5, NoteKind::ReservedMarker),
                note(11, NoteKind::IndentedCode),
            ]
        );
    }

    #[test]
    fn reference_links_are_sent_inline() {
        let input =
            "Read the [guide][setup], [setup][] and [setup].\n\n[setup]: https://x.y/s \"T\"\n";
        let split = md(input, &Limits::default());
        let unit = only_unit(&split);
        assert!(
            unit.text().starts_with(
                "Read the [guide](https://x.y/s \"T\"), [setup](https://x.y/s \"T\") and [setup](https://x.y/s \"T\")."
            ),
            "{}",
            unit.text()
        );
        assert_eq!(
            unit.source(),
            "Read the [guide][setup], [setup][] and [setup]."
        );
        assert_eq!(
            shape(&split.segments),
            [
                S::T("Read the [guide][setup], [setup][] and [setup]."),
                S::K("\n\n[setup]: https://x.y/s \"T\"\n")
            ]
        );
    }

    #[test]
    fn image_destination_with_space_is_bracketed() {
        let split = md("![logo][img]\n\n[img]: <a b.png>\n", &Limits::default());
        let unit = only_unit(&split);
        assert!(
            unit.text().starts_with("![logo](<a b.png>)"),
            "{}",
            unit.text()
        );
    }

    #[test]
    fn brackets_that_are_not_links_are_not_rewritten() {
        let split = md("a[0][1] and [x](http://y)\n", &Limits::default());
        let unit = only_unit(&split);
        assert_eq!(rewrite_count(unit), 0);
        assert_eq!(unit.text(), "a[0][1] and [x](http://y)");
    }

    #[test]
    fn rewrites_skip_definitions_that_could_carry_markers() {
        for definition in [
            "[r]: /x \"<!--pt:k1-->\"",
            "[r]: /x \"<|\u{2060}plamo: t\"",
            "[r]: /x \"two\nlines\"",
            "[r]: /a>b",
        ] {
            let input = format!("See [a][r] now.\n\n{definition}\n");
            let split = md(&input, &Limits::default());
            let unit = units(&split.segments)[0];
            assert_eq!(rewrite_count(unit), 0, "{definition:?}");
            assert_eq!(unit.text(), "See [a][r] now.");
        }
    }

    #[test]
    fn rewrite_destinations_and_titles_are_escaped() {
        let input = "[a][p] [b][e] [c][t]\n\n[p]: /p(1)\n[e]: <>\n[t]: /x 'say \"hi\"'\n";
        let split = md(input, &Limits::default());
        assert_eq!(
            only_unit(&split).text(),
            "[a](</p(1)>) [b](<>) [c](/x \"say \\\"hi\\\"\")"
        );
    }

    #[test]
    fn rewrites_skip_titles_with_backslashes_and_pipes() {
        for input in [
            "See [a][r].\n\n[r]: /x \"t\\\\\"\n",
            "| [l][r] | c |\n|---|---|\n| x | y |\n\n[r]: /x%7Cy \"p|q\"\n",
            "| [l][r] | c |\n|---|---|\n| x | y |\n\n[r]: /a|b\n",
        ] {
            let split = md(input, &Limits::default());
            let unit = units(&split.segments)[0];
            assert_eq!(rewrite_count(unit), 0, "{input:?}");
        }
    }

    /// Checks that `definition` is whole in exactly one of `segments` and never sent,
    /// then does the same for every cut of the unit holding it, until no cut is left.
    fn assert_never_cut_inside(segments: &[Segment], definition: &str) {
        let holding: Vec<&Segment> = segments
            .iter()
            .filter(|s| s.source().contains(definition))
            .collect();
        assert_eq!(holding.len(), 1, "{segments:?}");
        for unit in units(segments) {
            assert!(!unit.text().contains("[r]:"), "{unit:?}");
        }
        if let Segment::Translate(unit) = holding[0]
            && let Some(pieces) = split_unit_checked(unit)
        {
            assert_never_cut_inside(&pieces, definition);
        }
    }

    #[test]
    fn definitions_inside_containers_become_holes() {
        for (input, definition) in [
            (
                "> [r]: https://x.y/s \"Setup guide\"\n> See [a][r].\n",
                "[r]: https://x.y/s \"Setup guide\"",
            ),
            (
                "- item text\n\n  [r]: https://x.y/s \"Setup guide\"\n- b [a][r]\n",
                "[r]: https://x.y/s \"Setup guide\"",
            ),
            (
                "A[^1].\n\n[^1]: Note.\n\n    [r]: /x \"Title words\"\n",
                "[r]: /x \"Title words\"",
            ),
        ] {
            let split = md(input, &Limits::default());
            assert_never_cut_inside(&split.segments, definition);
        }
        let split = md(
            "> [r]: https://x.y/s \"Setup guide\"\n> See [a][r].\n",
            &Limits::default(),
        );
        assert_eq!(
            shape(&split.segments),
            [
                S::K("> [r]: https://x.y/s \"Setup guide\"\n"),
                S::T("> See [a][r]."),
                S::K("\n")
            ]
        );
        let unit = only_unit(&md(
            "- item text\n\n  [r]: https://x.y/s \"Setup guide\"\n- b [a][r]\n",
            &Limits::default(),
        ))
        .clone();
        assert_eq!(
            unit.text(),
            "- item text\n\n  <!--pt:k1-->\n- b [a](https://x.y/s \"Setup guide\")"
        );
    }

    #[test]
    fn image_inside_a_reference_link_is_carried_verbatim() {
        let input = "[![logo][img]][r] and [*em* text][r]\n\n[img]: /i.png\n[r]: /x\n";
        let split = md(input, &Limits::default());
        let unit = only_unit(&split);
        assert_eq!(rewrite_count(unit), 2);
        assert_eq!(unit.text(), "[![logo][img]](/x) and [*em* text](/x)");
    }

    #[test]
    fn nested_html_and_math_become_holes_with_block_line_notes() {
        let input = "Top.\n\n- item\n  <div>nested html text</div>\n- next\n";
        let split = md(input, &Limits::default());
        assert_eq!(
            only_unit(&split).text(),
            "Top.\n\n- item\n  <!--pt:k1-->\n- next"
        );
        assert_eq!(split.notes, [note(4, NoteKind::HtmlText)]);

        let split = md(
            "- a\n  <div>one</div>\n- b\n  <div>two</div>\n- c\n",
            &Limits::default(),
        );
        assert_eq!(
            split.notes,
            [note(2, NoteKind::HtmlText), note(4, NoteKind::HtmlText)]
        );

        let input = "1. one\n\n   $$\n   y = 2\n   $$\n";
        let split = md(input, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::T("1. one"), S::K("\n\n   $$\n   y = 2\n   $$\n")]
        );
        assert!(split.notes.is_empty());
    }

    #[test]
    fn bom_and_toml_front_matter_are_kept() {
        let input = "\u{FEFF}+++\nx = 1\n+++\nBody\n";
        let split = md(input, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::K("\u{FEFF}+++\nx = 1\n+++\n"), S::T("Body"), S::K("\n")]
        );
        let input = "\u{FEFF}---\nno closer\n";
        let split = md(input, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::K("\u{FEFF}---\n"), S::T("no closer"), S::K("\n")]
        );
    }

    #[test]
    fn html_text_detection() {
        use super::html_has_text;
        assert!(html_has_text("<details>\n<summary>Show</summary>\n"));
        assert!(html_has_text("> <div>\n> quoted text\n> </div>\n"));
        assert!(!html_has_text(
            "<p align=\"center\">\n<img src=\"a.png\">\n</p>\n"
        ));
        assert!(!html_has_text("  <!-- comment text -->\n"));
        assert!(!html_has_text("<img\n  src=\"a.png\"\n>\n"));
    }

    fn markdown_unit(input: &str) -> Unit {
        only_unit(&md(input, &Limits::default())).clone()
    }

    fn assert_cut(unit: &Unit, expected: &[S<'_>]) {
        let segments = split_unit_checked(unit).expect("a cut");
        assert_eq!(shape(&segments), expected);
    }

    #[test]
    fn list_unit_is_cut_between_items() {
        let unit = markdown_unit("- a\n- b\n- c\n- d");
        assert_cut(&unit, &[S::T("- a\n- b"), S::K("\n"), S::T("- c\n- d")]);
    }

    #[test]
    fn table_unit_is_not_cut_around_the_delimiter_row() {
        let unit = markdown_unit("| h |\n|---|\n| a |\n| b |");
        assert_cut(
            &unit,
            &[S::T("| h |\n|---|\n| a |"), S::K("\n"), S::T("| b |")],
        );
    }

    #[test]
    fn markdown_units_are_cut_at_the_hole() {
        assert_cut(
            &markdown_unit(ROW_9),
            &[
                S::T("- step:"),
                S::K("\n  ```sh\n  cargo build\n  ```\n"),
                S::T("- next"),
            ],
        );
        assert_cut(
            &markdown_unit(ROW_11),
            &[
                S::T("> text"),
                S::K("\n> ```\n> code\n> ```\n"),
                S::T("> more"),
            ],
        );
    }

    const ALL: &str = include_str!("testdata/all.md");

    /// Cuts `unit` until no cut is left, checking round-trip, shape and hole ids at each step.
    fn cut_until_done(unit: &Unit, max_unit_bytes: usize) {
        let Some(segments) = split_unit_checked(unit) else {
            return;
        };
        for segment in &segments {
            if let Segment::Translate(piece) = segment {
                assert!(piece.text().len() <= max_unit_bytes);
                cut_until_done(piece, max_unit_bytes);
            }
        }
    }

    #[test]
    fn round_trip_property_on_all_constructs() {
        assert!(ALL.contains("\n|-|-|\n"));
        let crlf = ALL.replace('\n', "\r\n");
        let inputs = [
            ALL.to_owned(),
            crlf.clone(),
            format!("\u{FEFF}{ALL}"),
            format!("\u{FEFF}{crlf}"),
        ];
        check_round_trip(&inputs);
    }

    /// Splits each input in both formats at three limit sets and cuts every unit until
    /// no cut is left, checking round-trip, shape and hole ids throughout. Table rows cannot be
    /// cut inside, so a row longer than a small limit stays oversize; the size and
    /// placeholder limits (size) are therefore only checked at the defaults.
    fn check_round_trip(inputs: &[String]) {
        let limit_sets = [
            ((10, 20, 16), false),
            ((100, 200, 2), false),
            ((3000, 6000, 16), true),
        ];
        for input in inputs {
            for format in [Format::Text, Format::Markdown] {
                for ((target_bytes, max_unit_bytes, max_placeholders), check_size) in limit_sets {
                    let limits = Limits {
                        target_bytes,
                        max_unit_bytes,
                        max_placeholders,
                    };
                    let checked = if check_size {
                        limits
                    } else {
                        Limits {
                            max_unit_bytes: usize::MAX,
                            max_placeholders: usize::MAX,
                            ..limits
                        }
                    };
                    let split = split(input, format, &limits);
                    check_invariants(input, &split, &checked);
                    for unit in units(&split.segments) {
                        cut_until_done(unit, checked.max_unit_bytes);
                    }
                }
            }
        }
    }

    const CONTAINERS: &str = include_str!("testdata/containers.md");

    #[test]
    fn round_trip_property_on_container_constructs() {
        let crlf = CONTAINERS.replace('\n', "\r\n");
        check_round_trip(&[CONTAINERS.to_owned(), crlf, format!("\u{FEFF}{CONTAINERS}")]);
    }

    #[test]
    fn container_constructs_split_as_markdown() {
        let split = md(CONTAINERS, &Limits::default());
        let sent: String = units(&split.segments)
            .iter()
            .map(|u| u.text())
            .collect::<Vec<_>>()
            .join("\n");
        for verbatim in [
            "quoted html text",
            "quoted list code",
            "footnote code",
            "[qdef]:",
            "[ldef]:",
            "[fdef]:",
            "[logo]:",
        ] {
            assert!(!sent.contains(verbatim), "{verbatim:?} was sent");
        }
        for translated in [
            "Quoted paragraph after the HTML.",
            "second quoted item",
            "[quoted](https://x.y/q \"Quoted definition\")",
            "[listed](https://x.y/l \"List definition\")",
            "| `a\\|b` | pipe inside inline code |",
            "footnote list item",
            "![logo](/logo.png)",
            "[fdef](/f \"Footnote definition\")",
        ] {
            assert!(sent.contains(translated), "{translated:?} was not sent");
        }
    }

    #[test]
    fn all_constructs_split_as_markdown() {
        let split = md(ALL, &Limits::default());
        let kinds: Vec<NoteKind> = split.notes.iter().map(|n| n.kind).collect();
        assert_eq!(
            kinds,
            [
                NoteKind::HtmlText,
                NoteKind::IndentedCode,
                NoteKind::HtmlText,
                NoteKind::ReservedMarker
            ]
        );
        let sent: String = units(&split.segments)
            .iter()
            .map(|u| u.text())
            .collect::<Vec<_>>()
            .join("\n");
        for verbatim in [
            "title: All constructs",
            "cargo build",
            "println!",
            "indented code block",
            "x^2 + y^2",
            "y = 2",
            "<summary>",
            "[setup]: /s",
        ] {
            assert!(!sent.contains(verbatim), "{verbatim:?} was sent");
        }
        for translated in [
            "[guide](/s \"T\")",
            "![logo](<a b.png>)",
            "[hint](/h)",
            "Inside details body.",
            "| row 220 | value for row 220 |",
            "日本語の二つ目の段落です。",
            "<|\u{2060}plamo:op|>",
            "Energy $E=mc^2$",
            "[^1]: The footnote text.",
        ] {
            assert!(sent.contains(translated), "{translated:?} was not sent");
        }
        assert!(
            units(&split.segments)
                .iter()
                .any(|u| u.source().contains("|-|-|") && u.source().len() > 3000)
        );
    }
}
