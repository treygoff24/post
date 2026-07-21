use crate::cli::WatchArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult};
use crate::mailbox::{mail_files, parse_mail, Context};
use crate::output::{InboxItem, WatchEvent};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) fn run(context: &Context, args: WatchArgs) -> AppResult<CommandResult> {
    let (room, inbox, _) = context.resolved_mailbox_dirs(args.room)?;
    // A typo'd --room silently watches a fresh empty mailbox forever, so
    // unlike inbox (whose empty listing is immediately visible) watch warns.
    if !context.load_rooms()?.contains_key(&room) {
        eprintln!("post: warning: room '{room}' is not registered; watching a new empty mailbox");
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    loop {
        let batch = scan_batch(&room, &inbox, &mut seen)?;
        if !batch.is_empty() {
            emit(&batch, args.text)?;
            if args.once {
                return Ok(CommandResult::success(String::new()));
            }
        }
        std::thread::sleep(Duration::from_millis(args.interval_ms));
    }
}

fn scan_batch(room: &str, inbox: &Path, seen: &mut HashSet<PathBuf>) -> AppResult<Vec<WatchEvent>> {
    let mut batch = Vec::new();
    for path in mail_files(inbox)? {
        if !seen.insert(path.clone()) {
            continue;
        }
        match parse_mail(&path) {
            Ok(mail) => batch.push(WatchEvent::mail(room, InboxItem::from(mail.envelope))),
            // Consumed by a concurrent read between scan and parse: no longer unread.
            Err(_) if !path.exists() => {}
            Err(error) => {
                eprintln!(
                    "post: warning: unreadable mail '{}': {}",
                    path.display(),
                    error.message
                );
                // Ring anyway — a malformed delivery must not silence the
                // doorbell — but echo nothing from the file except its name.
                let id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("<non-utf8 filename>")
                    .to_owned();
                batch.push(WatchEvent::unreadable(room, id));
            }
        }
    }
    Ok(batch)
}

fn emit(batch: &[WatchEvent], text: bool) -> AppResult<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for event in batch {
        let line = if text {
            event.text_line()
        } else {
            crate::output::json(event, false)?
        };
        output
            .write_all(line.as_bytes())
            .and_then(|_| output.flush())
            .map_err(|error| AppError::io("write watch event", Path::new("<stdout>"), error))?;
    }
    Ok(())
}
