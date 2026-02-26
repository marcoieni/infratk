use arboard::Clipboard;
use regex::Regex;

pub fn copy_to_clipboard(output_str: &str) {
    // Strip ANSI escape sequences
    let re = Regex::new(r"\x1b\[([\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e])").unwrap();
    let output_str = re.replace_all(output_str, "").to_string();

    let mut clipboard = Clipboard::new().unwrap();
    clipboard.set_text(output_str).unwrap();
}
