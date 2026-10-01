// Diagnostics on stderr must never panic. std's `eprintln!` panics when the
// write fails, and a caller that closes the pipe early (`post ... 2>&1 |
// head -1`) turned a finished command into exit 101. These definitions come
// before every `mod`, so they shadow std's macros crate-wide: a stderr line
// that cannot be written is dropped, and the command's own exit code and
// effects are unchanged. New code gets the same behavior by default.
macro_rules! eprintln {
    () => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr());
    }};
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

#[allow(unused_macros)]
macro_rules! eprint {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stderr(), $($arg)*);
    }};
}

mod app;
mod avatar;
mod bridge_topology;
mod channel;
mod channel_archive;
mod channel_state;
mod cli;
mod command_result;
mod commands;
mod cursor_state;
mod emote;
mod error;
mod imports;
mod lineage;
mod lineage_store;
mod mailbox;
mod migration_fence;
mod model;
pub mod output;
mod participant;
mod peers;
pub use commands::watch::sanitize_preview;
mod presence;
mod profile;
mod stdin_guard;
#[cfg(test)]
mod test_support;

pub use app::entry;
