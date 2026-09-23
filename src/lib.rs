mod app;
mod channel;
mod channel_archive;
mod channel_state;
mod cli;
mod command_result;
mod commands;
mod cursor_state;
mod error;
mod lineage;
mod lineage_store;
mod mailbox;
mod migration_fence;
mod model;
pub mod output;
mod participant;
pub use commands::watch::sanitize_preview;
mod presence;
mod profile;
#[cfg(test)]
mod test_support;

pub use app::entry;
