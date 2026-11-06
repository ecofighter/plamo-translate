//! Maps model output back onto a unit's source.

use std::ops::Range;

use super::{FinishError, Finished, MASKED, Part, RESERVED, Unit, check, mask};

/// A placeholder found in model output.
pub(super) struct Marker {
    pub range: Range<usize>,
    /// `None` if the digits do not fit an id.
    pub id: Option<u32>,
}

/// Every match of `<!--[ \t]*pt:k(\d+)[ \t]*-->` in `s`, in order. Models sometimes
/// pad the marker with spaces, so the parser is more lenient than what is sent.
pub(super) fn find_markers(s: &str) -> Vec<Marker> {
    let mut markers = Vec::new();
    let mut from = 0;
    while let Some(at) = s[from..].find("<!--").map(|i| from + i) {
        match parse_marker(s, at) {
            Some(marker) => {
                from = marker.range.end;
                markers.push(marker);
            }
            None => from = at + 1,
        }
    }
    markers
}

fn parse_marker(s: &str, start: usize) -> Option<Marker> {
    let rest = s[start + "<!--".len()..].trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix("pt:k")?;
    let digits_len = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits_len == 0 {
        return None;
    }
    let id = rest[..digits_len].parse().ok();
    let rest = rest[digits_len..].trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix("-->")?;
    Some(Marker {
        range: start..s.len() - rest.len(),
        id,
    })
}

/// Normalizes raw model output: LF line ends, no surrounding whitespace.
fn normalize(raw: &str) -> String {
    raw.replace("\r\n", "\n").trim().to_owned()
}

impl Unit {
    /// Rebuilds the unit from model output: translated text between the markers,
    /// verbatim blocks (with the whitespace and quote prefix around them) from the
    /// source.
    pub fn finish(&self, raw: &str) -> Result<Finished, FinishError> {
        let t = normalize(raw);
        if t.is_empty() {
            return Err(FinishError::Empty);
        }
        let holes: Vec<&Part> = self
            .parts
            .iter()
            .filter(|p| matches!(p, Part::Hole { .. }))
            .collect();
        let markers = find_markers(&t);
        let expected = holes.iter().map(|p| match p {
            Part::Hole { id, .. } => Some(*id),
            Part::Text { .. } => unreachable!(),
        });
        if !markers.iter().map(|m| m.id).eq(expected) {
            return Err(FinishError::Placeholders);
        }

        let mut fragments = Vec::with_capacity(markers.len() + 1);
        let mut cursor = 0;
        for marker in &markers {
            fragments.push(trim_before_marker(&t[cursor..marker.range.start]));
            cursor = marker.range.end;
        }
        fragments.push(&t[cursor..]);
        if !markers.is_empty() {
            for fragment in &mut fragments[1..] {
                *fragment = trim_after_marker(fragment);
            }
            // Every unit starts and ends with text, holes are never adjacent, and every
            // text part has an alphanumeric character, so a fragment without one means
            // the model dropped translated text.
            if fragments
                .iter()
                .any(|f| !f.chars().any(char::is_alphanumeric))
            {
                return Err(FinishError::Placeholders);
            }
        }

        let mut text = String::with_capacity(self.source.len());
        for (index, fragment) in fragments.iter().enumerate() {
            if index > 0 {
                let Part::Hole {
                    lead, body, trail, ..
                } = holes[index - 1]
                else {
                    unreachable!();
                };
                text.push_str(lead);
                text.push_str(body);
                text.push_str(trail);
            }
            text.push_str(&self.restore(fragment));
        }

        let issues = check::issues(self.format, &self.text(), &t);
        Ok(Finished { text, issues })
    }

    /// The (sent text, translation) pair for history, or `None` for blank output. The
    /// translation is masked so the daemon never rejects the pair.
    pub fn history_pair(&self, raw: &str) -> Option<(String, String)> {
        let t = normalize(raw);
        if t.is_empty() {
            return None;
        }
        Some((self.text(), mask(&t)))
    }

