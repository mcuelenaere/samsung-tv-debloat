//! Pure display helpers shared by the prompt and manager views.
//!
//! Kept free of EFL calls so the formatting rules can be tested on the host.

/// Escape text for Elementary markup. `&` must be replaced first.
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn small(text: &str, color: &str) -> String {
    small_raw(&escape(text), color)
}

pub fn small_raw(markup: &str, color: &str) -> String {
    format!("<font_size=20><color={color}>{markup}</color></font_size>")
}

pub fn next_notify(notify: &str) -> String {
    match notify {
        "every" => "first",
        "first" => "never",
        _ => "every",
    }
    .to_owned()
}

pub fn notify_button_text(notify: &str) -> String {
    match notify {
        "first" => "Toast: first use",
        "never" => "Toast: never",
        _ => "Toast: every",
    }
    .to_owned()
}

pub fn notify_description(notify: &str) -> String {
    match notify {
        "first" => "Toast on first command after 30 min idle",
        "never" => "No toasts",
        _ => "Toast on every command",
    }
    .to_owned()
}

pub fn signature_badge(signature: &str) -> String {
    match signature {
        "unsigned_will_resign" => small(
            "Unsigned \u{2014} it will be signed with this TV's CA.",
            "#ffb74d",
        ),
        "foreign_will_resign" => small(
            "Third-party signature \u{2014} it will be re-signed with this TV's CA.",
            "#ffb74d",
        ),
        "user_ca" => small("Signed by your own CA.", "#66bb6a"),
        "samsung" => small("Signed by Samsung.", "#90a4ae"),
        _ => small(
            "Signed package \u{2014} publisher not identified.",
            "#ffb74d",
        ),
    }
}

pub fn privilege_label(uri: &str) -> String {
    let friendly = match uri {
        "http://tizen.org/privilege/internet" => "Access the internet",
        "http://tizen.org/privilege/filesystem.read" => "Read shared files",
        "http://tizen.org/privilege/filesystem.write" => "Write shared files",
        "http://tizen.org/privilege/camera" => "Use the camera",
        "http://tizen.org/privilege/microphone" => "Use the microphone",
        "http://tizen.org/privilege/location" => "Use the TV's location",
        "http://tizen.org/privilege/contacts.read" => "Read contacts",
        "http://tizen.org/privilege/contacts.write" => "Write contacts",
        "http://tizen.org/privilege/calendar.read" => "Read the calendar",
        "http://tizen.org/privilege/calendar.write" => "Write the calendar",
        "http://tizen.org/privilege/message.read" => "Read messages",
        "http://tizen.org/privilege/message.write" => "Write messages",
        "http://tizen.org/privilege/bluetooth" => "Use Bluetooth",
        "http://tizen.org/privilege/nfc" => "Use NFC",
        "http://tizen.org/privilege/download" => "Download files",
        "http://tizen.org/privilege/appmanager.launch" => "Launch other apps",
        "http://tizen.org/privilege/network.get" => "Read network information",
        "http://tizen.org/privilege/network.set" => "Change network settings",
        "http://tizen.org/privilege/systeminfo.get" => "Read system information",
        "http://tizen.org/privilege/systeminfo.set" => "Change system information",
        "http://tizen.org/privilege/telephony" => "Use telephony",
        "http://tizen.org/privilege/healthinfo" => "Read health information",
        "http://tizen.org/privilege/recorder" => "Record audio",
        "http://tizen.org/privilege/vibration" => "Control vibration",
        "http://tizen.org/privilege/alarm" => "Set alarms",
        "http://tizen.org/privilege/power" => "Control power",
        "http://tizen.org/privilege/usb.host" => "Use USB devices",
        "http://tizen.org/privilege/mediacontroller" => "Control media playback",
        "http://tizen.org/privilege/d2d.datasharing" => "Share data with nearby devices",
        _ => "",
    };
    if !friendly.is_empty() {
        return friendly.to_owned();
    }
    const PREFIX: &str = "http://tizen.org/privilege/";
    uri.strip_prefix(PREFIX)
        .map(|rest| rest.replace('.', " "))
        .unwrap_or_else(|| uri.to_owned())
}

