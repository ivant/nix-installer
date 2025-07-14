use tokio::process::Command;
use tracing::{span, Span};

use crate::action::{ActionError, ActionState, ActionTag, StatefulAction};
use crate::execute_command;

use crate::action::{Action, ActionDescription};

/// Enable a given systemd unit.
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "enable_systemd_unit")]
pub struct EnableSystemdUnit {
    unit: String,
}

impl EnableSystemdUnit {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(unit: impl AsRef<str>) -> Result<StatefulAction<Self>, ActionError> {
        Ok(StatefulAction {
            action: Self {
                unit: unit.as_ref().to_string(),
            },
            state: ActionState::Uncompleted,
        })
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "enable_systemd_unit")]
impl Action for EnableSystemdUnit {
    fn action_tag() -> ActionTag {
        ActionTag("enable_systemd_unit")
    }

    fn tracing_synopsis(&self) -> String {
        format!("Enable the systemd unit `{}`", self.unit)
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "enable_systemd_unit",
            unit = %self.unit,
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(self.tracing_synopsis(), vec![])]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> Result<(), ActionError> {
        let Self { unit } = self;

        execute_command(
            Command::new("systemctl")
                .process_group(0)
                .arg("enable")
                .arg(unit)
                .stdin(std::process::Stdio::null()),
        )
        .await
        .map_err(Self::error)?;

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Disable the systemd unit `{}`", self.unit),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> Result<(), ActionError> {
        execute_command(
            Command::new("systemctl")
                .process_group(0)
                .arg("disable")
                .arg(&self.unit)
                .stdin(std::process::Stdio::null()),
        )
        .await
        .map_err(Self::error)?;

        Ok(())
    }
}

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum EnableSystemdUnitError {
    #[error("Failed to execute command")]
    Command(#[source] std::io::Error),
}
