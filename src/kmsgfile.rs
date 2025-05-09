use crate::common;
use crate::entry::{Entry, EntryParsingError};
/// This crate provides a /dev/kmsg file interface from Rust. Reading from this
/// virtual device is the more modern and simpler way to read the kernel
/// log buffer than making syscalls directly.
///
/// This allows Rust programs to consume dmesg-like output programmatically.
///
use crate::error::RMesgError;

use lazy_static::lazy_static;
use nonblock::NonBlockingReader;
use regex::Regex;
use std::fs as stdfs;

use std::io as stdio;
use std::io::BufRead;
use std::iter::Iterator;

const DEV_KMSG_PATH: &str = "/dev/kmsg";
lazy_static! {
    static ref RE_ENTRY_WITH_TIMESTAMP: Regex = Regex::new(
        r"(?x)^
            [[:space:]]*(?P<faclevstr>[[:digit:]]*)[[:space:]]*,
            # Sequence is a 64-bit integer: https://www.kernel.org/doc/Documentation/ABI/testing/dev-kmsg
            [[:space:]]*(?P<sequencenum>[[:digit:]]*)[[:space:]]*,
            [[:space:]]*(?P<timestampstr>[[:digit:]]*)[[:space:]]*,
            # Ignore everything until the semi-colon and then the semicolon
            [[^;]]*;
            (?P<message>.*)
            $"
    )
    .unwrap();
}

/// While reading the kernel log buffer is very useful in and of itself (especially when running the CLI),
/// a lot more value is unlocked when it can be tailed line-by-line.
///
/// This struct provides the facilities to do that. It implements an iterator to easily iterate
/// indefinitely over the lines.
///
/// Implements the synchronous std::iter::Iterator trait
///
pub struct KMsgEntriesIter {
    raw: bool,
    lines_iter: stdio::Lines<stdio::BufReader<stdfs::File>>,
}

impl KMsgEntriesIter {
    /// Create a new KMsgEntries with two specific options
    /// `file_override`: When `Some`, overrides the path from where to read the kernel logs
    /// `raw: bool` When set, does not parse the message and instead sets the entire log entry in the "message" field
    pub fn with_options(file_override: Option<String>, raw: bool) -> Result<Self, RMesgError> {
        let path = file_override.as_deref().unwrap_or(DEV_KMSG_PATH);

        let file = match stdfs::File::open(path) {
            Ok(fc) => fc,
            Err(e) => {
                if e.raw_os_error() == Some(libc::EPERM) {
                    return Err(RMesgError::OperationNotPermitted(format!(
                        "Open File {}",
                        path
                    )));
                } else {
                    return Err(RMesgError::DevKMsgFileOpenError(format!(
                        "Unable to open file {}: {}",
                        path, e
                    )));
                }
            }
        };

        let lines_iter = stdio::BufReader::new(file).lines();

        Ok(Self { raw, lines_iter })
    }
}

/// Trait to iterate over lines of the kernel log buffer.
impl Iterator for KMsgEntriesIter {
    type Item = Result<Entry, RMesgError>;

    /// This is a blocking call, and will use the calling thread to perform polling
    /// NOT a thread-safe method either. It is suggested this method be always
    /// blocked on to ensure no messages are missed.
    fn next(&mut self) -> Option<Self::Item> {
        match self.lines_iter.next() {
            None => None,
            Some(Err(e)) => Some(Err(RMesgError::IOError(format!(
                "Error reading next line from kernel log device file: {}",
                e
            )))),
            Some(Ok(line)) => {
                if self.raw {
                    Some(Ok(Entry {
                        facility: None,
                        level: None,
                        timestamp_from_system_start: None,
                        sequence_num: None,
                        message: line,
                    }))
                } else {
                    Some(entry_from_line(&line).map_err(|e| e.into()))
                }
            }
        }
    }
}

pub fn kmsg_raw(file_override: Option<String>) -> Result<String, RMesgError> {
    let path = file_override.as_deref().unwrap_or(DEV_KMSG_PATH);

    let file = match stdfs::File::open(path) {
        Ok(fc) => fc,
        Err(e) => {
            if e.raw_os_error() == Some(libc::EPERM) {
                return Err(RMesgError::OperationNotPermitted(format!(
                    "Open File {}",
                    path
                )));
            } else {
                return Err(RMesgError::DevKMsgFileOpenError(format!(
                    "Unable to open file {}: {}",
                    path, e
                )));
            }
        }
    };

    let mut noblock_file = NonBlockingReader::from_fd(file)?;

    let mut file_contents = String::new();
    match noblock_file.read_available_to_string(&mut file_contents) {
        Ok(_) => {}
        Err(e) => {
            if e.raw_os_error() == Some(libc::EPERM) {
                return Err(RMesgError::OperationNotPermitted(format!(
                    "Read from File {}",
                    path
                )));
            } else {
                return Err(RMesgError::DevKMsgFileOpenError(format!(
                    "Unable to read from file {}: {}",
                    path, e
                )));
            }
        }
    }

    Ok(file_contents)
}

