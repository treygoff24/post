use crate::cli::InboxArgs;
use crate::command_result::CommandResult;
use crate::error::{AppResult, ErrorCode};
use crate::mailbox::{mail_files, parse_mail, Context};
use crate::output::{self, InboxItem, InboxOutput};

pub(super) fn run(context: &Context, args: InboxArgs, pretty: bool) -> AppResult<CommandResult> {
    let (room, inbox, _) = context.resolved_mailbox_dirs(args.room)?;
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
        let room = output::sanitize_text_header(&room);
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
                let id = output::sanitize_text_header(&mail.id);
                let sender = output::sender_label_quoted(
                    &mail.from,
                    mail.display_name.as_deref(),
                    mail.pfp.as_deref(),
                );
                rendered.push_str(&format!(
                    "{}  [{}] from {}{}\n",
                    id, mail.kind, sender, subject
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

#[cfg(test)]
mod profile_render_tests {
    use crate::output::sender_label_quoted;

    #[test]
    fn inbox_line_sender_absent_profile_is_byte_identical() {
        // The inbox --text line built from this must match the pre-profile
        // form exactly: `from "beta"` with debug quotes.
        assert_eq!(sender_label_quoted("beta", None, None), "\"beta\"");
    }

    #[test]
    fn inbox_line_sender_renders_stamped_profile() {
        assert_eq!(
            sender_label_quoted("beta", Some("Lantern"), Some("🏮")),
            "🏮 Lantern (\"beta\")"
        );
    }
}
