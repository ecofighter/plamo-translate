use super::{BOM, Builder};

/// Every maximal run of non-blank lines is one candidate; blank lines, the BOM and
/// trimmed edges stay verbatim.
pub(super) fn split(input: &str, builder: &mut Builder) {
    let body_start = if input.starts_with(BOM) {
        BOM.len_utf8()
    } else {
        0
    };
    let mut block_start = None;
    let mut pos = body_start;
    for line in input[body_start..].split_inclusive('\n') {
        let blank = line.chars().all(char::is_whitespace);
        match (blank, block_start) {
            (false, None) => block_start = Some(pos),
            (true, Some(start)) => {
                let line = builder.line_at(start);
                builder.candidate(start..pos, Vec::new(), false, line);
                block_start = None;
            }
            _ => {}
        }
        pos += line.len();
    }
    if let Some(start) = block_start {
        let line = builder.line_at(start);
        builder.candidate(start..pos, Vec::new(), false, line);
    }
}
