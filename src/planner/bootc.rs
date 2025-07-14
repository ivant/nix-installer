use indoc::{formatdoc, indoc};

use crate::{
    action::{
        base::{CreateDirectory, CreateFile, MoveDirectory, RemoveDirectory},
        common::{
            ConfigureNix, ConfigureUpstreamInitService, CreateUsersAndGroups,
            ProvisionDeterminateNixd, ProvisionNix,
        },
        linux::{
            provision_selinux::{DETERMINATE_SELINUX_POLICY_PP_CONTENT, SELINUX_POLICY_PP_CONTENT},
            ProvisionSelinux,
        },
        StatefulAction,
    },
    distribution::Distribution,
    error::HasExpectedErrors,
    planner::{Planner, PlannerError},
    settings::{CommonSettings, InitSystem, InstallSettingsError},
    Action, BuiltinPlanner,
};
use std::{collections::HashMap, path::PathBuf};

use super::{
    linux::{check_nix_not_already_installed, check_not_nixos, check_not_wsl1},
    ShellProfileLocations,
};

/// A planner for bootable container systems using bootc
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "cli", derive(clap::Parser))]
pub struct Bootc {
    /// Where `/nix` read-only image will be installed in the container.
    #[cfg_attr(feature = "cli", clap(long, default_value = "/usr/lib/nix-install"))]
    readonly_image: PathBuf,
    /// Where `/nix` changes will be persisted in the container.
    #[cfg_attr(feature = "cli", clap(long, default_value = "/var/lib/nix-overlay"))]
    overlay: PathBuf,
    #[cfg_attr(feature = "cli", clap(long, default_value = "/etc/systemd/system"))]
    systemd_unit_dir: PathBuf,
    #[cfg_attr(feature = "cli", clap(flatten))]
    pub settings: CommonSettings,
}

impl Bootc {
    fn work_dir(&self) -> PathBuf {
        let mut work_dir = self.overlay.clone();
        work_dir.push("work");
        work_dir
    }

    fn upper_dir(&self) -> PathBuf {
        let mut upper_dir = self.overlay.clone();
        upper_dir.push("upper");
        upper_dir
    }

    fn systemd_unit_path(&self, unit_name: &str) -> PathBuf {
        let mut path = self.systemd_unit_dir.clone();
        path.push(unit_name);
        path
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "bootc")]
impl Planner for Bootc {
    async fn default() -> Result<Self, PlannerError> {
        Ok(Self {
            readonly_image: PathBuf::from("/usr/lib/nix-install"),
            overlay: PathBuf::from("/var/lib/nix-overlay"),
            systemd_unit_dir: PathBuf::from("/etc/systemd/system"),
            settings: CommonSettings::default().await?,
        })
    }

