pub fn resolve_target(text: &str, default_target: &str) -> String {
    if default_target != "auto" {
        return default_target.to_string();
    }
    let mut letters = 0usize;
    let mut cjk = 0usize;
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        letters += 1;
        if matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
        {
            cjk += 1;
        }
    }
    if letters > 0 && cjk * 10 >= letters * 3 {
        "en"
    } else {
        "ja"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_japanese_goes_to_en() {
        assert_eq!(resolve_target("会議は月曜日です", "auto"), "en");
    }
    #[test]
    fn auto_english_goes_to_ja() {
        assert_eq!(resolve_target("The meeting is on Monday", "auto"), "ja");
    }
    #[test]
    fn auto_threshold_30_percent_is_en() {
        assert_eq!(resolve_target("あいう abcdefg", "auto"), "en");
    }
    #[test]
    fn auto_below_threshold_is_ja() {
        assert_eq!(resolve_target("あい abcdefgh", "auto"), "ja");
    }
    #[test]
    fn explicit_default_passthrough() {
        assert_eq!(resolve_target("hello", "zh-hans"), "zh-hans");
    }
    #[test]
    fn no_letters_defaults_ja() {
        assert_eq!(resolve_target("12345 !!", "auto"), "ja");
    }
}
