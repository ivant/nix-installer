use crate::{
    action::{Action, ActionDescription, ActionError, ActionErrorKind, ActionTag, StatefulAction},
    execute_command,
    settings::CommonSettings,
};
use indoc::formatdoc;
use tokio::process::Command;
use tracing::{span, Span};

const SYSUSERS_PATH: &str = "/usr/lib/sysusers.d/nix.conf";

#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "create_users_and_group_sysusers")]
pub struct CreateUsersAndGroupsSysUsers {
    pub(crate) nix_build_group_name: String,
    pub(crate) nix_build_group_id: u32,
    pub(crate) nix_build_user_count: u32,
    pub(crate) nix_build_user_prefix: String,
    pub(crate) nix_build_user_id_base: u32,
}

impl CreateUsersAndGroupsSysUsers {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(settings: &CommonSettings) -> Result<StatefulAction<Self>, ActionError> {
        Ok(Self {
            nix_build_group_name: settings.nix_build_group_name.clone(),
            nix_build_group_id: settings.nix_build_group_id,
            nix_build_user_count: settings.nix_build_user_count,
            nix_build_user_prefix: settings.nix_build_user_prefix.clone(),
            nix_build_user_id_base: settings.nix_build_user_id_base,
        }
        .into())
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "create_users_and_group_sysusers")]
impl Action for CreateUsersAndGroupsSysUsers {
    fn action_tag() -> ActionTag {
        ActionTag("create_users_and_group_sysusers")
    }
    fn tracing_synopsis(&self) -> String {
        if self.nix_build_user_count == 0 {
            format!(
                "Create {} with build group {} (GID {})",
                SYSUSERS_PATH, self.nix_build_group_name, self.nix_build_group_id
            )
        } else {
            format!(
                "Create {} with build users {}* (UID {}-{}) and group {} (GID {})",
                SYSUSERS_PATH,
                self.nix_build_user_prefix,
                self.nix_build_user_id_base + 1,
                self.nix_build_user_id_base + self.nix_build_user_count,
                self.nix_build_group_name,
                self.nix_build_group_id,
            )
        }
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "create_users_and_group_sysusers",
            nix_build_user_count = self.nix_build_user_count,
            nix_build_group_name = self.nix_build_group_name,
            nix_build_group_id = self.nix_build_group_id,
            nix_build_user_prefix = self.nix_build_user_prefix,
            nix_build_user_id_base = self.nix_build_user_id_base,
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![
            ActionDescription::new(
                self.tracing_synopsis(),
                vec!["The Nix daemon requires system users (and a group they share) which it can act as in order to build".into()],
            ),
            ActionDescription::new(
                format!("Run `systemd-sysusers {SYSUSERS_PATH}` to create the users and group"),
                vec!["Build users and group are required for the rest of the installation to succeed".into()],
            ),
        ]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> Result<(), ActionError> {
        let Self {
            nix_build_user_count,
            nix_build_group_name,
            nix_build_group_id,
            nix_build_user_prefix,
            nix_build_user_id_base,
        } = self;

        let mut nix_sysusers_content = formatdoc! {
            r#"
            # Nix build group and users.
            g {nix_build_group_name} {nix_build_group_id}
            "#
        };
        for i in 1..=*nix_build_user_count {
            let uid = *nix_build_user_id_base + i - 1;
            let user_name = format!("{nix_build_user_prefix}{i}");
            // Starting Systemd 257 it is recommended to use "u!" instead of "u", which creates locked
            // user accounts. Unfortunately, this is version dependent and version 257 is relatively
            // recent (Dec 2024), so we'll use "u" for now. Eventually we can have "u!" as a default
            // with a flag to switch back to "u" if needed for older systems.
            //
            // Unfortunately, we must explicitly add the user to the group, otherwise we'll get
            // the following error:
            //   the build users group 'nixbld' has no members
            nix_sysusers_content += &formatdoc! {
                r#"
                u {user_name} {uid}:{nix_build_group_id} "Nix build user {i}"
                m {user_name} {nix_build_group_name}
                "#
            };
        }
        tokio::fs::write(SYSUSERS_PATH, nix_sysusers_content)
            .await
            .map_err(|e| ActionErrorKind::Write(SYSUSERS_PATH.into(), e))
            .map_err(Self::error)?;

        execute_command(Command::new("systemd-sysusers").arg(SYSUSERS_PATH))
            .await
            .map_err(Self::error)?;
        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![
            ActionDescription::new(
                format!("Remove {} containing the build users and group", SYSUSERS_PATH),
                vec!["The Nix daemon requires system users (and a group they share) which it can act as in order to build".into()],
            ),
            ActionDescription::new(
                format!("Run `systemd-sysusers {SYSUSERS_PATH}` to remove the users and group"),
                vec![],
            ),
        ]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> Result<(), ActionError> {
        tokio::fs::remove_file(SYSUSERS_PATH)
            .await
            .map_err(|e| ActionErrorKind::Remove(SYSUSERS_PATH.into(), e))
            .map_err(Self::error)?;
        execute_command(&mut Command::new("systemd-sysusers"))
            .await
            .map_err(Self::error)?;
        Ok(())
    }
}
