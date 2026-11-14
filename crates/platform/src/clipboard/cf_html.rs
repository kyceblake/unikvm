//! Translate between Win32 HTML clipboard data and UTF-8 HTML.

const START: &str = "<!--StartFragment-->";
const END: &str = "<!--EndFragment-->";
pub(super) const MAX_HEADER_BYTES: usize = 4096;

pub(super) fn decode(data: &[u8], limit: usize) -> Option<&str> {
    if !data.starts_with(b"Version:") {
        return None;
    }
    let mut start = None;
    let mut end = None;
    // Keep the end of each header line so a fragment cannot claim header bytes.
    let mut header_end = 0;
    for line in data[..data.len().min(MAX_HEADER_BYTES)]
        .split_inclusive(|&byte| matches!(byte, b'\r' | b'\n'))
    {
        header_end += line.len();
        let line = line.trim_ascii_end();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = std::str::from_utf8(line)
            .ok()
            .and_then(|line| line.split_once(':'))
        else {
            break;
        };
        match name {
            "StartFragment" if start.is_none() => start = Some(value.trim().parse::<usize>().ok()?),
            "EndFragment" if end.is_none() => end = Some(value.trim().parse::<usize>().ok()?),
            _ => {}
        }
        if start.is_some() && end.is_some() {
            break;
        }
    }
    let start = start?;
    let end = end?;
    let fragment = data.get(start..end)?;
    // Only transfer the selection, without the source URL or outer context.
    if start < header_end || fragment.is_empty() || fragment.len() > limit || fragment.contains(&0)
    {
        return None;
    }
    std::str::from_utf8(fragment).ok()
}

pub(super) fn encode(html: &str) -> String {
    let header = |start_html, end_html, start_fragment, end_fragment| {
        format!(
            "Version:1.0\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
        )
    };
    let start_html = header(0, 0, 0, 0).len();
    let lower = html.to_ascii_lowercase();
    let body = lower.find("<body").and_then(|open| {
        let tag = &lower[open + "<body".len()..];
        if !tag.starts_with(|c: char| c == '>' || c.is_ascii_whitespace()) {
            return None;
        }
        let from = open + "<body".len() + tag.find('>')? + 1;
        let to = from + lower[from..].rfind("</body>")?;
        Some((&html[..from], &html[from..to], &html[to..]))
    });
    let (prefix, fragment, suffix) = body.unwrap_or(("<html><body>", html, "</body></html>"));
    let start_fragment = start_html + prefix.len() + START.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + END.len() + suffix.len();
    let mut result = header(start_html, end_html, start_fragment, end_fragment);
    result.push_str(prefix);
    result.push_str(START);
    result.push_str(fragment);
    result.push_str(END);
    result.push_str(suffix);
    result.push('\0');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_byte_offsets_for_non_ascii_text_and_omits_outer_context() {
        let fragment = "<strong>日本語🧪</strong>";
        for html in [
            fragment.to_owned(),
            format!(
                "<HTML><head><title>🧪</title></head><BODY class=\"a\">{fragment}</BODY></HTML>"
            ),
        ] {
            let encoded = encode(&html);
            assert_eq!(decode(encoded.as_bytes(), fragment.len()), Some(fragment));
            assert_eq!(decode(encoded.as_bytes(), fragment.len() - 1), None);
        }
    }

    #[test]
    fn malformed_offsets_do_not_fall_back_to_the_outer_document() {
        let html = encode("<em>日本語</em>");
        let (first, rest) = html.split_once("EndFragment:").unwrap();
        let corrupt = format!("{first}EndFragment:9999999999{}", &rest[10..]);
        assert_eq!(decode(corrupt.as_bytes(), 1024), None);
        let (first, rest) = html.split_once("StartFragment:").unwrap();
        let into_header = format!("{first}StartFragment:0000000008{}", &rest[10..]);
        assert_eq!(decode(into_header.as_bytes(), 1024), None);
        assert_eq!(decode(b"Version:1.0\r\n<html>text</html>", 1024), None);
    }
}
