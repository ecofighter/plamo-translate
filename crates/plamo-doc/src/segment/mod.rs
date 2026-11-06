//! Splits a document into verbatim segments and translation units, and maps model
//! output back onto the source.
//!
//! Every byte that is not translated is copied from an input slice; nothing is
//! re-rendered. Verbatim material between merged candidates travels inside a unit as a
//! placeholder (`<!--pt:kN-->`) whose body is never sent to the model.
//!
//! The tests check four properties of every split:
//! - round-trip: concatenating every segment's source gives the input byte for byte
//!   (likewise for `split_unit` pieces and the unit they come from).
//! - shape: no two Keeps are adjacent and no Keep is empty; a unit starts and ends with
//!   a Text part with no whitespace edges; holes are never adjacent; every Text part has
//!   an alphanumeric character; `text()` never contains the reserved `<|plamo:` sequence.
//! - size: after `split`, every unit's `text()` is at most `max_unit_bytes` and has at
//!   most `max_placeholders` holes, unless no valid cut exists.
//! - hole ids: ids in one `split` result are 1..=H in document order, and `split_unit`
//!   keeps them.

mod check;
mod cut;
mod finish;
mod markdown;
mod plain;

use std::fmt;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
}

impl Format {
    pub fn as_str(&self) -> &'static str {
        match self {
            Format::Text => "text",
            Format::Markdown => "markdown",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub target_bytes: usize,
    pub max_unit_bytes: usize,
    pub max_placeholders: usize,
}

/// Merged units around 3 KB cut the request count sharply compared with one request per
/// block (the median unit was about 2.3 KB on a 300-file corpus). The model kept the
/// structure on single requests up to 15 KB, and the caps bound the cost of a failed
/// request. Translation accuracy against unit size was not measured; lower
/// `target_bytes` first if quality suffers.
impl Default for Limits {
    fn default() -> Self {
        Limits {
            target_bytes: 3000,
            max_unit_bytes: 6000,
            max_placeholders: 16,
        }
    }
}

pub struct Split {
    pub segments: Vec<Segment>,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Keep(String),
    Translate(Unit),
}

impl Segment {
    #[cfg(test)]
    pub fn source(&self) -> &str {
        match self {
            Segment::Keep(s) => s,
            Segment::Translate(u) => u.source(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Note {
    /// 1-based line of the block start.
    pub line: usize,
    pub kind: NoteKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteKind {
    IndentedCode,
    HtmlText,
    ReservedMarker,
}

impl NoteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NoteKind::IndentedCode => "indented_code",
            NoteKind::HtmlText => "html",
            NoteKind::ReservedMarker => "reserved_marker",
        }
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line = self.line;
        match self.kind {
            NoteKind::IndentedCode => write!(
                f,
                "line {line}: indented block left untranslated as code (use --format text if it is prose)"
            ),
            NoteKind::HtmlText => write!(f, "line {line}: HTML block left untranslated"),
            NoteKind::ReservedMarker => write!(
                f,
                "line {line}: block left untranslated: contains reserved marker text"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    parts: Vec<Part>,
    format: Format,
    crlf: bool,
    source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    /// `rewrites` hold ranges relative to `src`, sorted and disjoint.
    Text {
        src: String,
        rewrites: Vec<(Range<usize>, String)>,
    },
    Hole {
        id: u32,
        lead: String,
        qprefix: String,
        body: String,
        trail: String,
    },
}

impl Unit {
    /// The exact input bytes this unit covers.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Exactly what is sent to the model.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Text { src, rewrites } => out.push_str(&sent_text(src, rewrites)),
                Part::Hole {
                    id,
                    lead,
                    qprefix,
                    trail,
                    ..
                } => {
                    out.push_str(&lf(lead));
                    out.push_str(qprefix);
                    out.push_str(&marker(*id));
                    out.push_str(&lf(trail));
                }
            }
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Finished {
    pub text: String,
    pub issues: Vec<Issue>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Issue {
    Structure {
        expected: String,
        got: String,
    },
    Inline {
        missing: Vec<String>,
        added: Vec<String>,
    },
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::Structure { expected, got } => {
                write!(f, "structure changed (expected {expected}; got {got})")
            }
            Issue::Inline { missing, added } => {
                let sides: Vec<String> = [("missing", missing), ("added", added)]
                    .into_iter()
                    .filter(|(_, items)| !items.is_empty())
                    .map(|(label, items)| format!("{label}: {}", items.join(", ")))
                    .collect();
                write!(f, "inline elements changed ({})", sides.join("; "))
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FinishError {
    Empty,
    Placeholders,
}

impl fmt::Display for FinishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FinishError::Empty => "empty translation",
            FinishError::Placeholders => "verbatim-block markers lost or reordered",
        })
    }
}

const BOM: char = '\u{FEFF}';
/// The daemon rejects text containing this sequence.
const RESERVED: &str = "<|plamo:";
/// `RESERVED` with a word joiner inserted, which the daemon accepts.
const MASKED: &str = "<|\u{2060}plamo:";
const MARKER_PREFIX: &str = "<!--pt:k";
/// Budget charged for each placeholder marker when sizing a chunk.
const PH_EST: usize = 20;

fn marker(id: u32) -> String {
    format!("{MARKER_PREFIX}{id}-->")
}

fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn mask(s: &str) -> String {
    s.replace(RESERVED, MASKED)
}

fn apply_rewrites(src: &str, rewrites: &[(Range<usize>, String)]) -> String {
    let mut out = String::with_capacity(src.len());
    let mut cursor = 0;
    for (range, replacement) in rewrites {
        out.push_str(&src[cursor..range.start]);
        out.push_str(replacement);
        cursor = range.end;
    }
    out.push_str(&src[cursor..]);
    out
}

fn sent_text(src: &str, rewrites: &[(Range<usize>, String)]) -> String {
    mask(&lf(&apply_rewrites(src, rewrites)))
}

/// Longest prefix of the first line matching `^( {0,3}>[ ]?)+`.
fn quote_prefix(body: &str) -> &str {
    let line = body.split('\n').next().unwrap_or("");
    let bytes = line.as_bytes();
    let mut end = 0;
    loop {
        let mut i = end;
        while i < bytes.len() && i - end < 3 && bytes[i] == b' ' {
            i += 1;
        }
        if bytes.get(i) != Some(&b'>') {
            break;
        }
        i += 1;
        if bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        end = i;
    }
    &line[..end]
}

/// Splits verbatim text between two text parts into (lead, body, trail). Lead and
/// trail are the whitespace edges plus any whole quote-blank lines next to the body,
/// so that those lines always come back from the source.
fn hole_parts(verbatim: &str) -> (&str, &str, &str) {
    let rest = verbatim.trim_start();
    let mut lead_len = verbatim.len() - rest.len();
    if verbatim[..lead_len].contains('\n') {
        lead_len += quote_blank_head_len(rest);
    }
    let rest = &verbatim[lead_len..];
    let mut body = rest.trim_end();
    if body.is_empty() {
        // Nothing but quote-blank lines: they are the body.
        let rest = verbatim.trim_start();
        let body = rest.trim_end();
        return (
            &verbatim[..verbatim.len() - rest.len()],
            body,
            &rest[body.len()..],
        );
    }
    if rest[body.len()..].contains('\n') {
        body = &body[..without_quote_blank_tail(body)];
    }
    (&verbatim[..lead_len], body, &rest[body.len()..])
}

/// A line holding only `[ \t>]` (and the CR of a CRLF): a blank line inside a quote.
fn is_quote_blank(line: &str) -> bool {
    line.chars().all(|c| matches!(c, ' ' | '\t' | '>' | '\r'))
}

/// Bytes of the leading quote-blank lines of `s` and the whitespace after them. `s`
/// starts its line, or follows only whitespace on it.
fn quote_blank_head_len(s: &str) -> usize {
    let mut start = 0;
    while let Some(nl) = s[start..].find('\n') {
        if !is_quote_blank(&s[start..start + nl]) {
            break;
        }
        let next = start + nl + 1;
        start = s.len() - s[next..].trim_start().len();
    }
    start
}

/// Length of `s` without its trailing quote-blank lines and the whitespace before
/// them. `s` ends its line, or is followed only by whitespace on it.
fn without_quote_blank_tail(s: &str) -> usize {
    let mut end = s.len();
    while let Some(nl) = s[..end].rfind('\n') {
        if !is_quote_blank(&s[nl + 1..end]) {
            break;
        }
        end = s[..nl].trim_end().len();
    }
    end
}

/// A translatable range of the input, before chunking.
#[derive(Clone, Debug)]
struct Cand {
    range: Range<usize>,
    /// Absolute input ranges inside `range`, sorted and disjoint.
    rewrites: Vec<(Range<usize>, String)>,
    heading: bool,
    /// Set on pieces of a bisected oversize candidate; such pieces are never merged.
    no_merge: bool,
}

/// Collects candidates from a format-specific splitter. Everything outside the
/// candidates is verbatim; gaps between candidates become Keep segments or holes.
struct Builder<'a> {
    input: &'a str,
    cands: Vec<Cand>,
    notes: Vec<Note>,
    /// The offset of the last line lookup and its line. Lookups arrive in document
    /// order, so counting on from here keeps them linear in the input.
    line_cursor: (usize, usize),
}

impl<'a> Builder<'a> {
    fn new(input: &'a str) -> Self {
        Builder {
            input,
            cands: Vec::new(),
            notes: Vec::new(),
            line_cursor: (0, 1),
        }
    }

    /// 1-based line of `offset`.
    fn line_at(&mut self, offset: usize) -> usize {
        let (at, line) = self.line_cursor;
        let line = if offset >= at {
            line + self.input[at..offset].matches('\n').count()
        } else {
            line - self.input[offset..at].matches('\n').count()
        };
        self.line_cursor = (offset, line);
        line
    }

    fn note(&mut self, offset: usize, kind: NoteKind) {
        let line = self.line_at(offset);
        self.notes.push(Note { line, kind });
    }

    /// Adds `range` after edge trim; symbol-only cores and cores with reserved marker
    /// text stay verbatim (the latter with a note on `block_line`, the line where the
    /// enclosing block starts). Ranges must arrive in document order.
    fn candidate(
        &mut self,
        range: Range<usize>,
        rewrites: Vec<(Range<usize>, String)>,
        heading: bool,
        block_line: usize,
    ) {
        let slice = &self.input[range.clone()];
        let start = range.start + (slice.len() - slice.trim_start().len());
        let end = range.end - (slice.len() - slice.trim_end().len());
        if start >= end {
            return;
        }
        let core = &self.input[start..end];
        if !core.chars().any(char::is_alphanumeric) {
            return;
        }
        // Masked text or marker-like text in the source would be indistinguishable from
        // what this module inserts, so such blocks are never sent.
        if core.contains(MASKED) || contains_marker_like(core) {
            self.notes.push(Note {
                line: block_line,
                kind: NoteKind::ReservedMarker,
            });
            return;
        }
        debug_assert!(self.cands.last().is_none_or(|c| c.range.end <= start));
        debug_assert!(
            rewrites
                .iter()
                .all(|(r, _)| start <= r.start && r.end <= end)
        );
        self.cands.push(Cand {
            range: start..end,
            rewrites,
            heading,
            no_merge: false,
        });
    }
}

/// `<!--`, optional spaces or tabs, then `pt:k`: anything a lenient marker parser
/// could take for a placeholder.
fn contains_marker_like(s: &str) -> bool {
    s.match_indices("<!--").any(|(at, open)| {
        s[at + open.len()..]
            .trim_start_matches([' ', '\t'])
            .starts_with("pt:k")
    })
}

pub fn split(input: &str, format: Format, limits: &Limits) -> Split {
    let mut builder = Builder::new(input);
    match format {
        Format::Text => plain::split(input, &mut builder),
        Format::Markdown => markdown::split(input, &mut builder),
    }
    let Builder { cands, notes, .. } = builder;
    let cands = cut::split_oversize(input, cands, limits);
    Split {
        segments: assemble(input, format, &cands, limits),
        notes,
    }
}

/// Cuts `unit` once into `[Translate, Keep?, Translate]`, keeping placeholder ids;
/// `None` if no cut leaves translatable text on both sides.
pub fn split_unit(unit: &Unit) -> Option<Vec<Segment>> {
    cut::split_unit(unit)
}

/// Chunks the candidates, builds units with holes, and fills the rest with Keeps.
fn assemble(input: &str, format: Format, cands: &[Cand], limits: &Limits) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut cursor = 0;
    for chunk in chunk(input, cands, limits) {
        let chunk = &cands[chunk];
        let start = chunk[0].range.start;
        if cursor < start {
            segments.push(Segment::Keep(input[cursor..start].to_owned()));
        }
        segments.push(Segment::Translate(build_unit(input, format, chunk)));
        cursor = chunk[chunk.len() - 1].range.end;
    }
    if cursor < input.len() {
        segments.push(Segment::Keep(input[cursor..].to_owned()));
    }
    assign_ids(&mut segments);
    segments
}

/// Bytes a candidate contributes to the sent text.
fn cand_size(input: &str, cand: &Cand) -> usize {
    sent_text(
        &input[cand.range.clone()],
        &relative(&cand.rewrites, cand.range.start),
    )
    .len()
}

/// Size and placeholder cost of the verbatim gap between two merged candidates.
fn gap_cost(gap: &str) -> (usize, usize) {
    if gap.chars().all(char::is_whitespace) {
        return (lf(gap).len(), 0);
    }
    let (lead, body, trail) = hole_parts(gap);
    let size = lf(lead).len() + quote_prefix(body).len() + lf(trail).len() + PH_EST;
    (size, 1)
}

/// Groups consecutive candidates into chunks; returns index ranges into `cands`.
fn chunk(input: &str, cands: &[Cand], limits: &Limits) -> Vec<Range<usize>> {
    let sizes: Vec<usize> = cands.iter().map(|c| cand_size(input, c)).collect();
    let mut chunks = Vec::new();
    let mut first = 0;
    while first < cands.len() {
        let mut size = sizes[first];
        let mut holes = 0;
        let mut last = first;
        let mut overflow = false;
        while last + 1 < cands.len() {
            let next = last + 1;
            if cands[last].no_merge || cands[next].no_merge {
                break;
            }
            let (gap_size, gap_holes) =
                gap_cost(&input[cands[last].range.end..cands[next].range.start]);
            let grown = size + gap_size + sizes[next];
            let grown_holes = holes + gap_holes;
            if grown > limits.target_bytes || grown_holes > limits.max_placeholders {
                overflow = true;
                break;
            }
            size = grown;
            holes = grown_holes;
            last = next;
        }
        // A heading that would end a full chunk belongs with the section it introduces.
        if overflow && last > first && cands[last].heading {
            chunks.push(first..last);
            first = last;
        } else {
            chunks.push(first..last + 1);
            first = last + 1;
        }
    }
    chunks
}

fn relative(rewrites: &[(Range<usize>, String)], base: usize) -> Vec<(Range<usize>, String)> {
    rewrites
        .iter()
        .map(|(r, s)| (r.start - base..r.end - base, s.clone()))
        .collect()
}

/// Builds one unit from consecutive candidates. Whitespace-only gaps join the
/// surrounding Text part; other gaps become holes (ids assigned later). Quote-blank
/// lines at text edges next to a hole move into the hole, since models often drop
/// them and only holes are restored from the source.
fn build_unit(input: &str, format: Format, chunk: &[Cand]) -> Unit {
    let mut parts = Vec::new();
    let mut text_start = chunk[0].range.start;
    let mut text_end = chunk[0].range.end;
    let mut rewrites = chunk[0].rewrites.clone();
    let flush =
        |parts: &mut Vec<Part>, start: usize, end: usize, rewrites: &[(Range<usize>, String)]| {
            parts.push(Part::Text {
                src: input[start..end].to_owned(),
                rewrites: relative(rewrites, start),
            });
        };
    for cand in &chunk[1..] {
        let gap = &input[text_end..cand.range.start];
        if !gap.chars().all(char::is_whitespace) {
            // Only whole lines move: the gap must break the line on that side.
            let mut hole_start = text_end;
            if gap[..gap.len() - gap.trim_start().len()].contains('\n') {
                hole_start = text_start + without_quote_blank_tail(&input[text_start..text_end]);
            }
            let mut hole_end = cand.range.start;
            if gap[gap.trim_end().len()..].contains('\n') {
                hole_end += quote_blank_head_len(&input[cand.range.clone()]);
            }
            flush(&mut parts, text_start, hole_start, &rewrites);
            let (lead, body, trail) = hole_parts(&input[hole_start..hole_end]);
            parts.push(Part::Hole {
                id: 0,
                lead: lead.to_owned(),
                qprefix: quote_prefix(body).to_owned(),
                body: body.to_owned(),
                trail: trail.to_owned(),
            });
            text_start = hole_end;
            rewrites.clear();
        }
        text_end = cand.range.end;
        rewrites.extend(cand.rewrites.iter().cloned());
    }
    flush(&mut parts, text_start, text_end, &rewrites);
    let source = input[chunk[0].range.start..text_end].to_owned();
    Unit {
        parts,
        format,
        crlf: uses_crlf(&source),
        source,
    }
}

/// Whether newlines the model writes into this source should become CRLF. Holes
/// count too: when every text part is a single line, the only line ends are in the
/// whitespace around the holes.
fn uses_crlf(source: &str) -> bool {
    source.contains("\r\n")
}

/// Numbers holes `1..=H` in document order across all units.
fn assign_ids(segments: &mut [Segment]) {
    let mut next = 1;
    for segment in segments {
        if let Segment::Translate(unit) = segment {
            for part in &mut unit.parts {
                if let Part::Hole { id, .. } = part {
                    *id = next;
                    next += 1;
                }
            }
        }
    }
}

pub fn detect_format(input: &str) -> Format {
    let body = input.strip_prefix(BOM).unwrap_or(input);
    if front_matter_len(body).is_some() {
        return Format::Markdown;
    }
    let markdown_line = |line: &str| {
        is_atx_heading(line) || is_fence(line) || is_delimiter_row(line) || has_inline_link(line)
    };
    if body.lines().any(markdown_line) {
        Format::Markdown
    } else {
        Format::Text
    }
}

/// Length of front matter at the start of `s` (after any BOM), including the closing
/// line's `\n`: an opening line `---` or `+++`, closed by the first later `---` or
/// `...` line (`+++` only for `+++`). Either line may end in spaces and a `\r`, but
/// nothing else. `None` if there is no closing line.
fn front_matter_len(s: &str) -> Option<usize> {
    let content = |line: &str| {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        line.trim_end_matches(' ').to_owned()
    };
    let mut lines = s.split_inclusive('\n');
    let first = lines.next()?;
    let closers: &[&str] = match content(first).as_str() {
        "---" => &["---", "..."],
        "+++" => &["+++"],
        _ => return None,
    };
    let mut end = first.len();
    for line in lines {
        end += line.len();
        if closers.contains(&content(line).as_str()) {
            return Some(end);
        }
    }
    None
}

/// Number of leading spaces if there are at most three.
fn indent_upto_3(line: &str) -> Option<usize> {
    let n = line.len() - line.trim_start_matches(' ').len();
    (n <= 3).then_some(n)
}

/// `^ {0,3}#{1,6}([ \t]|$)`
fn is_atx_heading(line: &str) -> bool {
    let Some(indent) = indent_upto_3(line) else {
        return false;
    };
    let rest = &line[indent..];
    let after = rest.trim_start_matches('#');
    let hashes = rest.len() - after.len();
    (1..=6).contains(&hashes) && (after.is_empty() || after.starts_with([' ', '\t']))
}

/// `^ {0,3}(```|~~~)`
fn is_fence(line: &str) -> bool {
    indent_upto_3(line).is_some_and(|n| {
        let rest = &line[n..];
        rest.starts_with("```") || rest.starts_with("~~~")
    })
}

/// A table delimiter row: contains `|` and matches
/// `^[ \t>]*\|?[ \t]*:?-+:?[ \t]*(\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$`.
fn is_delimiter_row(line: &str) -> bool {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if !line.contains('|') {
        return false;
    }
    let rest = line.trim_start_matches([' ', '\t', '>']);
    let rest = rest.strip_prefix('|').unwrap_or(rest);
    let mut cells: Vec<&str> = rest.split('|').collect();
    if cells.len() > 1 && cells[cells.len() - 1].trim_matches([' ', '\t']).is_empty() {
        cells.pop();
    }
    cells.iter().all(|cell| {
        let cell = cell.trim_matches([' ', '\t']);
        let cell = cell.strip_prefix(':').unwrap_or(cell);
        let cell = cell.strip_suffix(':').unwrap_or(cell);
        !cell.is_empty() && cell.bytes().all(|b| b == b'-')
    })
}

/// A line containing `[label](destination)` with a non-empty destination.
fn has_inline_link(line: &str) -> bool {
    line.match_indices("](").any(|(at, _)| {
        let label_open = line[..at].rfind('[');
        let label_ok = label_open.is_some_and(|open| !line[open..at].contains(']'));
        let dest_ok = line[at + 2..].find(')').is_some_and(|close| close > 0);
        label_ok && dest_ok
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Shape of a split for comparisons: `K` for Keep, `T` for Translate with its source.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum S<'a> {
        K(&'a str),
        T(&'a str),
    }

    pub(super) fn shape(segments: &[Segment]) -> Vec<S<'_>> {
        segments
            .iter()
            .map(|s| match s {
                Segment::Keep(k) => S::K(k),
                Segment::Translate(u) => S::T(u.source()),
            })
            .collect()
    }

    pub(super) fn units(segments: &[Segment]) -> Vec<&Unit> {
        segments
            .iter()
            .filter_map(|s| match s {
                Segment::Translate(u) => Some(u),
                Segment::Keep(_) => None,
            })
            .collect()
    }

    pub(super) fn hole_ids(unit: &Unit) -> Vec<u32> {
        unit.parts
            .iter()
            .filter_map(|p| match p {
                Part::Hole { id, .. } => Some(*id),
                Part::Text { .. } => None,
            })
            .collect()
    }

    fn part_source(part: &Part) -> String {
        match part {
            Part::Text { src, .. } => src.clone(),
            Part::Hole {
                lead, body, trail, ..
            } => format!("{lead}{body}{trail}"),
        }
    }

    /// Checks the shape property for one unit, plus internal consistency of its stored fields.
    pub(super) fn check_unit(unit: &Unit) {
        let parts = &unit.parts;
        assert!(
            matches!(parts.first(), Some(Part::Text { .. })),
            "unit must start with text: {unit:?}"
        );
        assert!(
            matches!(parts.last(), Some(Part::Text { .. })),
            "unit must end with text: {unit:?}"
        );
        if let Some(Part::Text { src, .. }) = parts.first() {
            assert!(
                !src.starts_with(char::is_whitespace),
                "leading whitespace: {unit:?}"
            );
        }
        if let Some(Part::Text { src, .. }) = parts.last() {
            assert!(
                !src.ends_with(char::is_whitespace),
                "trailing whitespace: {unit:?}"
            );
        }
        for pair in parts.windows(2) {
            assert!(
                !matches!(pair, [Part::Hole { .. }, Part::Hole { .. }]),
                "adjacent holes: {unit:?}"
            );
        }
        for part in parts {
            match part {
                Part::Text { src, rewrites } => {
                    assert!(
                        src.chars().any(char::is_alphanumeric),
                        "no alphanumeric: {unit:?}"
                    );
                    let mut cursor = 0;
                    for (range, _) in rewrites {
                        assert!(
                            cursor <= range.start && range.end <= src.len(),
                            "rewrites: {unit:?}"
                        );
                        cursor = range.end;
                    }
                }
                Part::Hole {
                    lead,
                    body,
                    trail,
                    qprefix,
                    ..
                } => {
                    let quote_blank = |c: char| c.is_whitespace() || c == '>';
                    assert!(lead.chars().all(quote_blank), "lead: {unit:?}");
                    assert!(trail.chars().all(quote_blank), "trail: {unit:?}");
                    assert!(!body.is_empty() && body.trim() == body);
                    assert_eq!(qprefix, quote_prefix(body));
                }
            }
        }
        assert!(
            !unit.text().contains(RESERVED),
            "unmasked reserved text: {unit:?}"
        );
        let joined: String = parts.iter().map(part_source).collect();
        assert_eq!(joined, unit.source, "parts must cover the source exactly");
        assert_eq!(unit.crlf, unit.source.contains("\r\n"));
    }

    /// Checks round-trip, shape, size and hole ids for the result of `split`.
    pub(super) fn check_invariants(input: &str, split: &Split, limits: &Limits) {
        let joined: String = split.segments.iter().map(Segment::source).collect();
        assert_eq!(joined, input, "segments must reassemble the input");
        for pair in split.segments.windows(2) {
            assert!(
                !matches!(pair, [Segment::Keep(_), Segment::Keep(_)]),
                "adjacent keeps: {pair:?}"
            );
        }
        let mut ids = Vec::new();
        for segment in &split.segments {
            match segment {
                Segment::Keep(k) => assert!(!k.is_empty(), "empty keep"),
                Segment::Translate(unit) => {
                    check_unit(unit);
                    assert!(
                        unit.text().len() <= limits.max_unit_bytes,
                        "unit too large: {unit:?}"
                    );
                    let unit_ids = hole_ids(unit);
                    assert!(
                        unit_ids.len() <= limits.max_placeholders,
                        "too many holes: {unit:?}"
                    );
                    ids.extend(unit_ids);
                }
            }
        }
        let expected: Vec<u32> = (1..=ids.len() as u32).collect();
        assert_eq!(ids, expected, "hole ids must be 1..=H in document order");
    }

    fn split_checked(input: &str, format: Format, limits: &Limits) -> Split {
        let split = split(input, format, limits);
        check_invariants(input, &split, limits);
        split
    }

    fn target(bytes: usize) -> Limits {
        Limits {
            target_bytes: bytes,
            ..Limits::default()
        }
    }

    #[test]
    fn text_paragraphs_merge() {
        let input = "Hello world.\n\nSecond para.\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::T("Hello world.\n\nSecond para."), S::K("\n")]
        );
        assert_eq!(
            units(&split.segments)[0].text(),
            "Hello world.\n\nSecond para."
        );
        assert!(split.notes.is_empty());
    }

    #[test]
    fn text_paragraphs_over_target_stay_apart() {
        let input = "Hello world.\n\nSecond para.\n";
        let split = split_checked(input, Format::Text, &target(10));
        assert_eq!(
            shape(&split.segments),
            [
                S::T("Hello world."),
                S::K("\n\n"),
                S::T("Second para."),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn bom_and_crlf_edges_are_kept() {
        let input = "\u{FEFF}  Hi.\r\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::K("\u{FEFF}  "), S::T("Hi."), S::K("\r\n")]
        );
        let unit = units(&split.segments)[0];
        assert_eq!(unit.text(), "Hi.");
        assert_eq!(unit.format, Format::Text);
        assert!(!unit.crlf);
    }

    #[test]
    fn symbol_only_block_becomes_hole_in_text() {
        let input = "Para one.\n\n----\n\nPara two.\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::T("Para one.\n\n----\n\nPara two."), S::K("\n")]
        );
        let unit = units(&split.segments)[0];
        assert_eq!(unit.text(), "Para one.\n\n<!--pt:k1-->\n\nPara two.");
        assert_eq!(hole_ids(unit), [1]);
    }

    #[test]
    fn format_detection() {
        assert_eq!(detect_format("# T\n"), Format::Markdown);
        assert_eq!(detect_format("see [x](http://y)"), Format::Markdown);
        assert_eq!(detect_format("```\n"), Format::Markdown);
        assert_eq!(detect_format("a | b\n---|---\n"), Format::Markdown);
        assert_eq!(detect_format("Just prose.\n\n    indented\n"), Format::Text);
    }

    #[test]
    fn detect_format_details() {
        assert_eq!(
            detect_format("---\ntitle: x\n---\nbody\n"),
            Format::Markdown
        );
        assert_eq!(
            detect_format("\u{FEFF}+++ \r\nx = 1\r\n+++\r\n"),
            Format::Markdown
        );
        assert_eq!(detect_format("---\nno closer\n"), Format::Text);
        assert_eq!(detect_format("+++\nx\n---\n"), Format::Text);
        assert_eq!(detect_format("####### seven\n"), Format::Text);
        assert_eq!(detect_format("#hashtag\n"), Format::Text);
        assert_eq!(detect_format("    # indented four\n"), Format::Text);
        assert_eq!(detect_format("   ##\n"), Format::Markdown);
        assert_eq!(detect_format("> | :-: | --- |\n"), Format::Markdown);
        assert_eq!(detect_format("a - b | c\n"), Format::Text);
        assert_eq!(detect_format("f(x) [a] (b)\n"), Format::Text);
        assert_eq!(detect_format("![](a.png)\n"), Format::Markdown);
        assert_eq!(detect_format(""), Format::Text);
    }

    #[test]
    fn delimiter_rows() {
        for row in ["|---|", "---|---", "| :--- | ---: |", " > |-|", "|--|\r"] {
            assert!(is_delimiter_row(row), "{row:?}");
        }
        for row in ["---", "| a |", "||", "|:|", "|-- x|", "--|--|--|x"] {
            assert!(!is_delimiter_row(row), "{row:?}");
        }
    }

    #[test]
    fn quote_prefixes() {
        assert_eq!(quote_prefix("> ```\n> x"), "> ");
        assert_eq!(quote_prefix(">> > a"), ">> > ");
        assert_eq!(quote_prefix("   > a"), "   > ");
        assert_eq!(quote_prefix("    > a"), "");
        assert_eq!(quote_prefix("```"), "");
    }

    #[test]
    fn text_empty_and_symbol_only_inputs_are_one_keep() {
        assert!(
            split_checked("", Format::Text, &Limits::default())
                .segments
                .is_empty()
        );
        for input in ["\n\n  \n", "----\n\n* * *\n", "\u{FEFF}"] {
            let split = split_checked(input, Format::Text, &Limits::default());
            assert_eq!(shape(&split.segments), [S::K(input)]);
        }
    }

    #[test]
    fn text_symbol_only_blocks_at_edges_stay_outside_units() {
        let input = "====\n\nTitle text\n\n====\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::K("====\n\n"), S::T("Title text"), S::K("\n\n====\n")]
        );
    }

    #[test]
    fn text_reserved_marker_blocks_stay_verbatim_with_note() {
        let input = "First.\n\nOdd <|\u{2060}plamo: x\n\nSee <!--pt:k1--> x\n\nLast.\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [
                S::T("First.\n\nOdd <|\u{2060}plamo: x\n\nSee <!--pt:k1--> x\n\nLast."),
                S::K("\n")
            ]
        );
        let unit = units(&split.segments)[0];
        assert_eq!(unit.text(), "First.\n\n<!--pt:k1-->\n\nLast.");
        assert_eq!(
            split.notes,
            [
                Note {
                    line: 3,
                    kind: NoteKind::ReservedMarker
                },
                Note {
                    line: 5,
                    kind: NoteKind::ReservedMarker
                },
            ]
        );
    }

    #[test]
    fn spaced_marker_text_is_reserved() {
        let input = "See <!-- pt:k1 --> x\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        assert_eq!(shape(&split.segments), [S::K(input)]);
        assert_eq!(
            split.notes,
            [Note {
                line: 1,
                kind: NoteKind::ReservedMarker
            }]
        );
        assert!(contains_marker_like("a <!--\tpt:k2--> b"));
        assert!(!contains_marker_like("a <!-- note pt:k2 --> b"));
        assert!(!contains_marker_like("a <!-- --> pt:k b"));
    }

    #[test]
    fn front_matter_lines_allow_only_trailing_spaces_and_cr() {
        assert_eq!(front_matter_len("---  \r\nx\r\n--- \r\nbody"), Some(16));
        assert_eq!(front_matter_len("---\t\nx\n---\n"), None);
        assert_eq!(front_matter_len("---\nx\n---\t\n"), None);
        assert_eq!(front_matter_len("---\nx\n...\n"), Some(10));
    }

    #[test]
    fn text_reserved_sequence_is_masked() {
        let input = "Use <|plamo:op|> here.\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        let unit = units(&split.segments)[0];
        assert_eq!(unit.source(), "Use <|plamo:op|> here.");
        assert_eq!(unit.text(), "Use <|\u{2060}plamo:op|> here.");
        assert!(split.notes.is_empty());
    }

    #[test]
    fn text_crlf_is_sent_as_lf() {
        let input = "a\r\nb\r\n\r\n----\r\n\r\nc\r\n";
        let split = split_checked(input, Format::Text, &Limits::default());
        assert_eq!(
            shape(&split.segments),
            [S::T("a\r\nb\r\n\r\n----\r\n\r\nc"), S::K("\r\n")]
        );
        let unit = units(&split.segments)[0];
        assert_eq!(unit.text(), "a\nb\n\n<!--pt:k1-->\n\nc");
        assert!(unit.crlf);
    }

    #[test]
    fn placeholder_limit_closes_chunks_and_ids_continue_across_units() {
        let input = "a1\n\n--\n\na2\n\n--\n\na3\n\n--\n\na4\n";
        let limits = Limits {
            max_placeholders: 1,
            ..Limits::default()
        };
        let split = split_checked(input, Format::Text, &limits);
        assert_eq!(
            shape(&split.segments),
            [
                S::T("a1\n\n--\n\na2"),
                S::K("\n\n--\n\n"),
                S::T("a3\n\n--\n\na4"),
                S::K("\n")
            ]
        );
        let ids: Vec<Vec<u32>> = units(&split.segments).into_iter().map(hole_ids).collect();
        assert_eq!(ids, [vec![1], vec![2]]);
    }

    #[test]
    fn hole_size_counts_toward_target() {
        let input = "aaaa\n\n----\n\nbbbb\n";
        // 4 + (2 + 2 + PH_EST) + 4 = 32
        let merged = split_checked(input, Format::Text, &target(32));
        assert_eq!(units(&merged.segments).len(), 1);
        let apart = split_checked(input, Format::Text, &target(31));
        assert_eq!(units(&apart.segments).len(), 2);
    }

    fn cand(input: &str, text: &str, heading: bool) -> Cand {
        let start = input.find(text).unwrap();
        Cand {
            range: start..start + text.len(),
            rewrites: Vec::new(),
            heading,
            no_merge: false,
        }
    }

    #[test]
    fn heading_moves_to_the_chunk_it_introduces() {
        let input = "# A\n\naaaa\n\n# B\n\nbbbb\n";
        let cands = [
            cand(input, "# A", true),
            cand(input, "aaaa", false),
            cand(input, "# B", true),
            cand(input, "bbbb", false),
        ];
        let segments = assemble(input, Format::Markdown, &cands, &target(16));
        assert_eq!(
            shape(&segments),
            [
                S::T("# A\n\naaaa"),
                S::K("\n\n"),
                S::T("# B\n\nbbbb"),
                S::K("\n")
            ]
        );
    }

    #[test]
    fn heading_carry_turns_would_be_hole_into_keep() {
        let input = "# A\n\naaaa\n\n---\n\n# B\n\nbbbb\n";
        let cands = [
            cand(input, "# A", true),
            cand(input, "aaaa", false),
            cand(input, "# B", true),
            cand(input, "bbbb", false),
        ];
        let segments = assemble(input, Format::Markdown, &cands, &target(40));
        assert_eq!(
            shape(&segments),
            [
                S::T("# A\n\naaaa"),
                S::K("\n\n---\n\n"),
                S::T("# B\n\nbbbb"),
                S::K("\n")
            ]
        );
        assert!(units(&segments).iter().all(|u| hole_ids(u).is_empty()));
    }

    #[test]
    fn lone_heading_and_final_heading_are_not_carried() {
        let input = "# A\n\n# B\n";
        let cands = [cand(input, "# A", true), cand(input, "# B", true)];
        let segments = assemble(input, Format::Markdown, &cands, &target(5));
        assert_eq!(
            shape(&segments),
            [S::T("# A"), S::K("\n\n"), S::T("# B"), S::K("\n")]
        );
        let segments = assemble(input, Format::Markdown, &cands, &Limits::default());
        assert_eq!(shape(&segments), [S::T("# A\n\n# B"), S::K("\n")]);
    }

    #[test]
    fn no_merge_candidates_stay_alone_even_when_adjacent() {
        let input = "xxxxyyyy zzzz";
        let mut cands = [
            cand(input, "xxxx", false),
            cand(input, "yyyy", false),
            cand(input, "zzzz", false),
        ];
        cands[0].no_merge = true;
        cands[1].no_merge = true;
        let segments = assemble(input, Format::Text, &cands, &Limits::default());
        assert_eq!(
            shape(&segments),
            [S::T("xxxx"), S::T("yyyy"), S::K(" "), S::T("zzzz")]
        );
    }

    #[test]
    fn rewrites_are_applied_in_text_and_size() {
        let input = "See [a][r] now.\n\nNext.";
        let mut first = cand(input, "See [a][r] now.", false);
        first.rewrites = vec![(4..10, "[a](http://x)".to_owned())];
        let cands = [first, cand(input, "Next.", false)];
        let segments = assemble(input, Format::Markdown, &cands, &Limits::default());
        let unit = units(&segments)[0];
        check_unit(unit);
        assert_eq!(unit.source(), "See [a][r] now.\n\nNext.");
        assert_eq!(unit.text(), "See [a](http://x) now.\n\nNext.");
        // 22 + 2 + 5 = 29 bytes once rewritten; the raw source is only 22.
        let segments = assemble(input, Format::Markdown, &cands, &target(28));
        assert_eq!(units(&segments).len(), 2);
    }

    #[test]
    fn display_strings() {
        let structure = Issue::Structure {
            expected: "H2 Text".into(),
            got: "Text".into(),
        };
        assert_eq!(
            structure.to_string(),
            "structure changed (expected H2 Text; got Text)"
        );
        let inline = Issue::Inline {
            missing: vec!["`a`".into(), "`b`".into()],
            added: vec!["`c`".into()],
        };
        assert_eq!(
            inline.to_string(),
            "inline elements changed (missing: `a`, `b`; added: `c`)"
        );
        let only_missing = Issue::Inline {
            missing: vec!["x".into()],
            added: Vec::new(),
        };
        assert_eq!(
            only_missing.to_string(),
            "inline elements changed (missing: x)"
        );
        let only_added = Issue::Inline {
            missing: Vec::new(),
            added: vec!["y".into()],
        };
        assert_eq!(only_added.to_string(), "inline elements changed (added: y)");
        assert_eq!(FinishError::Empty.to_string(), "empty translation");
        assert_eq!(
            FinishError::Placeholders.to_string(),
            "verbatim-block markers lost or reordered"
        );
        let finished = Finished {
            text: String::new(),
            issues: vec![structure],
        };
        assert_eq!(finished.issues.len(), 1);
        assert!(finished.text.is_empty());
    }
}