    fn restore(&self, fragment: &str) -> String {
        let unmasked = fragment.replace(MASKED, RESERVED);
        if self.crlf {
            unmasked.replace('\n', "\r\n")
        } else {
            unmasked
        }
    }
}

fn is_quote_only(line: &str) -> bool {
    line.chars().all(|c| matches!(c, '\n' | ' ' | '\t' | '>'))
}

/// Text before a marker loses trailing whitespace and the final lines holding only a
/// quote prefix: the hole's `lead` and `qprefix` restore them from the source.
fn trim_before_marker(fragment: &str) -> &str {
    let mut fragment = fragment.trim_end_matches([' ', '\t']);
    while !fragment.is_empty() {
        let line_start = fragment.rfind('\n').unwrap_or(0);
        if !is_quote_only(&fragment[line_start..]) {
            break;
        }
        fragment = fragment[..line_start].trim_end();
    }
    fragment.trim_end()
}

/// Text after a marker loses leading whitespace and the lines holding only a quote
/// prefix: the hole's `trail` restores them from the source.
fn trim_after_marker(fragment: &str) -> &str {
    let mut fragment = fragment.trim_start();
    while let Some(line_end) = fragment.find('\n') {
        if !is_quote_only(&fragment[..line_end]) {
            break;
        }
        fragment = fragment[line_end + 1..].trim_start();
    }
    fragment
}

#[cfg(test)]
mod tests {
    use super::super::tests::{check_unit, units};
    use super::super::{Format, Limits, Segment, Unit, split, split_unit};
    use super::*;

    fn only_unit(input: &str, format: Format) -> Unit {
        let split = split(input, format, &Limits::default());
        let units = units(&split.segments);
        assert_eq!(units.len(), 1, "{:?}", split.segments);
        check_unit(units[0]);
        units[0].clone()
    }

    /// The reassembled text, asserting that no issue was reported.
    fn finished(unit: &Unit, raw: &str) -> String {
        let done = unit.finish(raw).unwrap();
        assert_eq!(done.issues, [], "{raw:?}");
        done.text
    }

    #[test]
    fn crlf_is_restored_and_blank_output_is_empty() {
        let unit = only_unit("a\r\nb", Format::Text);
        assert_eq!(unit.text(), "a\nb");
        assert_eq!(finished(&unit, "x\ny\n"), "x\r\ny");
        assert_eq!(unit.finish(" \n").err(), Some(FinishError::Empty));
        assert_eq!(unit.history_pair(" \n"), None);
    }

    #[test]
    fn mixed_line_ends_come_back_as_crlf() {
        let unit = only_unit("a\r\nb\nc", Format::Text);
        assert_eq!(unit.text(), "a\nb\nc");
        assert_eq!(finished(&unit, "x\ny\nz"), "x\r\ny\r\nz");
    }

    #[test]
    fn code_block_comes_back_from_the_source() {
        let unit = only_unit(
            "Intro\n\n```rs\nlet x = 1;\n```\n\nOutro\n",
            Format::Markdown,
        );
        let expected = "導入\n\n```rs\nlet x = 1;\n```\n\n結び";
        assert_eq!(finished(&unit, "導入\n<!--pt:k1-->\n結び"), expected);
        assert_eq!(finished(&unit, "導入\n<!-- pt:k1 -->\n結び"), expected);
        assert_eq!(
            unit.finish("導入 結び").err(),
            Some(FinishError::Placeholders)
        );
    }

    #[test]
    fn reserved_text_is_masked_and_restored() {
        let source = "Use `<|plamo:op|>` here.";
        let unit = only_unit(source, Format::Markdown);
        let text = unit.text();
        assert!(text.contains("<|\u{2060}plamo:"));
        assert!(!text.contains("<|plamo:"));
        assert_eq!(finished(&unit, &text), source);
        assert_eq!(
            unit.history_pair("`<|plamo:op|>` を使う\n"),
            Some((text, "`<|\u{2060}plamo:op|>` を使う".to_owned()))
        );
    }

