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
    let cli = match cli::Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                return if error.print().is_ok() { 0 } else { 70 };
            }
            _ => {
                let error = AppError::invalid_argument(error.to_string().trim().to_owned())
                    .reason("command-line parse failure");
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

fn finish_command_result<W: Write>(mut result: CommandResult, stdout: &mut W) -> AppResult<i32> {
    stdout
        .write_all(result.stdout.as_bytes())
        .and_then(|_| stdout.flush())
        .map_err(|source| {
            if result.delivery_committed {
                AppError::delivered_output_failure(source)
            } else {
                AppError::io("write stdout", Path::new("<stdout>"), source)
            }
        })?;
    if let Some(action) = result.after_stdout.take() {
        action()?;
    }
    Ok(result.exit_code)
}

#[cfg(test)]
mod tests {
    use super::finish_command_result;
    use crate::command_result::CommandResult;
    use std::fs;
    use std::io;
    use std::time::{SystemTime, UNIX_EPOCH};

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
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should follow Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("post-stdout-{nonce}"));
        fs::create_dir_all(&root).expect("create stdout test root");
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
        let cleanup = std::process::Command::new("trash")
            .arg(&root)
            .status()
            .expect("run recoverable test cleanup");
        assert!(cleanup.success(), "trash should clean stdout test root");
    }
}
