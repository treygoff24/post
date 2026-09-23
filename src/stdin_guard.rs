//! A2: does stdin carry a body on a command that will not read it?
//!
//! `post chat <channel> <<EOF ... EOF` without `--send` used to be a consuming
//! read: the unread backlog was marked seen and the body was dropped without a
//! word. This probe lets a read refuse when stdin actually carries input,
//! without ever refusing on "stdin is not a terminal" alone: agent harnesses
//! run every command with stdin from `/dev/null`, and hooks pipe empty input.
//!
//! The verdict is decided by the kind of stdin, its readiness, and at most one
//! byte of its read result -- never by draining it:
//!
//! - an interactive terminal is never probed (nothing typed is not a body);
//! - a regular file is decided by the bytes left after the current offset;
//! - anything else (pipe, socket, character or block device) waits for
//!   readiness up to a bound, then reads one byte: EOF is a normal read, a byte
//!   is a body. `/dev/null` is immediately ready and reads EOF, so it costs no
//!   wait; `/dev/zero` is ready and has data, so it is refused.
//!
//! A pipe still open and silent at the bound is ambiguous. No finite wait can
//! see a producer that writes later, so that case is refused as ambiguous and
//! never treated as a silent normal read.
//!
//! Readiness uses `select`, not `poll`: macOS `poll` reports POLLNVAL for
//! character devices, including `/dev/null`.

use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// The longest a read waits for an open, silent stdin to show data or EOF.
pub(crate) const READINESS_BOUND: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StdinVerdict {
    /// Nothing is queued: a terminal, EOF, an empty or fully consumed file, or
    /// a stdin that cannot supply bytes at all.
    Clear,
    /// At least one byte is queued. At most one byte was consumed to learn it.
    Queued,
    /// Still open and silent after the readiness bound.
    Ambiguous,
}

/// Classify `fd` (stdin in production) without consuming more than one byte.
pub(crate) fn probe(fd: RawFd, bound: Duration) -> StdinVerdict {
    // SAFETY: isatty and fstat only inspect the descriptor.
    if unsafe { libc::isatty(fd) } == 1 {
        return StdinVerdict::Clear;
    }
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        // Closed or invalid stdin: there is nothing a caller could have sent.
        return StdinVerdict::Clear;
    }
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFREG => {
            // SAFETY: lseek with SEEK_CUR and offset 0 reads the offset only.
            let offset = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
            let remaining = if offset >= 0 {
                stat.st_size - offset
            } else {
                stat.st_size
            };
            if remaining > 0 {
                StdinVerdict::Queued
            } else {
                StdinVerdict::Clear
            }
        }
        libc::S_IFDIR => StdinVerdict::Clear,
        _ => {
            if !wait_readable(fd, bound) {
                return StdinVerdict::Ambiguous;
            }
            read_one_byte(fd)
        }
    }
}

/// Whether `fd` becomes readable (data or EOF) within `bound`.
fn wait_readable(fd: RawFd, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // SAFETY: fd_set is plain data, initialized by FD_ZERO before use, and
        // fd is a small open descriptor (stdin in production).
        let mut readable: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut readable);
            libc::FD_SET(fd, &mut readable);
        }
        let mut timeout = libc::timeval {
            tv_sec: remaining.as_secs() as libc::time_t,
            tv_usec: remaining.subsec_micros() as libc::suseconds_t,
        };
        let ready = unsafe {
            libc::select(
                fd + 1,
                &mut readable,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut timeout,
            )
        };
        if ready > 0 {
            return true;
        }
        if ready == 0 {
            return false;
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            && !remaining.is_zero()
        {
            continue;
        }
        // select itself failed. Reading now could block forever on a silent
        // pipe, so report "not ready": the caller refuses as ambiguous.
        return false;
    }
}

