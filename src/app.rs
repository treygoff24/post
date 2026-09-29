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
            // `post --version` IS `post version`: clap's own text would be a
            // second, shorter build line ("post 0.9.0") that names no commit.
            ErrorKind::DisplayVersion => {
                let program = argv.first().cloned().unwrap_or_else(|| "post".into());
                match cli::Cli::try_parse_from([program, OsString::from("version")]) {
                    Ok(cli) => cli,
                    Err(_) => return 70,
                }
            }
            ErrorKind::DisplayHelp => {
                return if error.print().is_ok() { 0 } else { 70 };
            }
            _ => {
                let message = error.to_string().trim().to_owned();
                let mut error = AppError::invalid_argument(message.clone())
                    .reason("command-line parse failure");
                if let Some((fix, guidance)) = parse_failure_fix(&message, &argv) {
                    error.suggested_fix = guidance;
                    if let Some(fix) = fix {
                        error = error.exact_fix(fix);
                    }
                }
                output::write_error(&error, false);
                return error.exit_code;
            }
        },
    };
    let pretty = cli.pretty;
    let human = human_rendering(&cli);
    let report = |error: &AppError| {
        if human {
            output::write_error_text(error);
        } else {
            output::write_error(error, pretty);
        }
    };
    match commands::execute(cli) {
        Ok(result) => match finish_process_stdout(result) {
            Ok(exit_code) => exit_code,
            Err(error) => {
                report(&error);
                error.exit_code
            }
        },
        Err(error) => {
            report(&error);
            error.exit_code
        }
    }
}

/// True when the caller chose a human-only rendering (`--text`, `--brief`):
/// its errors are prose on stderr, not a JSON envelope. `--json` always wins.
/// A command with no human flag keeps the envelope in both modes: hooks and
/// wrappers parse it (`error.code`), and the flagless default is what they run.
fn human_rendering(cli: &cli::Cli) -> bool {
    use cli::Command;
    !cli.json
        && match &cli.command {
            Command::Doctor(args) => args.brief,
            Command::Channels(args) => args.text,
            Command::Who(args) => args.text,
            Command::Inbox(args) => args.text,
            Command::Watch(args) => args.text,
            _ => false,
        }
}

fn finish_process_stdout(result: CommandResult) -> AppResult<i32> {
    #[cfg(unix)]
    {
        finish_command_result(result, &mut StrictStdout)
    }
    #[cfg(not(unix))]
    {
        finish_command_result(result, &mut std::io::stdout().lock())
    }
}

/// Rust's Unix `StdoutRaw` deliberately converts EBADF into a successful
/// write. Post cannot use that behavior at a state-commit boundary: a child
/// inheriting fd1 from a read-only regular file would otherwise consume mail
/// after emitting zero bytes. Direct libc writes retain the real descriptor
/// error while keeping `finish_command_result`'s delivery/registration rules.
#[cfg(unix)]
struct StrictStdout;

#[cfg(unix)]
impl Write for StrictStdout {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        loop {
            // SAFETY: bytes supplies a valid pointer and byte count for the
            // duration of this synchronous call; fd1 is borrowed, not closed.
            let written =
                unsafe { libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len()) };
            if written >= 0 {
                return Ok(written as usize);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Writes are unbuffered at this layer. fsync would be wrong for pipes
        // and terminals, where it returns EINVAL despite successful output.
        Ok(())
    }
}

