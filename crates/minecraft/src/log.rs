//! Reading the game's console output.
//!
//! Ephervex passes the log4j configuration each release ships, because for the
//! older half of the supported range that file *is* Mojang's Log4Shell
//! mitigation - its pattern is `%msg{nolookups}`, and launching 1.12.2 without
//! it means launching the vulnerable configuration. The side effect is that the
//! console appender it installs is a `LegacyXMLLayout`, so the game's stdout is
//! not lines of text but a stream of XML records:
//!
//! ```text
//! <log4j:Event logger="net.minecraft.client.main.Main" timestamp="1754…"
//!              level="INFO" thread="main">
//!     <log4j:Message><![CDATA[Setting user: Notch]]></log4j:Message>
//! </log4j:Event>
//! ```
//!
//! Streaming that verbatim to a terminal would satisfy "stream stdout" and help
//! nobody. [`Log4jReader`] reassembles the records instead, which costs about
//! eighty lines and gives the CLI readable output today and the GUI a level and
//! a logger to filter on in log filtering support.
//!
//! # Why it is not an XML parser
//!
//! The input is one program's output in one fixed layout, not a document from
//! anywhere. A parser would have to be fed a stream, tolerate a stack trace
//! inside CDATA, and survive the process being killed mid-record - and it would
//! pull in a dependency to do less. Anything that does not look like a record
//! is passed through untouched, which is the behavior that matters: a JVM
//! crash message, a `Segmentation fault`, or a launch that failed before log4j
//! was configured must still reach the user.

/// One line of the game's output, however it arrived.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogRecord {
    /// `INFO`, `WARN`, `ERROR`, … when the record carried one.
    pub level: Option<String>,
    /// The log4j logger name, e.g. `net.minecraft.client.main.Main`.
    pub logger: Option<String>,
    /// The thread that emitted it, e.g. `Render thread`.
    pub thread: Option<String>,
    /// The message, with any attached throwable appended.
    pub message: String,
}

impl LogRecord {
    /// A line that was not a log4j record.
    #[must_use]
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// The line as a terminal should show it.
    #[must_use]
    pub fn display(&self) -> String {
        match (&self.thread, &self.level) {
            (Some(thread), Some(level)) => format!("[{thread}/{level}] {}", self.message),
            (None, Some(level)) => format!("[{level}] {}", self.message),
            _ => self.message.clone(),
        }
    }
}

const EVENT_END: &str = "</log4j:Event>";
const CDATA_START: &str = "<![CDATA[";
const CDATA_END: &str = "]]>";

/// Reassembles `LegacyXMLLayout` records from a line-oriented stream.
///
/// Message text is untrusted (chat, server names, mod output), so markup is
/// only recognized outside CDATA sections. The layout splits a literal `]]>`
/// in a message into `]]>]]&gt;<![CDATA[`.
#[derive(Debug, Default)]
pub struct Log4jReader {
    buffer: String,
    /// How much of `buffer` the record-end scan has already passed.
    scanned: usize,
    /// Whether that scan stopped inside a CDATA section.
    in_cdata: bool,
}

impl Log4jReader {
    /// Largest a partial record may grow before it is given up on.
    ///
    /// A record that never closes means the layout is not what this reader
    /// expects, or the process died mid-write. Either way the buffered text is
    /// the user's output and has to be released rather than accumulated
    /// forever.
    const MAX_BUFFERED: usize = 1024 * 1024;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one line, returning whatever records it completed.
    pub fn push(&mut self, line: &str) -> Vec<LogRecord> {
        if self.buffer.is_empty() && !line.trim_start().starts_with("<log4j:Event") {
            return vec![LogRecord::plain(line)];
        }

        self.buffer.push_str(line);
        self.buffer.push('\n');

        let mut records = Vec::new();
        while let Some(end) = self.find_event_end() {
            let record = self.buffer[..end - EVENT_END.len()].to_owned();
            self.buffer.drain(..end);
            self.scanned = 0;
            self.in_cdata = false;
            records.push(parse_event(&record));
        }

        if self.buffer.len() > Self::MAX_BUFFERED {
            records.extend(self.flush());
        }
        // Whitespace between records is not output; keeping it would emit an
        // empty line after every single log line.
        if self.buffer.trim().is_empty() {
            self.buffer.clear();
            self.scanned = 0;
            self.in_cdata = false;
        }
        records
    }

