//! UTC calendar time in the two encodings ISO 9660 uses.

use std::time::{SystemTime, UNIX_EPOCH};

/// A date and time in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timestamp {
    pub(crate) year: i64,
    pub(crate) month: u8,
    pub(crate) day: u8,
    pub(crate) hour: u8,
    pub(crate) minute: u8,
    pub(crate) second: u8,
}

impl Timestamp {
    /// Converts a system time to UTC calendar fields. Times before 1970 are
    /// treated as the epoch, and times past `i64` seconds saturate.
    pub(crate) fn from_system_time(t: SystemTime) -> Self {
        let secs = t
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month: month as u8,
            day: day as u8,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
        }
    }

    /// The 7-byte directory record form (years since 1900, UTC).
    pub(crate) fn record(&self) -> [u8; 7] {
        let year = (self.year - 1900).clamp(0, 255) as u8;
        [
            year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            0,
        ]
    }

    /// The 17-byte volume descriptor form (`YYYYMMDDHHMMSScc` + offset).
    pub(crate) fn descriptor(&self) -> [u8; 17] {
        let s = format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}00",
            self.year.clamp(0, 9999),
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        );
        let mut out = [0u8; 17];
        out[..16].copy_from_slice(s.as_bytes());
        out
    }
}