pub fn format_size(bytes: u64) -> String {
    const KILOBYTE: u64 = 1024;
    const MEGABYTE: u64 = KILOBYTE * 1024;
    const GIGABYTE: u64 = MEGABYTE * 1024;
    if bytes >= GIGABYTE {
        format!("{:.1} GB", bytes as f64 / GIGABYTE as f64)
    } else if bytes >= MEGABYTE {
        format!("{:.1} MB", bytes as f64 / MEGABYTE as f64)
    } else if bytes >= KILOBYTE {
        format!("{:.0} KB", bytes as f64 / KILOBYTE as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Byte index just past `max_chars` characters (always a char boundary).
pub fn safe_prefix_len(text: &str, max_chars: usize) -> usize {
    text.char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

/// First line only, cut to fit; marks any cut with an ellipsis.
pub fn shorten(text: &str, max_chars: usize, already_truncated: bool) -> String {
    let mut text = text.to_owned();
    let mut cut = already_truncated;
    if let Some(index) = text.find('\n') {
        text.truncate(index);
        while text.ends_with('\r') {
            text.pop();
        }
        cut = true;
    }
    if text.chars().count() > max_chars {
        let length = safe_prefix_len(&text, max_chars.saturating_sub(1));
        text.truncate(length);
        cut = true;
    }
    if cut {
        text.push('\u{2026}');
    }
    text
}

pub fn command_markup(command: &str, already_truncated: bool) -> String {
    let mut lines = Vec::new();
    let mut truncated = already_truncated;
    for raw_line in command.replace("\r\n", "\n").split('\n') {
        let mut line = raw_line;
        while line.chars().count() > 72 {
            let length = safe_prefix_len(line, 72);
            if length == 0 {
                break;
            }
            lines.push(line[..length].to_owned());
            line = &line[length..];
        }
        lines.push(line.to_owned());
        if lines.len() > 12 {
            break;
        }
    }
    if lines.len() > 12 {
        lines.truncate(12);
        truncated = true;
    }
    let mut builder = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            builder.push_str("<br/>");
        }
        builder.push_str(&escape(line));
    }
    if truncated {
        builder.push_str("<br/><color=#8a8f99><i>Command truncated for display.</i></color>");
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_xml_markup() {
        assert_eq!(escape("a&b<c>d"), "a&amp;b&lt;c&gt;d");
    }

    #[test]
    fn shortens_to_the_first_line_and_char_boundaries() {
        assert_eq!(shorten("one\ntwo", 20, false), "one\u{2026}");
        assert_eq!(shorten("abcdef", 4, false), "abc\u{2026}");
        assert_eq!(shorten("abc", 4, false), "abc");
        assert_eq!(shorten("abc", 4, true), "abc\u{2026}");
        assert_eq!(shorten("héllo", 2, false), "h\u{2026}");
    }

    #[test]
    fn wraps_long_commands() {
        let markup = command_markup(&"x".repeat(1000), false);
        assert_eq!(markup.matches("<br/>").count(), 12);
        assert!(markup.contains("Command truncated for display."));
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn cycles_toast_modes() {
        assert_eq!(next_notify("every"), "first");
        assert_eq!(next_notify("first"), "never");
        assert_eq!(next_notify("never"), "every");
    }

    #[test]
    fn labels_privileges() {
        assert_eq!(
            privilege_label("http://tizen.org/privilege/internet"),
            "Access the internet"
        );
        assert_eq!(
            privilege_label("http://tizen.org/privilege/foo.bar"),
            "foo bar"
        );
        assert_eq!(privilege_label("custom"), "custom");
    }
}
