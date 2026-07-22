use crate::channel;
use crate::cli::ChatArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use crate::output::{self, ChatSendOutput};

pub(super) fn run(
    context: &Context,
    args: ChatArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    if args.join {
        return join(context, &args.name, json_output, pretty);
    }
    if args.send {
        return send(context, args, json_output, pretty);
    }
    // ponytail: the read/cursor path is the coordinator's lane (contract
    // 20260722-013246); this arm is replaced by their patch.
    Err(AppError::invalid_argument(format!(
        "reading '#{}' is not built yet (read/cursor lane in flight); `--join` and `--send` are live",
        args.name
    ))
    .reason("unbuilt lane"))
}

fn join(
    context: &Context,
    name: &str,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let outcome = channel::join(context, name)?;
    let rendered = if json_output {
        output::json(
            &output::ChatJoinOutput {
                ok: true,
                channel: name.to_owned(),
                room: outcome.room.clone(),
                created: outcome.channel_created,
                already_member: outcome.already_member,
                event_id: outcome.event_id.clone(),
            },
            pretty,
        )?
    } else if outcome.already_member {
        format!("post: {} is already a member of #{name}\n", outcome.room)
    } else if outcome.channel_created {
        format!("post: created #{name} and joined as {}\n", outcome.room)
    } else {
        format!("post: joined #{name} as {}\n", outcome.room)
    };
    Ok(CommandResult::committed(rendered))
}

fn send(
    context: &Context,
    mut args: ChatArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let body = super::send::read_body(args.body.take(), args.file.as_deref())?;
    let message = channel::send(context, &args.name, &args.subject, &body)?;
    let rendered = if json_output {
        output::json(&ChatSendOutput { ok: true, message }, pretty)?
    } else {
        format!(
            "post: sent #{} {} from {}\n",
            message.channel, message.id, message.from
        )
    };
    Ok(CommandResult::committed(rendered))
}
