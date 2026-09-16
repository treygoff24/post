mod common;

use common::{assert_success, from_stdout, register_alpha_beta, Sandbox};
use serde_json::Value;

#[test]
fn basic_inbox_unread_count_uses_participant_eligibility_not_file_subtraction() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let beta_participant = sandbox.test_participant("beta");
    let mut ids = Vec::new();
    for body in ["one", "two", "three"] {
        let sent = sandbox.run_in(
            &["send", "--to", "workspace:beta", "--body", body, "--json"],
            None,
            &alpha,
        );
        assert_success(&sent);
        let sent: Value = from_stdout(&sent);
        ids.push(sent["envelope"]["id"].as_str().expect("mail id").to_owned());
    }

    let first = sandbox.run_as_participant(&["inbox", "--json"], &beta_participant, &beta);
    assert_success(&first);
    let first: Value = from_stdout(&first);
    assert_eq!(first["count"], 3);
    assert_eq!(first["unread_count"], 3);

    let read = sandbox.run_as_participant(&["read", &ids[0], "--json"], &beta_participant, &beta);
    assert_success(&read);
    let second = sandbox.run_as_participant(&["inbox", "--json"], &beta_participant, &beta);
    assert_success(&second);
    let second: Value = from_stdout(&second);
    assert_eq!(second["count"], 2);
    assert_eq!(second["unread_count"], 2);
    assert!(ids.iter().all(|id| sandbox
        .mail_root
        .join("beta/inbox")
        .join(format!("{id}.mail"))
        .is_file()));
}
