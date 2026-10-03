use anyhow::Result;
use serde::Serialize;

use crate::contracts::worker::WorkerConsoleCommand;

use super::super::{EmptyRequest, WorkerControl};

impl WorkerControl {
    pub async fn claim_console_command(&self) -> Result<Option<WorkerConsoleCommand>> {
        self.post_json(
            "/internal/worker/control/console/commands/claim",
            &EmptyRequest {},
        )
        .await
    }

    pub async fn complete_console_command(
        &self,
        command: &WorkerConsoleCommand,
        succeeded: bool,
        output: Vec<String>,
        error: Option<&str>,
    ) -> Result<()> {
        let error = error.map(super::super::bounded_worker_error);
        self.post_no_content(
            &format!(
                "/internal/worker/control/console/commands/{}/complete",
                command.id
            ),
            &ConsoleCommandCompletionRequest {
                generation: command.generation,
                attempt: command.attempt,
                succeeded,
                output,
                error: error.map(|error| error.into_owned()),
            },
        )
        .await
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ConsoleCommandCompletionRequest {
    generation: i64,
    attempt: i32,
    succeeded: bool,
    output: Vec<String>,
    error: Option<String>,
}