/// This is the key safe function that makes the klogctl syslog call with parameters.
/// While the internally used function supports all klogctl parameters, this function
/// only provides one bool parameter which indicates whether the buffer is to be cleared
/// or not, after its contents have been read.
///
/// Note that this is a by-definition synchronous function. So it is available
/// whether or not "async" feature is enabled
///
pub fn kmsg(file_override: Option<String>) -> Result<Vec<Entry>, RMesgError> {
    let file_contents = kmsg_raw(file_override)?;
    let entry_results: Result<Vec<Entry>, EntryParsingError> =
        file_contents.lines().map(entry_from_line).collect();

    Ok(entry_results?)
}

// Message spec: https://github.com/torvalds/linux/blob/master/Documentation/ABI/testing/dev-kmsg
// Parses a kernel log line that looks like this (we ignore lines wtihout the timestamp):
// 5,0,0,-;Linux version 4.14.131-linuxkit (root@6d384074ad24) (gcc version 8.3.0 (Alpine 8.3.0)) #1 SMP Fri Jul 19 12:31:17 UTC 2019
// 6,1,0,-;Command, line: BOOT_IMAGE=/boot/kernel console=ttyS0 console=ttyS1 page_poison=1 vsyscall=emulate panic=1 root=/dev/sr0 text
//  LINE2=foobar
//  LINE 3 = foobar ; with semicolon
// 6,2,0,-;x86/fpu: Supporting XSAVE feature 0x001: 'x87 floating point registers'
// 6,3,0,-,more,deets;x86/fpu: Supporting XSAVE; feature 0x002: 'SSE registers'
pub fn entry_from_line(line: &str) -> Result<Entry, EntryParsingError> {
    if let Some(kmsgparts) = RE_ENTRY_WITH_TIMESTAMP.captures(line) {
        let (facility, level) = match kmsgparts.name("faclevstr") {
            Some(faclevstr) => common::parse_favlecstr(faclevstr.as_str(), line)?,
            None => (None, None),
        };

        let sequence_num = match kmsgparts.name("sequencenum") {
            Some(sequencestr) => Some(common::parse_fragment::<usize>(sequencestr.as_str(), line)?),
            None => None,
        };

        let timestamp_from_system_start = match kmsgparts.name("timestampstr") {
            Some(timestampstr) => common::parse_timestamp_microsecs(timestampstr.as_str(), line)?,
            None => None,
        };

        let message = kmsgparts["message"].to_owned();

        Ok(Entry {
            facility,
            level,
            sequence_num,
            timestamp_from_system_start,
            message,
        })
    } else {
        Ok(Entry {
            facility: None,
            level: None,
            sequence_num: None,
            timestamp_from_system_start: None,
            message: line.to_owned(),
        })
    }
}

/**********************************************************************************/
// Tests! Tests! Tests!
#[cfg(all(test, target_os = "linux"))]
mod test {
    use super::*;
    #[test]
    fn test_kmsg() {
        let entries = kmsg(None);
        assert!(entries.is_ok(), "Response from kmsg not Ok");
        assert!(!entries.unwrap().is_empty(), "Should have non-empty logs");
    }

    #[test]
    fn test_iterator() {
        // uncomment below if you want to be extra-sure
        //let enable_timestamp_result = kernel_log_timestamps_enable(true);
        //assert!(enable_timestamp_result.is_ok());

        // Don't clear the buffer. Poll every second.
        let iterator_result = KMsgEntriesIter::with_options(None, false);
        assert!(iterator_result.is_ok());

        let iterator = iterator_result.unwrap();

        // Read 10 lines and quit
        for (count, entry) in iterator.enumerate() {
            assert!(entry.is_ok());
            if count > 10 {
                break;
            }
        }
    }

    #[test]
    fn test_parse_serialize() {
        let line1 = " LINE2=foobar";
        let e1r = entry_from_line(line1);
        assert!(e1r.is_ok());
        let line1again = e1r.unwrap().to_kmsg_str().unwrap();
        assert_eq!(line1, line1again);

        let line2 = "6,779,91650777797,-;docker0: port 2(veth98d5024) entered disabled state";
        let e2r = entry_from_line(line2);
        assert!(e2r.is_ok());
        let line2again = e2r.unwrap().to_kmsg_str().unwrap();
        assert_eq!(line2, line2again);
    }
}