    async fn plan(&self) -> Result<Vec<StatefulAction<Box<dyn Action>>>, PlannerError> {
        let mut plan = vec![];

        // Create /usr/lib/tmpfiles.d/nix.conf file that creates the overlay directory if it is missing.
        //
        // We assume that /usr/lib/tmpfiles.d already exists. This is a reasonable assumption for Bootc,
        // which has various other files in it.
        let tmpfiles_content = formatdoc! {
            r#"
            # Create overlay directories for Nix.
            d {upper_dir} 0755 root root -
            # Work dir must be empty before overlayfs is mounted.
            R {work_dir} - - - - -
            d {work_dir} 0755 root root -
            "#,
            upper_dir = self.upper_dir().display(),
            work_dir = self.work_dir().display(),
        };

        plan.push(
            CreateFile::plan(
                "/usr/lib/tmpfiles.d/nix.conf",
                None,
                None,
                0o0644,
                tmpfiles_content,
                false,
            )
            .await
            .map_err(PlannerError::Action)?
            .boxed(),
        );

        // Create /nix directory.
        plan.push(
            CreateDirectory::plan("/nix", None, None, 0o0755, false)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // Create systemd mount unit that uses overlayfs to combine readonly_image and overlay and mount it to /nix.
        let nix_mount_unit_content = formatdoc! {
            r#"
            [Unit]
            Description=Overlay mount for Nix in bootc container
            DefaultDependencies=no
            Requires=local-fs.target systemd-tmpfiles-setup.service
            After=local-fs.target systemd-tmpfiles-setup.service
            Before=nix-daemon.service
            Before=nix-daemon.socket
            PropagatesStopTo=nix-daemon.service
            ConditionPathIsDirectory=/nix

            [Mount]
            What=overlay
            Where=/nix
            Type=overlay
            Options=lowerdir={readonly_image},upperdir={upper_dir},workdir={work_dir}
            DirectoryMode=0755

            [Install]
            WantedBy=sysinit.target
            RequiredBy=nix-daemon.service
            RequiredBy=nix-daemon.socket
            "#,
            readonly_image = self.readonly_image.display(),
            upper_dir = self.upper_dir().display(),
            work_dir = self.work_dir().display(),
        };

        plan.push(
            CreateFile::plan(
                self.systemd_unit_path("nix.mount"),
                None,
                None,
                0o0644,
                nix_mount_unit_content,
                false,
            )
            .await
            .map_err(PlannerError::Action)?
            .boxed(),
        );

        // Create "Ensure symlinked units resolve" unit that runs after the mount unit
        let ensure_symlinked_units_resolve_content = indoc! {
            r#"
            [Unit]
            Description=Ensure Nix related units which are symlinked resolve
            After=nix.mount
            Requires=nix.mount
            DefaultDependencies=no

            [Service]
            Type=oneshot
            RemainAfterExit=yes
            ExecStart=/usr/bin/systemctl daemon-reload
            ExecStart=/usr/bin/systemctl restart --no-block nix-daemon.socket

            [Install]
            WantedBy=sysinit.target
            "#
        };

        plan.push(
            CreateFile::plan(
                self.systemd_unit_path("ensure-symlinked-units-resolve.service"),
                None,
                None,
                0o0644,
                ensure_symlinked_units_resolve_content.to_string(),
                false,
            )
            .await
            .map_err(PlannerError::Action)?
            .boxed(),
        );

        // Create /nix directory. We'll install Nix there, then move it to the readonly image directory.
        plan.push(
            CreateDirectory::plan("/nix", None, None, 0o0755, true)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // Provision Determinate Nix if needed.
        if self.settings.distribution() == Distribution::DeterminateNix {
            plan.push(
                ProvisionDeterminateNixd::plan()
                    .await
                    .map_err(PlannerError::Action)?
                    .boxed(),
            );
        }

        // Provision Nix to the /nix directory. We'll move it to the readonly image directory later.
        let nix_settings = self.settings.clone();
        plan.push(
            ProvisionNix::plan(&nix_settings)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // Create users and groups.
        plan.push(
            CreateUsersAndGroups::plan(self.settings.clone())
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // Configure Nix.
        plan.push(
            ConfigureNix::plan(ShellProfileLocations::default(), &self.settings)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // Provision SELinux.
        //
        // Bootc containers always ship with SELinux.
        // Even if /etc/selinux/config has SELINUX=disabled, it makes sense to install
        // Nix SELinux policy to avoid problems if SELinux gets enabled in a later layer.
        plan.push(
            ProvisionSelinux::plan(
                "/etc/nix-installer/selinux/packages/nix.pp".into(),
                if self.settings.distribution() == Distribution::DeterminateNix {
                    DETERMINATE_SELINUX_POLICY_PP_CONTENT
                } else {
                    SELINUX_POLICY_PP_CONTENT
                },
            )
            .await
            .map_err(PlannerError::Action)?
            .boxed(),
        );

        // Configure upstream init service, but don't start daemon.
        plan.push(
            ConfigureUpstreamInitService::plan(InitSystem::Systemd, false)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // Move /nix directory to readonly_image directory.
        // This is the final step to prepare the Nix installation for the bootc container.
        plan.push(
            MoveDirectory::plan("/nix", &self.readonly_image)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // Re-create an empty /nix directory. This must be created within the
        // container, because root is read-only and this is our mountpoint.
        plan.push(
            CreateDirectory::plan("/nix", None, None, 0o0755, true)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        // // Enable the nix.mount unit.
        // plan.push(
        //     StartSystemdUnit::plan("nix.mount".to_string(), false)
        //         .await
        //         .map_err(PlannerError::Action)?
        //         .boxed(),
        // );

        // Enable and start the ensure-symlinked-units-resolve service
        // plan.push(
        //    StartSystemdUnit::plan("ensure-symlinked-units-resolve.service".to_string(), true)
        //        .await
        //        .map_err(PlannerError::Action)?
        //        .boxed(),
        // );

        // Remove scratch directory
        plan.push(
            RemoveDirectory::plan(crate::settings::SCRATCH_DIR)
                .await
                .map_err(PlannerError::Action)?
                .boxed(),
        );

        Ok(plan)
    }

    fn settings(&self) -> Result<HashMap<String, serde_json::Value>, InstallSettingsError> {
        let Self {
            readonly_image,
            overlay,
            systemd_unit_dir,
            settings,
        } = self;
        let mut map = HashMap::default();

        map.extend(settings.settings()?);
        map.insert(
            "readonly_image".to_string(),
            serde_json::to_value(readonly_image)?,
        );
        map.insert("overlay".to_string(), serde_json::to_value(overlay)?);
        map.insert(
            "systemd_unit_dir".to_string(),
            serde_json::to_value(systemd_unit_dir)?,
        );

        Ok(map)
    }

    async fn configured_settings(
        &self,
    ) -> Result<HashMap<String, serde_json::Value>, PlannerError> {
        let default = Self::default().await?.settings()?;
        let configured = self.settings()?;

        let mut settings: HashMap<String, serde_json::Value> = HashMap::new();
        for (key, value) in configured.iter() {
            if default.get(key) != Some(value) {
                settings.insert(key.clone(), value.clone());
            }
        }

        Ok(settings)
    }

    async fn platform_check(&self) -> Result<(), PlannerError> {
        use target_lexicon::OperatingSystem;
        match target_lexicon::OperatingSystem::host() {
            OperatingSystem::Linux => {
                // TODO: Add bootc-specific checks here
                // For now, just check if we're on Linux
                Ok(())
            },
            host_os => Err(PlannerError::IncompatibleOperatingSystem {
                planner: self.typetag_name(),
                host_os,
            }),
        }
    }

    async fn pre_uninstall_check(&self) -> Result<(), PlannerError> {
        check_not_wsl1()?;

        // TODO: Add bootc-specific pre-uninstall checks

        Ok(())
    }

    async fn pre_install_check(&self) -> Result<(), PlannerError> {
        check_not_nixos()?;
        check_nix_not_already_installed().await?;
        check_not_wsl1()?;

        // TODO: Add bootc-specific pre-install checks
        // - Check if we're actually running in a bootc container
        // - Check container runtime environment
        // - Verify persistence capabilities

        Ok(())
    }
}

impl From<Bootc> for BuiltinPlanner {
    fn from(val: Bootc) -> Self {
        BuiltinPlanner::Bootc(val)
    }
}

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum BootcError {
    #[error("Not running in a bootc container environment")]
    NotBootcContainer,
    #[error("Persistence directory is not available or writable")]
    PersistenceNotAvailable,
    #[error("Required bootc tools are not available")]
    BootcToolsNotAvailable,
}

impl HasExpectedErrors for BootcError {
    fn expected<'a>(&'a self) -> Option<Box<dyn std::error::Error + 'a>> {
        match self {
            BootcError::NotBootcContainer => Some(Box::new(self)),
            BootcError::PersistenceNotAvailable => Some(Box::new(self)),
            BootcError::BootcToolsNotAvailable => Some(Box::new(self)),
        }
    }
}

impl From<BootcError> for PlannerError {
    fn from(v: BootcError) -> PlannerError {
        PlannerError::Custom(Box::new(v))
    }
}