/// Turn a clap parse failure into a fix that names the right invocation for
/// the subcommand that was actually attempted. A flag accepted on one
/// subcommand and rejected on another otherwise produces a bare "unexpected
/// argument" that offers no alternative.
/// Returns an OPTIONAL command and the prose explaining it. The command is
/// `Some` only when it runs as written; every correction here that needs a
/// value only the caller has -- a room, a name, a body -- returns `None` and
/// says it in prose instead. This function used to hand back templates like
/// `post send --to <ROOM> --body <TEXT>` as `exact_fix`, and its own comment
/// licensed that ("run or template it directly") in flat contradiction of the
/// README, which promises a command that runs as written.
fn parse_failure_fix(message: &str, argv: &[OsString]) -> Option<(Option<String>, String)> {
    let subcommand = subcommand_of(argv);
    let subcommand = subcommand.as_deref();
    // Agents keep typing `post send <room> --body …`; the bare argument is the
    // message body, so clap reports a BODY/--body conflict that hides the real
    // mistake: the room belongs in --to.
    if subcommand == Some("send")
        && message.contains("'[BODY]'")
        && message.contains("cannot be used with")
    {
        return Some((
            None,
            "The recipient is named by --to, never by position: a bare argument is the message body, so it cannot be combined with --body or --body-file. Pass the recipient as --to <ROOM> and give the body once; `post schema` lists the exact grammar."
                .to_owned(),
        ));
    }
    if subcommand == Some("profile")
        && (message.contains("unexpected argument '--name'")
            || message.contains("unexpected argument '--pfp'"))
    {
        return Some((
            // The value is the caller's, so the correction is prose: a
            // `--name <NAME>` template would not run as written.
            None,
            "`post profile` sets nothing at the top level: the spelling is `post profile set --name <NAME>` (add `--pfp <EMOJI>` in the same command to set the sigil)."
                .to_owned(),
        ));
    }
    if message.contains("unexpected argument '--room'") {
        return Some(match subcommand {
            Some("chat") => (
                // No command: the correction is to run it from another
                // directory. A stripped `post chat tax` would run fine right
                // here and send under the WRONG identity -- a correction that
                // silently does something else is worse than none.
                None,
                "Channel identity comes from cwd, so chat has no --room: run it from inside the room's registered directory. Run `post rooms` to see the paths."
                    .to_owned(),
            ),
            Some("channels") => (
                Some("post channels".to_owned()),
                "channels takes no --room; it lists every channel with its members.".to_owned(),
            ),
            Some("send") => (
                None,
                "send names the recipient with --to and the sender with --from; it has no --room."
                    .to_owned(),
            ),
            _ => (
                None,
                "--room is a command option for inbox, read, and watch only.".to_owned(),
            ),
        });
    }
    if message.contains("unexpected argument '--from'") {
        return Some(match subcommand {
            Some("chat") => (
                None,
                "Channel sender identity comes from cwd, so chat has no --from: run it from inside the room's registered directory."
                    .to_owned(),
            ),
            _ => (
                None,
                "--from names the sender on send only.".to_owned(),
            ),
        });
    }
    if message.contains("unexpected argument '--body-file'")
        || message.contains("unexpected argument '--body'")
    {
        return Some((
            // Runnable only when we know which subcommand was typed.
            subcommand.map(|name| format!("post {name} --help")),
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
            // The change landed; only the receipt could not be shown. Say so
            // where a person might still look, and exit as the command did:
            // a nonzero exit here invites a retry that would repeat the change.
            eprintln!(
                "post: the change was committed but stdout could not take the result ({source})"
            );
            return Ok(result.exit_code);
        }
        // The reader went away (`| head`, a parser that gave up). A command
        // with nothing deferred has nothing to undo and nothing to retry, so
        // the ordinary Unix answer applies: stop quietly. A command that still
        // owes a state change after its output (a consuming read) keeps the
        // error below, because that mail really is still unread.
        if source.kind() == std::io::ErrorKind::BrokenPipe
            && result.after_stdout.is_none()
            && !result.delivery_committed
        {
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

    #[test]
    fn a_send_that_landed_exits_as_it_did_when_its_receipt_cannot_be_written() {
        // `post send` returns a committed delivery that is also a committed
        // registration: the mail exists, so no stdout failure may read as a
        // failed send (a caller that retries sends a second copy).
        let landed = CommandResult::committed("receipt\n".to_owned()).registration_committed();
        assert_eq!(
            finish_command_result(landed, &mut BrokenWriter)
                .expect("a landed send must not exit nonzero"),
            0
        );
    }

    #[test]
    fn a_reader_that_went_away_ends_a_read_only_command_quietly() {
        // `post who --text | head`: nothing was changed, so there is nothing to
        // retry and no error to print. The command's own exit code stands.
        let listing = CommandResult::success("listing\n".to_owned());
        assert_eq!(
            finish_command_result(listing, &mut BrokenWriter).expect("EPIPE on a read-only result"),
            0
        );
        let mut findings = CommandResult::success("findings\n".to_owned());
        findings.exit_code = 1;
        assert_eq!(
            finish_command_result(findings, &mut BrokenWriter).expect("EPIPE keeps the exit code"),
            1
        );
    }

    #[test]
    fn only_a_closed_pipe_is_quiet_for_read_only_commands() {
        struct FullDisk;
        impl io::Write for FullDisk {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("no space left on device"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error = finish_command_result(CommandResult::success("x\n".to_owned()), &mut FullDisk)
            .expect_err("a genuine write failure is still reported");
        assert_eq!(error.code, crate::error::ErrorCode::IoError);
    }
}