/// Read at most one byte from a descriptor `select` reported readable.
fn read_one_byte(fd: RawFd) -> StdinVerdict {
    let mut byte = 0_u8;
    loop {
        // SAFETY: reads at most one byte into a one-byte local buffer.
        let read = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        if read > 0 {
            return StdinVerdict::Queued;
        }
        if read == 0 {
            return StdinVerdict::Clear;
        }
        let error = std::io::Error::last_os_error();
        match error.kind() {
            std::io::ErrorKind::Interrupted => continue,
            // Readiness raced away; waiting again could block, so refuse.
            std::io::ErrorKind::WouldBlock => return StdinVerdict::Ambiguous,
            // EISDIR, EBADF, EIO...: this stdin cannot supply a body.
            _ => return StdinVerdict::Clear,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::net::UnixStream;

    fn timed(fd: RawFd) -> (StdinVerdict, Duration) {
        let started = Instant::now();
        let verdict = probe(fd, READINESS_BOUND);
        (verdict, started.elapsed())
    }

    #[test]
    fn dev_null_is_a_normal_read_with_no_wait() {
        let null = File::open("/dev/null").expect("open /dev/null");
        let (verdict, elapsed) = timed(null.as_raw_fd());
        assert_eq!(verdict, StdinVerdict::Clear);
        assert!(
            elapsed < READINESS_BOUND / 2,
            "/dev/null must not wait for the bound: {elapsed:?}"
        );
    }

    #[test]
    fn empty_regular_file_is_a_normal_read() {
        let root = crate::test_support::test_root("stdin-empty-file");
        let path = root.join("empty");
        File::create(&path).expect("create empty file");
        let file = File::open(&path).expect("open empty file");
        let (verdict, elapsed) = timed(file.as_raw_fd());
        assert_eq!(verdict, StdinVerdict::Clear);
        assert!(elapsed < READINESS_BOUND / 2, "{elapsed:?}");
        crate::test_support::trash_test_root(&root);
    }

    #[test]
    fn nonempty_regular_file_is_queued_and_untouched() {
        let root = crate::test_support::test_root("stdin-body-file");
        let path = root.join("body");
        std::fs::write(&path, "a body").expect("write body");
        let mut file = File::open(&path).expect("open body");
        assert_eq!(
            probe(file.as_raw_fd(), READINESS_BOUND),
            StdinVerdict::Queued
        );
        let mut rest = String::new();
        file.read_to_string(&mut rest).expect("read body");
        assert_eq!(rest, "a body", "a regular file is decided without reading");
        crate::test_support::trash_test_root(&root);
    }

    #[test]
    fn fully_consumed_regular_file_is_a_normal_read() {
        let root = crate::test_support::test_root("stdin-consumed-file");
        let path = root.join("body");
        std::fs::write(&path, "a body").expect("write body");
        let mut file = File::open(&path).expect("open body");
        file.read_to_string(&mut String::new()).expect("consume");
        assert_eq!(
            probe(file.as_raw_fd(), READINESS_BOUND),
            StdinVerdict::Clear
        );
        crate::test_support::trash_test_root(&root);
    }

    #[test]
    fn pipe_at_eof_is_a_normal_read() {
        let (reader, writer) = std::io::pipe().expect("pipe");
        drop(writer);
        let (verdict, elapsed) = timed(reader.as_raw_fd());
        assert_eq!(verdict, StdinVerdict::Clear);
        assert!(elapsed < READINESS_BOUND / 2, "{elapsed:?}");
    }

    #[test]
    fn nonempty_pipe_is_queued_after_reading_one_byte_only() {
        let (mut reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"xyz").expect("write");
        assert_eq!(
            probe(reader.as_raw_fd(), READINESS_BOUND),
            StdinVerdict::Queued
        );
        drop(writer);
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).expect("read rest");
        assert_eq!(rest, b"yz", "one byte decides; the probe never drains");
    }

    #[test]
    fn socket_with_a_queued_byte_is_queued() {
        let (reader, mut writer) = UnixStream::pair().expect("socket pair");
        writer.write_all(b"x").expect("write");
        assert_eq!(
            probe(reader.as_raw_fd(), READINESS_BOUND),
            StdinVerdict::Queued
        );
    }

    #[test]
    fn character_device_with_data_is_queued() {
        let zero = File::open("/dev/zero").expect("open /dev/zero");
        assert_eq!(
            probe(zero.as_raw_fd(), READINESS_BOUND),
            StdinVerdict::Queued
        );
    }

    #[test]
    fn delayed_writer_inside_the_bound_is_caught() {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            writer.write_all(b"late body").expect("write");
            writer
        });
        assert_eq!(
            probe(reader.as_raw_fd(), READINESS_BOUND),
            StdinVerdict::Queued
        );
        drop(producer.join().expect("producer"));
    }

    #[test]
    fn silent_open_pipe_past_the_bound_is_ambiguous_not_a_read() {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            // A writer slower than the bound: too late, and never a read.
            let _ = writer.write_all(b"too late");
            writer
        });
        let (verdict, elapsed) = timed(reader.as_raw_fd());
        assert_eq!(verdict, StdinVerdict::Ambiguous);
        assert!(elapsed >= READINESS_BOUND, "waited the bound: {elapsed:?}");
        assert!(elapsed < Duration::from_millis(400), "bounded: {elapsed:?}");
        drop(producer.join().expect("producer"));
    }

    #[test]
    fn terminal_is_a_normal_read_with_no_probe() {
        // A pseudo-terminal nobody types into is silent forever: probing it
        // would wait out the bound and call it ambiguous.
        // SAFETY: standard pty setup; the slave path is copied out immediately.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        assert!(master >= 0, "posix_openpt");
        assert_eq!(unsafe { libc::grantpt(master) }, 0, "grantpt");
        assert_eq!(unsafe { libc::unlockpt(master) }, 0, "unlockpt");
        let name = unsafe { libc::ptsname(master) };
        assert!(!name.is_null(), "ptsname");
        let slave_path = unsafe { std::ffi::CStr::from_ptr(name) }
            .to_str()
            .expect("utf-8 pty path")
            .to_owned();
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&slave_path)
            .expect("open pty slave");
        let (verdict, elapsed) = timed(slave.as_raw_fd());
        assert_eq!(verdict, StdinVerdict::Clear);
        assert!(elapsed < READINESS_BOUND / 2, "{elapsed:?}");
        drop(slave);
        unsafe { libc::close(master) };
    }
}
