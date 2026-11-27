//! The text of a PDF for a chat message. The browser cannot read a PDF without a large library, so the owner's file
//! goes to the hub, which returns its text cut to `MAX_CHARS`. A scanned PDF has no text in it and is not read: this
//! build has no OCR.

/// The most text one PDF contributes (the chat cuts an attachment again to its own, shorter limit).
pub const MAX_CHARS: usize = 6000;

/// The text of a PDF, and whether it was cut to `MAX_CHARS`.
pub fn pdf_text(bytes: &[u8]) -> Result<(String, bool), String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("the file is not a PDF (it does not start with %PDF-)".into());
    }
    let owned = bytes.to_vec();
    // The extractor is a third-party parser; a file it cannot handle must not take the hub down with it.
    let text = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&owned))
        .map_err(|_| "the PDF could not be read".to_string())?
        .map_err(|e| format!("the PDF could not be read: {e}"))?;
    Ok(cut_chars(text.trim(), MAX_CHARS))
}

/// The first `max` characters of `text`, and whether there were more.
pub fn cut_chars(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    (text.chars().take(max).collect(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pdf_gives_its_text() {
        let (text, truncated) = pdf_text(include_bytes!("testdata/hello.pdf")).unwrap();
        assert!(text.contains("Hello from the PDF test"), "{text:?}");
        assert!(!truncated);
    }

    #[test]
    fn a_file_that_is_not_a_pdf_is_refused() {
        assert!(pdf_text(b"hello").unwrap_err().contains("not a PDF"));
    }

    #[test]
    fn a_long_text_is_cut_and_says_so() {
        assert_eq!(cut_chars("abc", 5), ("abc".to_string(), false));
        assert_eq!(cut_chars("abcdef", 4), ("abcd".to_string(), true));
        // characters, not bytes: an accented letter is one character
        assert_eq!(cut_chars("ééé", 2), ("éé".to_string(), true));
    }
}