    /// Finds the end of the first complete record, as an index just past its
    /// closing tag, skipping over CDATA sections.
    ///
    /// Resumes where the previous call stopped. Lines are buffered whole, so
    /// no tag straddles the end of the buffer.
    fn find_event_end(&mut self) -> Option<usize> {
        loop {
            let rest = &self.buffer[self.scanned..];
            if self.in_cdata {
                let Some(at) = rest.find(CDATA_END) else {
                    self.scanned = self.buffer.len();
                    return None;
                };
                self.scanned += at + CDATA_END.len();
                self.in_cdata = false;
                continue;
            }
            let Some(at) = rest.find('<') else {
                self.scanned = self.buffer.len();
                return None;
            };
            let tail = &rest[at..];
            if tail.starts_with(EVENT_END) {
                return Some(self.scanned + at + EVENT_END.len());
            }
            if tail.starts_with(CDATA_START) {
                self.scanned += at + CDATA_START.len();
                self.in_cdata = true;
            } else {
                self.scanned += at + 1;
            }
        }
    }

    /// Releases anything still buffered, as plain text.
    ///
    /// Called when the stream ends. A process killed part-way through writing a
    /// record leaves a fragment, and a fragment the user never sees is a
    /// fragment that might have said why it died.
    pub fn flush(&mut self) -> Vec<LogRecord> {
        let pending = std::mem::take(&mut self.buffer);
        self.scanned = 0;
        self.in_cdata = false;
        if pending.trim().is_empty() {
            return Vec::new();
        }
        pending.lines().map(LogRecord::plain).collect()
    }
}

/// Pulls the fields out of one `<log4j:Event>` block.
fn parse_event(block: &str) -> LogRecord {
    let mut message = element_text(block, "log4j:Message").unwrap_or_default();
    if let Some(throwable) = element_text(block, "log4j:Throwable") {
        // A stack trace belongs with the message it explains; splitting them
        // into two records loses which line the trace came from.
        if !message.is_empty() {
            message.push('\n');
        }
        message.push_str(throwable.trim_end());
    }
    let opening = opening_tag(block);
    LogRecord {
        level: attribute(opening, "level"),
        logger: attribute(opening, "logger"),
        thread: attribute(opening, "thread"),
        message,
    }
}

/// The `<log4j:Event ...>` tag itself, so attributes are never read out of
/// the message that follows it.
fn opening_tag(block: &str) -> &str {
    let start = block.find("<log4j:Event").unwrap_or(0);
    let mut quoted = false;
    for (offset, c) in block[start..].char_indices() {
        match c {
            '"' => quoted = !quoted,
            '>' if !quoted => return &block[start..start + offset],
            _ => {}
        }
    }
    &block[start..]
}

/// Reads `name="value"` from the opening tag.
fn attribute(opening: &str, name: &str) -> Option<String> {
    let needle = format!(" {name}=\"");
    let start = opening.find(&needle)? + needle.len();
    let rest = opening.get(start..)?;
    let end = rest.find('"')?;
    Some(unescape(&rest[..end]))
}

/// Reads the text of the first `<tag>` element outside any CDATA section.
///
/// CDATA sections are taken verbatim and the text between them is unescaped,
/// which reassembles a `]]>` the layout split across sections.
fn element_text(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");

    let mut at = 0;
    let content = loop {
        let rest = &block[at..];
        let lt = rest.find('<')?;
        let tail = &rest[lt..];
        if tail.starts_with(&open) {
            break &tail[open.len()..];
        }
        at += lt;
        at += if let Some(body) = tail.strip_prefix(CDATA_START) {
            CDATA_START.len() + body.find(CDATA_END)? + CDATA_END.len()
        } else {
            1
        };
    };

    // (is CDATA, text); the first and last pieces are always plain text.
    let mut pieces: Vec<(bool, &str)> = Vec::new();
    let mut rest = content;
    loop {
        let lt = rest.find('<')?;
        let tail = &rest[lt..];
        if let Some(body) = tail.strip_prefix(CDATA_START) {
            pieces.push((false, &rest[..lt]));
            let end = body.find(CDATA_END)?;
            pieces.push((true, &body[..end]));
            rest = &body[end + CDATA_END.len()..];
        } else if tail.starts_with(&close) {
            pieces.push((false, &rest[..lt]));
            break;
        } else {
            pieces.push((false, &rest[..=lt]));
            rest = &rest[lt + 1..];
        }
    }

    let last = pieces.len() - 1;
    let mut text = String::new();
    for (index, (is_cdata, piece)) in pieces.into_iter().enumerate() {
        if is_cdata {
            text.push_str(piece);
            continue;
        }
        let piece = if index == 0 {
            piece.trim_start()
        } else {
            piece
        };
        let piece = if index == last {
            piece.trim_end()
        } else {
            piece
        };
        text.push_str(&unescape(piece));
    }
    Some(text.trim_end_matches('\n').to_owned())
}

