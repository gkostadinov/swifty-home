//! Log output. Request lines hold timestamps, statuses, durations and sizes
//! only. Audio, transcripts and tokens never reach this module.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The log file is rotated before it would grow past this size.
pub const LOG_CAP_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone)]
pub struct Logger {
    sink: Arc<Sink>,
}

enum Sink {
    Stdout,
    File(Mutex<RotatingFile>),
    Discard,
}

/// One finished request. Only numbers: nothing from the request can end up here.
#[derive(Debug, Default, Clone, Copy)]
pub struct RequestRecord {
    pub status: u16,
    pub duration: Duration,
    pub body_bytes: u64,
    pub audio_ms: Option<u64>,
    pub decode_ms: Option<u64>,
}

/// Connections that never became a request, since the last report. Only numbers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionReport {
    pub refused_address: u64,
    pub refused_busy: u64,
    pub header_timeouts: u64,
}

impl Logger {
    pub fn stdout() -> Self {
        Self { sink: Arc::new(Sink::Stdout) }
    }

    pub fn file(path: PathBuf) -> Self {
        Self { sink: Arc::new(Sink::File(Mutex::new(RotatingFile::new(path, LOG_CAP_BYTES)))) }
    }

    pub fn discard() -> Self {
        Self { sink: Arc::new(Sink::Discard) }
    }

    /// A service lifecycle message. Callers pass fixed text, paths or numbers.
    pub fn info(&self, message: &str) {
        self.write_line(message);
    }

    pub fn request(&self, record: &RequestRecord) {
        let mut line = format!(
            "status={} duration_ms={} body_bytes={}",
            record.status,
            record.duration.as_millis(),
            record.body_bytes
        );
        if let Some(audio_ms) = record.audio_ms {
            line.push_str(&format!(" audio_ms={audio_ms}"));
        }
        if let Some(decode_ms) = record.decode_ms {
            line.push_str(&format!(" decode_ms={decode_ms}"));
        }
        self.write_line(&line);
    }

    pub fn connections(&self, report: &ConnectionReport) {
        self.write_line(&format!(
            "connections refused_address={} refused_busy={} header_timeouts={}",
            report.refused_address, report.refused_busy, report.header_timeouts
        ));
    }

    /// Logging must never take the service down, so write errors are dropped.
    fn write_line(&self, text: &str) {
        let line = format!("{} {text}\n", format_timestamp(SystemTime::now()));
        match self.sink.as_ref() {
            Sink::Stdout => {
                let mut out = io::stdout().lock();
                let _ = out.write_all(line.as_bytes());
                let _ = out.flush();
            }
            Sink::File(file) => {
                let mut file = file.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = file.write_line(line.as_bytes());
            }
            Sink::Discard => {}
        }
    }
}

/// A log file that is moved to `<name>.1` when it reaches its cap, so the log
/// occupies at most twice the cap on disk.
struct RotatingFile {
    path: PathBuf,
    cap: u64,
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    fn new(path: PathBuf, cap: u64) -> Self {
        Self { path, cap, file: None, size: 0 }
    }

    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        if self.file.is_none() {
            let (file, size) = open_append(&self.path)?;
            self.file = Some(file);
            self.size = size;
        }
        if self.size.saturating_add(line.len() as u64) > self.cap {
            self.rotate()?;
        }
        if let Some(file) = self.file.as_mut() {
            file.write_all(line)?;
            self.size = self.size.saturating_add(line.len() as u64);
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let backup = backup_path(&self.path);
        if fs::rename(&self.path, &backup).is_err() {
            // The cap still has to hold if the file cannot be moved.
            let _ = fs::remove_file(&self.path);
        }
        let (file, size) = open_append(&self.path)?;
        self.file = Some(file);
        self.size = size;
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let size = file.metadata()?.len();
    Ok((file, size))
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

/// UTC in RFC 3339 form with milliseconds.
pub fn format_timestamp(time: SystemTime) -> String {
    let total = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = total.as_secs();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let in_day = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600,
        in_day % 3_600 / 60,
        in_day % 60,
        total.subsec_millis()
    )
}

/// Proleptic Gregorian calendar date for a count of days since 1970-01-01
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64, millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(millis)
    }

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_timestamp(at(0, 0)), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_timestamp(at(951_782_399, 7)), "2000-02-28T23:59:59.007Z");
        assert_eq!(format_timestamp(at(951_782_400, 0)), "2000-02-29T00:00:00.000Z");
        assert_eq!(format_timestamp(at(1_709_251_199, 999)), "2024-02-29T23:59:59.999Z");
        assert_eq!(format_timestamp(at(1_798_761_600, 0)), "2027-01-01T00:00:00.000Z");
    }

    #[test]
    fn rotates_at_the_cap_and_keeps_one_backup() {
        let dir = std::env::temp_dir().join(format!("gs-log-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("speech.log");
        let mut file = RotatingFile::new(path.clone(), 100);

        let line = [b'x'; 40];
        for _ in 0..7 {
            file.write_line(&line).unwrap();
        }
        drop(file);

        let current = fs::metadata(&path).unwrap().len();
        let backup = fs::metadata(backup_path(&path)).unwrap().len();
        assert!(current <= 100, "current log is {current} bytes");
        assert!(backup <= 100, "backup is {backup} bytes");
        assert_eq!((current + backup) % 40, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn appends_to_an_existing_file_and_counts_its_size() {
        let dir = std::env::temp_dir().join(format!("gs-log-append-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("speech.log");
        fs::write(&path, [b'y'; 90]).unwrap();

        let mut file = RotatingFile::new(path.clone(), 100);
        file.write_line(&[b'z'; 40]).unwrap();
        drop(file);

        assert_eq!(fs::metadata(&path).unwrap().len(), 40);
        assert_eq!(fs::metadata(backup_path(&path)).unwrap().len(), 90);
        fs::remove_dir_all(&dir).unwrap();
    }
}
