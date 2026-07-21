use crate::error::AppResult;

pub(crate) struct CommandResult {
    pub stdout: String,
    pub exit_code: i32,
    pub delivery_committed: bool,
    pub registration_committed: bool,
    pub after_stdout: Option<Box<dyn FnOnce() -> AppResult<()>>>,
}

impl CommandResult {
    pub(crate) fn success(stdout: String) -> Self {
        Self {
            stdout,
            exit_code: 0,
            delivery_committed: false,
            registration_committed: false,
            after_stdout: None,
        }
    }

    pub(crate) fn committed(stdout: String) -> Self {
        Self {
            delivery_committed: true,
            ..Self::success(stdout)
        }
    }

    pub(crate) fn json(value: &impl serde::Serialize, pretty: bool) -> AppResult<Self> {
        Ok(Self::success(crate::output::json(value, pretty)?))
    }

    pub(crate) fn registration_committed(mut self) -> Self {
        self.registration_committed = true;
        self
    }

    pub(crate) fn after_stdout(
        stdout: String,
        action: impl FnOnce() -> AppResult<()> + 'static,
    ) -> Self {
        Self {
            after_stdout: Some(Box::new(action)),
            ..Self::success(stdout)
        }
    }
}
