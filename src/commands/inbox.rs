use crate::cli::InboxArgs;
use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::mailbox::{mail_files, parse_mail, Context};
use crate::output::{InboxItem, InboxOutput};

pub fn run(context: &Context, args: InboxArgs, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let room = context.resolved_room(args.room, &rooms)?;
    let (inbox, _) = context.mailbox_dirs(&room)?;
    let mut unread = Vec::new();
    let mut skipped_unreadable = 0;
    for path in mail_files(&inbox)? {
        let mail = match parse_mail(&path) {
            Ok(mail) => mail,
            Err(error) => {
                if error.code == ErrorCode::IoError {
                    skipped_unreadable += 1;
                    eprintln!(
                        "post: warning: skipped unreadable mail '{}': {}",
                        path.display(),
                        error.message
                    );
                } else {
                    eprintln!(
                        "post: warning: skipped malformed mail '{}': {}",
                        path.display(),
                        error.message
                    );
                }
                continue;
            }
        };
        unread.push(InboxItem::from(mail.envelope));
    }
    unread.sort_by(|left, right| left.id.cmp(&right.id));
    let count = unread.len();
    if args.text {
        let rendered = if unread.is_empty() {
            format!("post: inbox empty for {room}\n")
        } else {
            let mut rendered = format!("post: inbox for {room} ({count} unread)\n");
            for mail in unread {
                let subject = if mail.subject.is_empty() {
                    String::new()
                } else {
                    format!("  {:?}", mail.subject)
                };
                rendered.push_str(&format!(
                    "{}  [{}] from {}{}\n",
                    mail.id, mail.kind, mail.from, subject
                ));
            }
            rendered
        };
        return Ok(CommandResult::success(rendered));
    }
    let output = InboxOutput {
        ok: true,
        room,
        unread,
        count,
        skipped_unreadable,
    };
    CommandResult::json(&output, pretty)
}