    #[test]
    fn markers_must_match_the_holes_in_order() {
        let input = "a1\n\n--\n\na2\n\n--\n\na3\n";
        let unit = only_unit(input, Format::Text);
        assert_eq!(
            unit.text(),
            "a1\n\n<!--pt:k1-->\n\na2\n\n<!--pt:k2-->\n\na3"
        );
        assert_eq!(
            finished(&unit, "b1\n<!--pt:k1-->\nb2\n<!--pt:k2-->\nb3"),
            "b1\n\n--\n\nb2\n\n--\n\nb3"
        );
        for raw in [
            "b1 <!--pt:k2--> b2 <!--pt:k1--> b3",
            "b1 <!--pt:k1--> b2 b3",
            "b1 <!--pt:k1--> b2 <!--pt:k2--> b3 <!--pt:k2-->",
            "b1 <!--pt:k1--> b2 <!--pt:k3--> b3",
            "b1 <!--pt:k1--> <!--pt:k2--> b3",
            "b1 <!--pt:k1-->\n \n<!--pt:k2--> b3",
            "b1 <!--pt:k1--> b2 <!--pt:k99999999999--> b3",
            "<!--pt:k1--> b2 <!--pt:k2--> b3",
            "b1 <!--pt:k1--> b2 <!--pt:k2-->",
            "> <!--pt:k1--> b2 <!--pt:k2--> b3",
        ] {
            assert_eq!(
                unit.finish(raw).err(),
                Some(FinishError::Placeholders),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn lenient_marker_parsing() {
        let ids = |s: &str| -> Vec<Option<u32>> { find_markers(s).iter().map(|m| m.id).collect() };
        assert_eq!(ids("<!--pt:k1-->"), [Some(1)]);
        assert_eq!(ids("<!-- \tpt:k12\t -->x<!--pt:k3-->"), [Some(12), Some(3)]);
        assert_eq!(ids("<!-- pt :k1 -->"), []);
        assert_eq!(ids("<!--pt:k-->"), []);
        assert_eq!(ids("<!--pt:k1x-->"), []);
        assert_eq!(ids("<!--pt:k1--"), []);
        assert_eq!(ids("<!--<!--pt:k2-->"), [Some(2)]);
        assert_eq!(ids("<!--pt:k99999999999-->"), [None]);
        let markers = find_markers("ab <!-- pt:k7 --> cd");
        assert_eq!(markers[0].range, 3..17);
    }

    #[test]
    fn quote_only_line_before_a_marker_is_dropped() {
        assert_eq!(trim_before_marker("> a\n> "), "> a");
        assert_eq!(trim_before_marker("> a\n>>\t"), "> a");
        assert_eq!(trim_before_marker("a  \n\n"), "a");
        assert_eq!(trim_before_marker("a\n> b "), "a\n> b");
        assert_eq!(trim_before_marker("a"), "a");
    }

    #[test]
    fn output_without_holes_is_trimmed_and_unmasked() {
        let unit = only_unit("Hello there.", Format::Text);
        let done = unit
            .finish("\r\n  こんにちは <|\u{2060}plamo:x\r\n")
            .unwrap();
        assert_eq!(done.text, "こんにちは <|plamo:x");
        assert!(done.issues.is_empty());
    }

    #[test]
    fn crlf_in_hole_whitespace_alone_makes_output_crlf() {
        let unit = only_unit(
            "Intro\r\n\r\n```rs\r\nlet x = 1;\r\n```\r\n\r\nOutro\r\n",
            Format::Markdown,
        );
        assert!(unit.crlf);
        assert_eq!(unit.text(), "Intro\n\n<!--pt:k1-->\n\nOutro");
        let text = finished(&unit, "導入\n<!--pt:k1-->\n結び\nです");
        assert_eq!(
            text,
            "導入\r\n\r\n```rs\r\nlet x = 1;\r\n```\r\n\r\n結び\r\nです"
        );
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
    }

    /// `finish` given the sent text unchanged reproduces the source.
    fn assert_round_trip(unit: &Unit) {
        assert_eq!(finished(unit, &unit.text()), unit.source());
    }

    #[test]
    fn text_ending_in_a_quote_only_line_before_a_hole() {
        let unit = only_unit("> para\n>\n> ```\n> c\n> ```\n> more\n", Format::Markdown);
        assert_eq!(unit.text(), "> para\n>\n> <!--pt:k1-->\n> more");
        assert_round_trip(&unit);
        assert_eq!(
            finished(&unit, "> 段落\n>\n> <!--pt:k1-->\n> 続き"),
            "> 段落\n>\n> ```\n> c\n> ```\n> 続き"
        );
    }

    #[test]
    fn hole_whitespace_carries_list_indentation() {
        let unit = only_unit("- text\n  ```\n  code\n  ```\n  more\n", Format::Markdown);
        assert_eq!(unit.text(), "- text\n  <!--pt:k1-->\n  more");
        assert_round_trip(&unit);
        assert_eq!(
            finished(&unit, "- 本文\n<!--pt:k1-->\n続き"),
            "- 本文\n  ```\n  code\n  ```\n  続き"
        );
    }

    #[test]
    fn unclosed_fence_inside_a_list() {
        let unit = only_unit("- a\n  ```\n  code\n- b\n", Format::Markdown);
        assert_eq!(unit.text(), "- a\n  <!--pt:k1-->\n- b");
        assert_round_trip(&unit);
        assert_eq!(
            finished(&unit, "- あ\n  <!--pt:k1-->\n- い"),
            "- あ\n  ```\n  code\n- い"
        );
    }

    #[test]
    fn marker_text_inside_a_hole_body_is_never_parsed() {
        let unit = only_unit("A\n\n```\n<!--pt:k1-->\n```\n\nB\n", Format::Markdown);
        assert_eq!(unit.text(), "A\n\n<!--pt:k1-->\n\nB");
        assert_round_trip(&unit);
        assert_eq!(
            finished(&unit, "あ\n<!--pt:k1-->\nい"),
            "あ\n\n```\n<!--pt:k1-->\n```\n\nい"
        );
    }

    #[test]
    fn quote_blank_lines_next_to_a_hole_come_back_from_the_source() {
        for (source, raw, expected) in [
            (
                "> para\n>\n>     code\n>\n> more\n",
                "> 段落\n> <!--pt:k1-->\n>\n> 続き",
                "> 段落\n>\n>     code\n>\n> 続き",
            ),
            (
                "> See [a][r].\n>\n> [r]: /x\n>\n> More text.\n",
                "> 参照 [a](/x)。\n> <!--pt:k1-->\n>\n> 続き",
                "> 参照 [a](/x)。\n>\n> [r]: /x\n>\n> 続き",
            ),
            (
                "> para\n>\n> <div>\n> x\n> </div>\n>\n> more\n",
                "> 段落\n>\n> <!--pt:k1-->\n> 続き",
                "> 段落\n>\n> <div>\n> x\n> </div>\n>\n> 続き",
            ),
        ] {
            let unit = only_unit(source, Format::Markdown);
            let [first, Part::Hole { lead, trail, .. }, _] = unit.parts.as_slice() else {
                panic!("one hole expected: {unit:?}");
            };
            assert!(matches!(first, Part::Text { src, .. } if !src.ends_with('>')));
            assert_eq!((lead.as_str(), trail.as_str()), ("\n>\n", "\n>\n"));
            assert!(unit.text().contains(">\n> <!--pt:k1-->\n>\n"), "{unit:?}");
            // A rewritten reference link comes back in its inline form.
            if !source.contains("][") {
                assert_round_trip(&unit);
            }
            assert_eq!(finished(&unit, raw), expected, "{source:?}");
        }
    }

    #[test]
    fn quote_blank_lines_around_a_hole_in_crlf_and_nested_quotes() {
        let unit = only_unit(
            ">> para\r\n>>\r\n>>     code\r\n> >\r\n>> more\r\n",
            Format::Markdown,
        );
        assert_round_trip(&unit);
        assert_eq!(
            finished(&unit, ">> 段落\n>> <!--pt:k1-->\n>> 続き"),
            ">> 段落\r\n>>\r\n>>     code\r\n> >\r\n>> 続き"
        );
    }

    #[test]
    fn fragments_without_alphanumeric_text_are_a_placeholder_error() {
        let unit = only_unit(
            "> para\n>\n> ```\n> c\n> ```\n>\n> more\n",
            Format::Markdown,
        );
        for raw in [
            ">\n> <!--pt:k1-->\n>\n> 続き",
            "> 段落\n> <!--pt:k1-->\n>",
            "> 。\n> <!--pt:k1-->\n> 続き",
        ] {
            assert_eq!(
                unit.finish(raw).err(),
                Some(FinishError::Placeholders),
                "{raw:?}"
            );
        }
        let unit = only_unit("a1\n\n--\n\na2", Format::Text);
        for raw in [
            ">\n<!--pt:k1-->\nb2",
            "b1\n<!--pt:k1-->\n>",
            "b1 <!--pt:k1--> --",
        ] {
            assert_eq!(
                unit.finish(raw).err(),
                Some(FinishError::Placeholders),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn quote_only_lines_after_a_marker_are_dropped() {
        assert_eq!(trim_after_marker("\n>\n> a"), "> a");
        assert_eq!(trim_after_marker(" \n> \n>>\n  b\n>"), "b\n>");
        assert_eq!(trim_after_marker(">"), ">");
        assert_eq!(trim_before_marker("> a\n>\n> "), "> a");
    }

    #[test]
    fn losing_the_first_or_last_fragment_is_a_placeholder_error() {
        let unit = only_unit("a1\n\n--\n\na2", Format::Text);
        for raw in ["b1\n<!--pt:k1-->", "<!--pt:k1-->\nb2"] {
            assert_eq!(
                unit.finish(raw).err(),
                Some(FinishError::Placeholders),
                "{raw:?}"
            );
        }
    }

    /// Cuts `unit` with `split_unit` until no cut is left and checks every piece.
    fn round_trip_recursively(unit: &Unit, checked: &mut usize) {
        let has_rewrites = unit
            .parts
            .iter()
            .any(|p| matches!(p, Part::Text { rewrites, .. } if !rewrites.is_empty()));
        if !has_rewrites {
            assert_eq!(
                unit.finish(&unit.text()),
                Ok(Finished {
                    text: unit.source().to_owned(),
                    issues: Vec::new(),
                }),
                "{unit:?}"
            );
            *checked += 1;
        }
        for segment in split_unit(unit).into_iter().flatten() {
            if let Segment::Translate(piece) = segment {
                round_trip_recursively(&piece, checked);
            }
        }
    }

    #[test]
    fn sent_text_returned_unchanged_reassembles_the_source() {
        let documents = [
            include_str!("testdata/all.md"),
            include_str!("testdata/containers.md"),
        ];
        let mut checked = 0;
        for document in documents {
            let crlf = document.replace('\n', "\r\n");
            let variants = [
                document.to_owned(),
                format!("\u{FEFF}{document}"),
                format!("\u{FEFF}{crlf}"),
                crlf,
            ];
            for input in &variants {
                for format in [Format::Text, Format::Markdown] {
                    for (target_bytes, max_unit_bytes, max_placeholders) in
                        [(100, 200, 2), (3000, 6000, 16)]
                    {
                        let limits = Limits {
                            target_bytes,
                            max_unit_bytes,
                            max_placeholders,
                        };
                        for unit in units(&split(input, format, &limits).segments) {
                            round_trip_recursively(unit, &mut checked);
                        }
                    }
                }
            }
        }
        assert!(checked > 1000, "{checked}");
    }
}
