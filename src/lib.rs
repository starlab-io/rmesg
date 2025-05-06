mod common;

pub mod entry;
pub mod error;
/// KLog Implementation (makes klogctl aka syslog system call through libc)
pub mod klogctl;
/// KMsg Implementation (reads from the /dev/kmsg file)
pub mod kmsgfile;

use std::iter::Iterator;

#[derive(Clone, Copy, Debug)]
pub enum Backend {
    Default,
    KLogCtl,
    DevKMsg,
}

pub enum EntriesIterator {
    KLogCtl(klogctl::KLogEntries),
    DevKMsg(kmsgfile::KMsgEntriesIter),
}
impl Iterator for EntriesIterator {
    type Item = Result<entry::Entry, error::RMesgError>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::KLogCtl(k) => k.next(),
            Self::DevKMsg(d) => d.next(),
        }
    }
}

pub fn log_entries(b: Backend, clear: bool) -> Result<Vec<entry::Entry>, error::RMesgError> {
    match b {
        Backend::Default => match kmsgfile::kmsg(None) {
            Ok(e) => Ok(e),
            Err(error::RMesgError::DevKMsgFileOpenError(s)) => {
                eprintln!(
                    "Falling back from device file to klogctl syscall due to error: {}",
                    s
                );
                klogctl::klog(clear)
            }
            Err(e) => Err(e),
        },
        Backend::KLogCtl => klogctl::klog(clear),
        Backend::DevKMsg => kmsgfile::kmsg(None),
    }
}

pub fn logs_raw(b: Backend, clear: bool) -> Result<String, error::RMesgError> {
    match b {
        Backend::Default => match kmsgfile::kmsg_raw(None) {
            Ok(e) => Ok(e),
            Err(error::RMesgError::DevKMsgFileOpenError(s)) => {
                eprintln!(
                    "Falling back from device file to klogctl syscall due to error: {}",
                    s
                );
                klogctl::klog_raw(clear)
            }
            Err(e) => Err(e),
        },
        Backend::KLogCtl => klogctl::klog_raw(clear),
        Backend::DevKMsg => kmsgfile::kmsg_raw(None),
    }
}

pub fn logs_iter(b: Backend, clear: bool, raw: bool) -> Result<EntriesIterator, error::RMesgError> {
    match b {
        Backend::Default => match kmsgfile::KMsgEntriesIter::with_options(None, raw) {
            Ok(e) => Ok(EntriesIterator::DevKMsg(e)),
            Err(error::RMesgError::DevKMsgFileOpenError(s)) => {
                eprintln!(
                    "Falling back from device file to klogctl syscall due to error: {}",
                    s
                );
                Ok(EntriesIterator::KLogCtl(
                    klog_entries_only_if_timestamp_enabled(clear)?,
                ))
            }
            Err(e) => Err(e),
        },
        Backend::KLogCtl => Ok(EntriesIterator::KLogCtl(
            klog_entries_only_if_timestamp_enabled(clear)?,
        )),
        Backend::DevKMsg => Ok(EntriesIterator::DevKMsg(
            kmsgfile::KMsgEntriesIter::with_options(None, raw)?,
        )),
    }
}

fn klog_entries_only_if_timestamp_enabled(
    clear: bool,
) -> Result<klogctl::KLogEntries, error::RMesgError> {
    let log_timestamps_enabled = klogctl::klog_timestamps_enabled()?;

    // ensure timestamps in logs
    if !log_timestamps_enabled {
        eprintln!("WARNING: Timestamps are disabled but tailing/following logs (as you've requested) requires them.");
        eprintln!("Aboring program.");
        eprintln!("You can enable timestamps by running the following: ");
        eprintln!("  echo Y > /sys/module/printk/parameters/time");
        return Err(error::RMesgError::KLogTimestampsDisabled);
    }

    klogctl::KLogEntries::with_options(clear, klogctl::SUGGESTED_POLL_INTERVAL)
}

/**********************************************************************************/
// Tests! Tests! Tests!

#[cfg(all(test, target_os = "linux"))]
mod test {
    use super::*;

    #[test]
    fn test_log_entries() {
        let entries = log_entries(Backend::Default, false);
        assert!(entries.is_ok(), "Response from kmsg not Ok");
        assert!(!entries.unwrap().is_empty(), "Should have non-empty logs");
    }

    #[test]
    fn test_iterator() {
        // uncomment below if you want to be extra-sure
        //let enable_timestamp_result = kernel_log_timestamps_enable(true);
        //assert!(enable_timestamp_result.is_ok());

        // Don't clear the buffer. Poll every second.
        let iterator_result = logs_iter(Backend::Default, false, false);
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
}