/// The five XML entities log4j's layout escapes in an attribute.
fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn feed(lines: &[&str]) -> Vec<LogRecord> {
        let mut reader = Log4jReader::new();
        let mut records: Vec<LogRecord> = lines.iter().flat_map(|line| reader.push(line)).collect();
        records.extend(reader.flush());
        records
    }

    #[test]
    fn a_record_becomes_one_readable_line() {
        let records = feed(&[
            r#"<log4j:Event logger="net.minecraft.client.main.Main" timestamp="1754000000000" level="INFO" thread="main">"#,
            r"<log4j:Message><![CDATA[Setting user: Notch]]></log4j:Message>",
            r"</log4j:Event>",
        ]);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "Setting user: Notch");
        assert_eq!(records[0].level.as_deref(), Some("INFO"));
        assert_eq!(records[0].thread.as_deref(), Some("main"));
        assert_eq!(
            records[0].logger.as_deref(),
            Some("net.minecraft.client.main.Main")
        );
        assert_eq!(records[0].display(), "[main/INFO] Setting user: Notch");
    }

    #[test]
    fn several_records_in_one_stream_stay_separate() {
        let records = feed(&[
            r#"<log4j:Event logger="a" timestamp="1" level="INFO" thread="main">"#,
            r"<log4j:Message><![CDATA[first]]></log4j:Message>",
            r"</log4j:Event>",
            r#"<log4j:Event logger="b" timestamp="2" level="WARN" thread="Render thread">"#,
            r"<log4j:Message><![CDATA[second]]></log4j:Message>",
            r"</log4j:Event>",
        ]);

        assert_eq!(records.len(), 2);
        assert_eq!(records[1].display(), "[Render thread/WARN] second");
    }

    #[test]
    fn a_throwable_stays_attached_to_the_message_it_explains() {
        let records = feed(&[
            r#"<log4j:Event logger="a" timestamp="1" level="ERROR" thread="main">"#,
            r"<log4j:Message><![CDATA[Crash!]]></log4j:Message>",
            r"<log4j:Throwable><![CDATA[java.lang.RuntimeException: boom",
            r"	at net.minecraft.Foo.bar(Foo.java:1)]]></log4j:Throwable>",
            r"</log4j:Event>",
        ]);

        assert_eq!(records.len(), 1);
        assert!(
            records[0]
                .message
                .starts_with("Crash!\njava.lang.RuntimeException: boom")
        );
        assert!(records[0].message.contains("at net.minecraft.Foo.bar"));
    }

    #[test]
    fn output_that_is_not_a_log4j_record_reaches_the_user_untouched() {
        // A JVM that refuses to start says so before log4j exists, and that is
        // exactly the line a user needs.
        let records = feed(&[
            "Error occurred during initialization of VM",
            "Could not reserve enough space for 8388608KB object heap",
        ]);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].level, None);
        assert_eq!(
            records[0].display(),
            "Error occurred during initialization of VM"
        );
    }

    #[test]
    fn a_record_cut_off_by_a_killed_process_is_still_shown() {
        let mut reader = Log4jReader::new();
        reader.push(r#"<log4j:Event logger="a" timestamp="1" level="INFO" thread="main">"#);
        reader.push(r"<log4j:Message><![CDATA[half a li");

        let flushed = reader.flush();
        assert!(!flushed.is_empty(), "a fragment might say why it died");
        assert!(
            flushed
                .iter()
                .any(|record| record.message.contains("half a li"))
        );
    }

    #[test]
    fn escaped_characters_in_an_attribute_are_restored() {
        let records = feed(&[
            r#"<log4j:Event logger="a&amp;b" timestamp="1" level="INFO" thread="Server thread">"#,
            r"<log4j:Message><![CDATA[ok]]></log4j:Message>",
            r"</log4j:Event>",
        ]);
        assert_eq!(records[0].logger.as_deref(), Some("a&b"));
    }

    #[test]
    fn a_message_without_cdata_is_still_read() {
        // The layout uses CDATA, but a message with no special characters can
        // arrive bare; dropping it would silently lose a line.
        let records = feed(&[
            r#"<log4j:Event logger="a" timestamp="1" level="INFO" thread="main">"#,
            r"<log4j:Message>plain text</log4j:Message>",
            r"</log4j:Event>",
        ]);
        assert_eq!(records[0].message, "plain text");
    }

    /// How log4j's layout writes a message: a literal `]]>` is split across
    /// sections as `]]>]]&gt;<![CDATA[`.
    fn layout_cdata(text: &str) -> String {
        format!("<![CDATA[{}]]>", text.replace("]]>", "]]>]]&gt;<![CDATA["))
    }

    fn event_with_message(message: &str) -> String {
        format!(
            r#"<log4j:Event logger="a" timestamp="1" level="INFO" thread="main"><log4j:Message>{}</log4j:Message></log4j:Event>"#,
            layout_cdata(message)
        )
    }

    #[test]
    fn closing_event_tag_text_inside_a_message_stays_in_the_message() {
        let records = feed(&[
            r#"<log4j:Event level="INFO" thread="main"><log4j:Message><![CDATA[before </log4j:Event> after]]></log4j:Message></log4j:Event>"#,
        ]);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "before </log4j:Event> after");
        assert_eq!(records[0].level.as_deref(), Some("INFO"));
    }

    #[test]
    fn closing_event_tag_text_on_a_continuation_line_stays_in_the_message() {
        let records = feed(&[
            r#"<log4j:Event level="INFO" thread="main">"#,
            r"<log4j:Message><![CDATA[first",
            r"</log4j:Event>",
            r"last]]></log4j:Message>",
            r"</log4j:Event>",
        ]);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "first\n</log4j:Event>\nlast");
    }

    #[test]
    fn closing_message_and_throwable_tag_text_inside_cdata_stays_in_the_field() {
        let records = feed(&[
            r#"<log4j:Event level="ERROR" thread="main">"#,
            r"<log4j:Message><![CDATA[a </log4j:Message> b]]></log4j:Message>",
            r"<log4j:Throwable><![CDATA[boom </log4j:Throwable> still the trace]]></log4j:Throwable>",
            r"</log4j:Event>",
        ]);

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].message,
            "a </log4j:Message> b\nboom </log4j:Throwable> still the trace"
        );
    }

    #[test]
    fn a_fake_event_inside_a_message_does_not_become_a_record() {
        let injected = r#"chat </log4j:Event><log4j:Event level="ERROR" thread="evil"><log4j:Message><![CDATA[fake]]></log4j:Message></log4j:Event>"#;
        let records = feed(&[&event_with_message(injected)]);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, injected);
        assert_eq!(records[0].level.as_deref(), Some("INFO"));
        assert_eq!(records[0].thread.as_deref(), Some("main"));
    }

    #[test]
    fn a_message_split_across_cdata_sections_is_joined_back_together() {
        let records = feed(&[&event_with_message("a]]>b]]>c")]);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "a]]>b]]>c");
    }

    #[test]
    fn record_boundaries_survive_a_message_that_ends_with_a_cdata_terminator() {
        let records = feed(&[
            &event_with_message("ends with ]]>"),
            &event_with_message("next"),
        ]);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].message, "ends with ]]>");
        assert_eq!(records[1].message, "next");
    }

    #[test]
    fn markup_lookalikes_in_a_message_are_not_read_as_fields() {
        let records = feed(&[
            r#"<log4j:Event logger="a" level="INFO">"#,
            &format!(
                "<log4j:Message>{}</log4j:Message>",
                layout_cdata(r#"x thread="fake" <log4j:Throwable>not a trace</log4j:Throwable>"#)
            ),
            r"</log4j:Event>",
        ]);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].thread, None);
        assert_eq!(
            records[0].message,
            r#"x thread="fake" <log4j:Throwable>not a trace</log4j:Throwable>"#
        );
    }

    #[test]
    fn a_record_that_never_closes_is_released_rather_than_buffered_forever() {
        let mut reader = Log4jReader::new();
        reader.push(r#"<log4j:Event logger="a" timestamp="1" level="INFO" thread="main">"#);
        let mut released = Vec::new();
        for _ in 0..2000 {
            released.extend(reader.push(&"x".repeat(1024)));
        }
        assert!(
            !released.is_empty(),
            "output must not be held hostage by a layout this reader does not know"
        );
    }
}
