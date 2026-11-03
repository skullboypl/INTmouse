//! Local wall-clock time without a date library: log lines and the connection journal should
//! show the time the user sees on the clock, not UTC.

use std::fmt;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// `(year, month, day, hour, minute, second)` in local time.
fn now() -> (u16, u16, u16, u16, u16, u16) {
    let t = unsafe { GetLocalTime() };
    (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// `17:44:12`
pub fn hms() -> String {
    let (_, _, _, h, mi, s) = now();
    format!("{h:02}:{mi:02}:{s:02}")
}

/// `2026-10-31 17:44:12`
pub fn stamp() -> String {
    let (y, mo, d, h, mi, s) = now();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Timestamps of `tracing` lines in local time.
pub struct LocalTimer;

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
        write!(w, "{}", stamp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_and_hms_have_the_expected_shape() {
        let stamp = stamp();
        assert_eq!(stamp.len(), 19, "{stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], " ");
        let hms = hms();
        assert_eq!(hms.len(), 8, "{hms}");
        assert_eq!(&hms[2..3], ":");
        assert!(stamp.ends_with(&hms) || stamp[11..13] == hms[0..2], "same clock: {stamp} vs {hms}");
    }
}
