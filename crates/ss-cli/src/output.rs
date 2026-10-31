//! Plain-text output helpers: a column-aligned table, byte and secret
//! formatting, and the redaction applied to every tracing line.

use std::fmt::Write as _;
use std::io::{self, Write};

use tracing_subscriber::fmt::MakeWriter;

/// A simple left-aligned table.
#[derive(Debug, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// A table with these column headers.
    pub fn new<S: Into<String>>(headers: impl IntoIterator<Item = S>) -> Self {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    /// Append a row; short rows are padded, long ones truncated.
    pub fn row<S: Into<String>>(&mut self, cells: impl IntoIterator<Item = S>) -> &mut Self {
        let mut row: Vec<String> = cells.into_iter().map(Into::into).collect();
        row.resize(self.headers.len(), String::new());
        self.rows.push(row);
        self
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Render with two spaces between columns and a `-` rule under the header.
    pub fn render(&self) -> String {
        let n = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(n) {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
        let mut out = String::new();
        let line = |out: &mut String, cells: &[String]| {
            for (i, cell) in cells.iter().enumerate().take(n) {
                if i > 0 {
                    out.push_str("  ");
                }
                if i + 1 == n {
                    out.push_str(cell);
                } else {
                    let _ = write!(out, "{cell:<width$}", width = widths[i]);
                }
            }
            out.push('\n');
        };
        line(&mut out, &self.headers);
        let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
        line(&mut out, &rule);
        for row in &self.rows {
            line(&mut out, row);
        }
        out
    }
}

/// `1.5 MiB`, `312 KiB`, `17 B`.
pub fn format_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// `yes` / `no`.
pub fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Surveillance Station's `videoCodec` numbers as the web UI names them.
/// Best effort: the mapping is from the SS web client's constants and is
/// not part of the documented API.
pub fn video_codec_name(code: i64) -> String {
    match code {
        0 => "-".into(),
        1 => "MJPEG".into(),
        2 => "MPEG-4".into(),
        3 => "H.264".into(),
        4 => "MxPEG".into(),
        5 => "H.265".into(),
        6 => "H.264+".into(),
        7 => "H.265+".into(),
        n => format!("codec#{n}"),
    }
}

/// Values following these keys are secrets. The needles are matched
/// case-insensitively; a needle ending in `"` means the value runs to the
/// next quote, otherwise to the next `&`, whitespace or bracket.
const SECRET_NEEDLES: &[&str] = &[
    "_sid=",
    "passwd=",
    "password=",
    "otp_code=",
    "device_id=",
    "cookie: id=",
    "\"sid\":\"",
    "\"sid\": \"",
    "\"did\":\"",
    "\"did\": \"",
];

fn value_end(text: &str, start: usize, quoted: bool) -> usize {
    text[start..]
        .char_indices()
        .find(|&(_, c)| {
            if quoted {
                c == '"'
            } else {
                c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ',' | ';' | ')' | ']' | '}')
            }
        })
        .map_or(text.len(), |(i, _)| start + i)
}

/// Replace every secret value in `text` (session ids, passwords, OTP
/// codes, device tokens, the WebSocket cookie) with `<redacted>`.
pub fn redact(text: &str) -> String {
    // ASCII lowercasing keeps byte offsets identical to `text`.
    let lower = text.to_ascii_lowercase();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for needle in SECRET_NEEDLES {
        let quoted = needle.ends_with('"');
        let mut from = 0;
        while let Some(pos) = lower[from..].find(needle) {
            let start = from + pos + needle.len();
            let end = value_end(text, start, quoted);
            if end > start {
                ranges.push((start, end));
            }
            from = start;
        }
    }
    if ranges.is_empty() {
        return text.to_owned();
    }
    ranges.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        if start < cursor {
            continue; // overlapped by the previous replacement
        }
        out.push_str(&text[cursor..start]);
        out.push_str("<redacted>");
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// A [`MakeWriter`] for `tracing_subscriber::fmt` that redacts each
/// formatted event before it reaches stderr.
#[derive(Debug, Clone, Copy, Default)]
pub struct RedactingStderr;

impl<'a> MakeWriter<'a> for RedactingStderr {
    type Writer = RedactingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::default()
    }
}

/// One event's bytes, scrubbed on flush/drop.
#[derive(Debug, Default)]
pub struct RedactingWriter {
    buf: Vec<u8>,
}

impl Write for RedactingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        let mut err = io::stderr().lock();
        err.write_all(redact(&text).as_bytes())?;
        err.flush()
    }
}

impl Drop for RedactingWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_aligns_columns() {
        let mut t = Table::new(["id", "name", "x"]);
        t.row(["1", "Front Door", "a"]).row(["22", "B"]);
        let s = t.render();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "id  name        x");
        assert_eq!(lines[1], "--  ----------  -");
        assert_eq!(lines[2], "1   Front Door  a");
        assert_eq!(lines[3], "22  B           ");
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
    }

    #[test]
    fn bytes_and_codecs() {
        assert_eq!(format_bytes(17), "17 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(video_codec_name(3), "H.264");
        assert_eq!(video_codec_name(5), "H.265");
        assert_eq!(video_codec_name(0), "-");
        assert_eq!(video_codec_name(99), "codec#99");
    }

    #[test]
    fn redacts_every_secret_shape() {
        let line = "GET /webapi/entry.cgi?api=X&_sid=AbC123&method=List _SID=Zz9 \
                    passwd=hunter2&otp_code=123456 Cookie: id=sess-77 \
                    {\"sid\":\"S1\",\"did\":\"D1\"} {\"sid\": \"S2\"} device_id=tok)";
        let r = redact(line);
        for secret in [
            "AbC123", "Zz9", "hunter2", "123456", "sess-77", "S1", "D1", "S2", "tok",
        ] {
            assert!(!r.contains(secret), "{secret} leaked: {r}");
        }
        assert!(r.contains("api=X&_sid=<redacted>&method=List"));
        assert!(r.contains("\"sid\":\"<redacted>\",\"did\":\"<redacted>\""));
        assert!(r.contains("device_id=<redacted>)"));
        // Camera ids and the word "id" are left alone.
        assert_eq!(redact("camera id=7 paused=true"), "camera id=7 paused=true");
        assert_eq!(redact("nothing here"), "nothing here");
        // A secret at the very end, without a terminator.
        assert_eq!(redact("x _sid=tail"), "x _sid=<redacted>");
    }
}
