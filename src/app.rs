use crate::cli;
use crate::command_result::CommandResult;
use crate::commands;
use crate::error::{AppError, AppResult};
use crate::output;
use clap::error::ErrorKind;
use clap::Parser;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

pub fn entry<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match cli::Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                return if error.print().is_ok() { 0 } else { 70 };
            }
            _ => {
                let message = error.to_string().trim().to_owned();
                let mut error = AppError::invalid_argument(message.clone())
                    .reason("command-line parse failure");
                if let Some((fix, guidance)) = parse_failure_fix(&message, &argv) {
                    error.suggested_fix = guidance;
                    error = error.exact_fix(fix);
                }
                output::write_error(&error, false);
                return error.exit_code;
            }
        },
    };
    let pretty = cli.pretty;
    match commands::execute(cli) {
        Ok(result) => match finish_command_result(result, &mut std::io::stdout().lock()) {
            Ok(exit_code) => exit_code,
            Err(error) => {
                output::write_error(&error, pretty);
                error.exit_code
            }
        },
        Err(error) => {
            output::write_error(&error, pretty);
            error.exit_code
        }
    }
}

/// Turn a clap parse failure into a fix that names the right invocation for
/// the subcommand that was actually attempted. A flag accepted on one
/// subcommand and rejected on another otherwise produces a bare "unexpected
/// argument" that offers no alternative.
/// Returns the command to run and the prose explaining it. `exact_fix` stays a
/// bare command so a caller can run or template it directly; the reasoning
/// goes to `suggested_fix`, which is the field that carries prose.
fn parse_failure_fix(message: &str, argv: &[OsString]) -> Option<(String, String)> {
    let subcommand = subcommand_of(argv);
    let subcommand = subcommand.as_deref();
    // Agents keep typing `post send <room> --body …`; the positional is a body
    // FILE, so clap reports a FILE/--body conflict that hides the real mistake.
    if message.contains("'[FILE]' cannot be used with '--body") {
        return Some((
            "post send --to <ROOM> --from <NAME> --subject <SUBJECT> --body <TEXT>".to_owned(),
            "The recipient is named by --to, never by position: the positional argument is a body FILE. Pass the recipient as a flag and the message as --body."
                .to_owned(),
        ));
    }
    if message.contains("unexpected argument '--room'") {
        return Some(match subcommand {
            Some("chat") => (
                "post chat <CHANNEL>".to_owned(),
                "Channel identity comes from cwd, so chat has no --room: run it from inside the room's registered directory. Run `post rooms` to see the paths."
                    .to_owned(),
            ),
            Some("channels") => (
                "post channels".to_owned(),
                "channels takes no --room; it lists every channel with its members.".to_owned(),
            ),
            Some("send") => (
                "post send --to <ROOM> --from <NAME> --body <TEXT>".to_owned(),
                "send names the recipient with --to and the sender with --from; it has no --room."
                    .to_owned(),
            ),
            _ => (
                "post inbox --room <ROOM>".to_owned(),
                "--room is a command option for inbox, read, and watch only.".to_owned(),
            ),
        });
    }
    if message.contains("unexpected argument '--from'") {
        return Some(match subcommand {
            Some("chat") => (
                "post chat <CHANNEL> --send --body <TEXT>".to_owned(),
                "Channel sender identity comes from cwd, so chat has no --from: run it from inside the room's registered directory."
                    .to_owned(),
            ),
            _ => (
                "post send --to <ROOM> --from <NAME> --body <TEXT>".to_owned(),
                "--from names the sender on send only.".to_owned(),
            ),
        });
    }
    if message.contains("unexpected argument '--body-file'")
        || message.contains("unexpected argument '--body'")
    {
        return Some((
            format!("post {} --help", subcommand.unwrap_or("<command>")),
            "--body and --body-file supply a message body on `send` and `chat --send` only."
                .to_owned(),
        ));
    }
    None
}

/// First non-flag token after the program name.
fn subcommand_of(argv: &[OsString]) -> Option<String> {
    argv.iter()
        .skip(1)
        .filter_map(|value| value.to_str())
        .find(|value| !value.starts_with('-'))
        .map(str::to_owned)
}

fn finish_command_result<W: Write>(mut result: CommandResult, stdout: &mut W) -> AppResult<i32> {
    if let Err(source) = stdout
        .write_all(result.stdout.as_bytes())
        .and_then(|_| stdout.flush())
    {
        if result.registration_committed {
            return Ok(result.exit_code);
        }
        return Err(if result.delivery_committed {
            AppError::delivered_output_failure(source)
        } else {
            AppError::io("write stdout", Path::new("<stdout>"), source)
        });
    }
    if let Some(action) = result.after_stdout.take() {
        action()?;
    }
    Ok(result.exit_code)
}

#[cfg(test)]
mod tests {
    use super::finish_command_result;
    use crate::command_result::CommandResult;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;
    use std::io;

    struct BrokenWriter;

    impl io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "broken test pipe",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stdout_failure_leaves_read_mail_unmoved_and_delivered_send_nonretryable() {
        let root = test_root("stdout");
        let inbox = root.join("inbox.mail");
        fs::write(&inbox, "mail").expect("create unread mail");
        let read = root.join("read.mail");
        let result = CommandResult::after_stdout("mail\n".to_owned(), move || {
            fs::rename(&inbox, &read)
                .map_err(|error| crate::error::AppError::io("mark read", &read, error))
        });
        let error = finish_command_result(result, &mut BrokenWriter)
            .expect_err("broken stdout must stop the read rename");
        assert!(root.join("inbox.mail").exists());
        assert!(!root.join("read.mail").exists());
        assert!(error.retryable);

        let error = finish_command_result(
            CommandResult::committed("receipt\n".to_owned()),
            &mut BrokenWriter,
        )
        .expect_err("broken receipt output must fail");
        assert!(!error.retryable);
        assert_eq!(error.code, crate::error::ErrorCode::DeliveredOutputFailure);
        assert_eq!(error.exit_code, 70);
        trash_test_root(&root);
    }

    #[test]
    fn stdout_failure_after_room_registration_is_still_success() {
        let result = CommandResult::success("rooms\n".to_owned()).registration_committed();

        assert_eq!(
            finish_command_result(result, &mut BrokenWriter)
                .expect("a committed registration must not invite a retry"),
            0
        );
    }
}
