use super::{CommandResult, Context};
use crate::cli::InboxArgs;
use crate::error::AppResult;
use crate::output::{self, InboxItem, InboxOutput};
use crate::{mail_files, parse_mail};

pub fn run(context: &Context, args: InboxArgs, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let room = context.resolved_room(args.room, &rooms)?;
    let (inbox, _) = context.mailbox_dirs(&room)?;
    let mut unread = Vec::new();
    for path in mail_files(&inbox)? {
        let Ok(mail) = parse_mail(&path) else {
            continue;
        };
        unread.push(InboxItem {
            id: mail.envelope.id,
            from: mail.envelope.from,
            kind: mail.envelope.kind,
            subject: mail.envelope.subject,
            sent: mail.envelope.sent,
        });
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
    };
    Ok(CommandResult::success(output::json(&output, pretty)?))
}
