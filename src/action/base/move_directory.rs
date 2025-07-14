use std::path::{Path, PathBuf};

use tracing::{span, Span};

use crate::action::{Action, ActionDescription, ActionErrorKind, ActionState};
use crate::action::{ActionError, StatefulAction};

/** Move a directory from one location to another.

This action will move a directory from `src` to `dest`. The destination path must not exist.
On revert, it will move the directory back to its original location.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "move_directory")]
pub struct MoveDirectory {
    pub(crate) src: PathBuf,
    pub(crate) dest: PathBuf,
}

impl MoveDirectory {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        src: impl AsRef<Path>,
        dest: impl AsRef<Path>,
    ) -> Result<StatefulAction<Self>, ActionError> {
        let src = src.as_ref().to_path_buf();
        let dest = dest.as_ref().to_path_buf();

        Ok(StatefulAction {
            action: Self { src, dest },
            state: ActionState::Uncompleted,
        })
    }

    async fn check_src_and_dest(src: &Path, dest: &Path) -> Result<(), ActionError> {
        let src_metadata = tokio::fs::metadata(src)
            .await
            .map_err(|e| ActionErrorKind::GettingMetadata(src.to_path_buf(), e))
            .map_err(Self::error)?;
        let dest_exists = tokio::fs::try_exists(dest)
            .await
            .map_err(|e| ActionErrorKind::GettingMetadata(dest.to_path_buf(), e))
            .map_err(Self::error)?;
        if dest_exists {
            return Err(Self::error(ActionErrorKind::DirExists(dest.to_path_buf())));
        }
        if !src_metadata.is_dir() {
            return Err(Self::error(ActionErrorKind::PathWasNotDirectory(
                src.to_path_buf(),
            )));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "move_directory")]
impl Action for MoveDirectory {
    fn action_tag() -> crate::action::ActionTag {
        crate::action::ActionTag("move_directory")
    }

    fn tracing_synopsis(&self) -> String {
        format!(
            "Move directory `{}` to `{}`",
            self.src.display(),
            self.dest.display()
        )
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "move_directory",
            src = tracing::field::display(self.src.display()),
            dest = tracing::field::display(self.dest.display()),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(self.tracing_synopsis(), vec![])]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> Result<(), ActionError> {
        let Self { src, dest } = self;
        Self::check_src_and_dest(&src, &dest).await?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = dest.parent() {
            let parent_exists = tokio::fs::try_exists(parent)
                .await
                .map_err(|e| ActionErrorKind::GettingMetadata(parent.to_path_buf(), e))
                .map_err(Self::error)?;
            if !parent_exists {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| ActionErrorKind::CreateDirectory(parent.to_path_buf(), e))
                    .map_err(Self::error)?;
            }
        }

        // Move the directory
        tokio::fs::rename(&src, &dest)
            .await
            .map_err(|e| ActionErrorKind::Rename(src.clone(), dest.clone(), e))
            .map_err(Self::error)?;

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!(
                "Move directory `{}` back to `{}`",
                self.dest.display(),
                self.src.display()
            ),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> Result<(), ActionError> {
        let Self { src, dest } = self;
        Self::check_src_and_dest(&dest, &src).await?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = src.parent() {
            let parent_exists = tokio::fs::try_exists(parent)
                .await
                .map_err(|e| ActionErrorKind::GettingMetadata(parent.to_path_buf(), e))
                .map_err(Self::error)?;
            if !parent_exists {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| ActionErrorKind::CreateDirectory(parent.to_path_buf(), e))
                    .map_err(Self::error)?;
            }
        }

        // Move the directory back
        tokio::fs::rename(&dest, &src)
            .await
            .map_err(|e| ActionErrorKind::Rename(dest.clone(), src.clone(), e))
            .map_err(Self::error)?;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn moves_and_reverts_directory() -> eyre::Result<()> {
        let temp_dir = tempdir()?;
        let src_dir = temp_dir.path().join("source");
        let dest_dir = temp_dir.path().join("destination");

        // Create source directory with some content
        tokio::fs::create_dir_all(&src_dir).await?;
        tokio::fs::write(src_dir.join("test.txt"), "test content").await?;

        let mut action = MoveDirectory::plan(&src_dir, &dest_dir).await?;

        // Execute should move the directory
        action.try_execute().await?;

        assert!(
            !src_dir.exists(),
            "Source directory should not exist after move"
        );
        assert!(
            dest_dir.exists(),
            "Destination directory should exist after move"
        );
        assert!(
            dest_dir.join("test.txt").exists(),
            "Content should be moved"
        );

        // Revert should move it back
        action.try_revert().await?;

        assert!(
            src_dir.exists(),
            "Source directory should exist after revert"
        );
        assert!(
            !dest_dir.exists(),
            "Destination directory should not exist after revert"
        );
        assert!(
            src_dir.join("test.txt").exists(),
            "Content should be moved back"
        );

        Ok(())
    }

    #[tokio::test]
    async fn handles_already_moved_directory() -> eyre::Result<()> {
        let temp_dir = tempdir()?;
        let src_dir = temp_dir.path().join("source");
        let dest_dir = temp_dir.path().join("destination");

        // Create destination directory (simulating already moved)
        tokio::fs::create_dir_all(&dest_dir).await?;

        let action = MoveDirectory::plan(&src_dir, &dest_dir).await?;

        // Should be marked as completed
        assert!(matches!(action.state, ActionState::Completed));

        Ok(())
    }

    #[tokio::test]
    async fn fails_when_source_missing() -> eyre::Result<()> {
        let temp_dir = tempdir()?;
        let src_dir = temp_dir.path().join("nonexistent");
        let dest_dir = temp_dir.path().join("destination");

        let result = MoveDirectory::plan(&src_dir, &dest_dir).await;

        assert!(result.is_err(), "Should fail when source doesn't exist");

        Ok(())
    }
}
