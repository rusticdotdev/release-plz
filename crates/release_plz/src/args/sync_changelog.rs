use std::path::{Path, PathBuf};

use clap::builder::NonEmptyStringValueParser;
use release_plz_core::sync_changelog::SyncChangelogRequest;
use secrecy::SecretString;

use crate::config::Config;

use super::{
    config_path::ConfigPath,
    git_forge::{GitForgeKind, git_forge},
    manifest_command::ManifestCommand,
    repo_command::RepoCommand,
};

#[derive(clap::Parser, Debug)]
pub struct SyncChangelog {
    /// Number of the release PR.
    /// If unspecified, release-plz uses the opened release PR.
    #[arg(long)]
    pr: Option<u64>,

    /// Path to the Cargo.toml of the project you want to update.
    /// If not provided, release-plz will use the Cargo.toml of the current directory.
    /// Both Cargo workspaces and single packages are supported.
    #[arg(long, value_parser = clap::value_parser!(PathBuf), alias = "project-manifest")]
    manifest_path: Option<PathBuf>,

    /// GitHub/Gitea/GitLab repository url where your project is hosted.
    /// It defaults to the url of the default remote.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    repo_url: Option<String>,

    /// Git token used to read the release PR and to commit to its branch.
    #[arg(long, value_parser = NonEmptyStringValueParser::new(), env, hide_env_values = true)]
    git_token: String,

    /// Kind of git forge
    #[arg(long, visible_alias = "backend", value_enum)]
    forge: Option<GitForgeKind>,

    #[command(flatten)]
    pub config: ConfigPath,
}

impl SyncChangelog {
    pub fn sync_changelog_request(self, config: &Config) -> anyhow::Result<SyncChangelogRequest> {
        let cargo_metadata = self.cargo_metadata()?;
        let repo_url = self.get_repo_url(config)?;
        let git_token = SecretString::from(self.git_token.clone());
        let forge = git_forge(repo_url, git_token, self.forge, "sync changelog")?;
        let request = SyncChangelogRequest::new(cargo_metadata, forge)
            .with_pr_number(self.pr)
            .with_branch_prefix(config.workspace.pr_branch_prefix.clone());
        config.fill_sync_changelog_config(request)
    }
}

impl RepoCommand for SyncChangelog {
    fn repo_url(&self) -> Option<&str> {
        self.repo_url.as_deref()
    }
}

impl ManifestCommand for SyncChangelog {
    fn optional_manifest(&self) -> Option<&Path> {
        self.manifest_path.as_deref()
    }
}
