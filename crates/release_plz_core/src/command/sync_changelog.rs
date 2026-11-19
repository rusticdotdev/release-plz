use std::collections::BTreeMap;

use anyhow::Context as _;
use cargo_metadata::{Metadata, Package, camino::Utf8Path};
use cargo_utils::workspace_members;
use git_cmd::Repo;
use tracing::{info, instrument};

use crate::{
    DEFAULT_BRANCH_PREFIX, ForgeType, GitClient, GitForge, GitPr, PackageUpdateConfig,
    UpdateConfig,
    changelog_guard::{GuardedChangelog, guarded_changelogs},
    root_repo_path_from_manifest_dir,
    update_request::PackagesConfig,
};

use super::release_pr::{add_changes_and_commit, github_commit_changes};

#[derive(Debug)]
#[must_use]
pub struct SyncChangelogRequest {
    /// Cargo metadata.
    metadata: Metadata,
    git: GitForge,
    /// Number of the release PR.
    /// If `None`, the opened release PR is used.
    pr_number: Option<u64>,
    /// Prefix of the release PR branch.
    branch_prefix: String,
    /// Package-specific configurations. Only `changelog_path` is used.
    packages_config: PackagesConfig,
}

impl SyncChangelogRequest {
    pub fn new(metadata: Metadata, git: GitForge) -> Self {
        Self {
            metadata,
            git,
            pr_number: None,
            branch_prefix: DEFAULT_BRANCH_PREFIX.to_string(),
            packages_config: PackagesConfig::default(),
        }
    }

    pub fn with_pr_number(mut self, pr_number: Option<u64>) -> Self {
        self.pr_number = pr_number;
        self
    }

    pub fn with_branch_prefix(mut self, pr_branch_prefix: Option<String>) -> Self {
        if let Some(branch_prefix) = pr_branch_prefix {
            self.branch_prefix = branch_prefix;
        }
        self
    }

    /// Set update config for all packages.
    pub fn with_default_package_config(mut self, config: UpdateConfig) -> Self {
        self.packages_config.set_default(config);
        self
    }

    /// Set update config for a specific package.
    pub fn with_package_config(
        mut self,
        package: impl Into<String>,
        config: PackageUpdateConfig,
    ) -> Self {
        self.packages_config.set(package.into(), config);
        self
    }
}

/// Copy the changelogs between the guards of the release PR body into the changelog files,
/// and commit the result to the release PR branch.
///
/// The release PR branch must be checked out.
///
/// Returns `true` if a commit was pushed, `false` if the changelog files were already up to date.
#[instrument(skip_all)]
pub async fn sync_changelog(input: &SyncChangelogRequest) -> anyhow::Result<bool> {
    let git_client = GitClient::new(input.git.clone())?;
    let pr = release_pr(&git_client, input).await?;
    let repo_root = root_repo_path_from_manifest_dir(&input.metadata.workspace_root)?;
    let repo = Repo::new(&repo_root)?;
    let local_commit = repo.current_commit_hash()?;
    anyhow::ensure!(
        local_commit == pr.head.sha,
        "the local commit {local_commit} isn't the head {} of the release PR {}. Check out the branch `{}` before running sync-changelog",
        pr.head.sha,
        pr.html_url,
        pr.branch()
    );

    let changelogs = guarded_changelogs(pr.body.as_deref().unwrap_or_default())
        .with_context(|| format!("invalid changelog guards in the body of {}", pr.html_url))?;
    if changelogs.is_empty() {
        info!(
            "the body of {} doesn't contain changelog guards",
            pr.html_url
        );
        return Ok(false);
    }

    let packages: BTreeMap<String, Package> = workspace_members(&input.metadata)?
        .map(|p| (p.name.to_string(), p))
        .collect();
    let mut is_any_changelog_updated = false;
    for changelog in &changelogs {
        let package = packages.get(&changelog.package).with_context(|| {
            format!(
                "package `{}` of the changelog guard not found in the workspace",
                changelog.package
            )
        })?;
        let config = input.packages_config.get(&package.name);
        let changelog_path = crate::changelog_path(
            package,
            &input.metadata.workspace_root,
            config.generic.changelog_path.as_deref(),
        );
        is_any_changelog_updated |= update_changelog_file(&changelog_path, changelog)
            .with_context(|| format!("failed to update changelog at {changelog_path}"))?;
    }
    if !is_any_changelog_updated {
        info!("changelogs already match the body of {}", pr.html_url);
        return Ok(false);
    }

    // Reuse the PR title as the commit message, like the other commits release-plz
    // pushes to the release PR branch.
    if git_client.forge == ForgeType::Github {
        github_commit_changes(&git_client, &repo, pr.branch(), &pr.title).await?;
    } else {
        add_changes_and_commit(&repo, &pr.title)?;
        repo.push(pr.branch())?;
    }
    info!("updated changelogs of {}", pr.html_url);
    Ok(true)
}

async fn release_pr(git_client: &GitClient, input: &SyncChangelogRequest) -> anyhow::Result<GitPr> {
    let pr = match input.pr_number {
        Some(pr_number) => git_client
            .get_pr_info(pr_number)
            .await
            .with_context(|| format!("cannot get pr {pr_number}"))?,
        None => git_client
            .opened_prs(&input.branch_prefix)
            .await
            .context("cannot get opened release-plz prs")?
            .into_iter()
            .next()
            .context("there's no opened release pr")?,
    };
    // sanity check to avoid committing to non-release-plz branches
    anyhow::ensure!(
        pr.branch().starts_with(&input.branch_prefix)
            || pr.branch().starts_with(DEFAULT_BRANCH_PREFIX),
        "{} isn't a release pr: its branch `{}` doesn't start with `{}`",
        pr.html_url,
        pr.branch(),
        input.branch_prefix
    );
    Ok(pr)
}

/// Returns `true` if the changelog file changed.
fn update_changelog_file(
    changelog_path: &Utf8Path,
    guarded_changelog: &GuardedChangelog,
) -> anyhow::Result<bool> {
    let old_changelog = fs_err::read_to_string(changelog_path)?;
    let new_changelog = replace_release_notes(&old_changelog, guarded_changelog)?;
    if new_changelog == old_changelog {
        return Ok(false);
    }
    fs_err::write(changelog_path, new_changelog)?;
    Ok(true)
}

/// Replace the notes of the release `guarded_changelog.version` in `changelog`.
/// The release title is left untouched.
fn replace_release_notes(
    changelog: &str,
    guarded_changelog: &GuardedChangelog,
) -> anyhow::Result<String> {
    let version = &guarded_changelog.version;
    let releases = parse_changelog::parse(changelog).context("can't parse changelog")?;
    let release = releases
        .values()
        .find(|release| release.version.trim_start_matches('v') == version)
        .with_context(|| format!("release {version} not found in changelog"))?;
    // `release.notes` borrows from `changelog`, so its address gives its position.
    let notes_start = (release.notes.as_ptr() as usize)
        .checked_sub(changelog.as_ptr() as usize)
        .context("release notes aren't part of the changelog")?;
    let notes_end = notes_start + release.notes.len();
    anyhow::ensure!(
        changelog.get(notes_start..notes_end) == Some(release.notes),
        "release notes aren't part of the changelog"
    );
    Ok(format!(
        "{}{}{}",
        &changelog[..notes_start],
        guarded_changelog.notes,
        &changelog[notes_end..]
    ))
}

#[cfg(test)]
mod tests {
    use base64::prelude::*;
    use secrecy::SecretString;
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, method, path},
    };

    use super::*;
    use crate::{GitHub, fs_utils::to_utf8_path};

    /// Mock the GitHub API call that returns the release PR 42.
    async fn mock_release_pr(server: &MockServer, head_sha: &str, body: &str) {
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user": {"id": 1, "login": "release-plz[bot]"},
                "number": 42,
                "html_url": "https://github.com/owner/repo/pull/42",
                "head": {"ref": "release-plz-test", "sha": head_sha},
                "title": "chore: release v0.2.0",
                "body": body,
                "labels": []
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    /// Repository of the package `my-crate`, with [`CHANGELOG`] committed.
    fn release_pr_repo(directory: &Utf8Path) -> Repo {
        let repo = Repo::init(directory);
        fs_err::write(
            directory.join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs_err::create_dir(directory.join("src")).unwrap();
        fs_err::write(directory.join("src/lib.rs"), "").unwrap();
        fs_err::write(directory.join("CHANGELOG.md"), CHANGELOG).unwrap();
        repo.add_all_and_commit("chore: release v0.2.0").unwrap();
        repo
    }

    fn sync_changelog_request(directory: &Utf8Path, server: &MockServer) -> SyncChangelogRequest {
        let metadata = cargo_utils::get_manifest_metadata(&directory.join("Cargo.toml")).unwrap();
        let github = GitHub::new("owner".into(), "repo".into(), SecretString::from("token"))
            .with_base_url(server.uri().parse().unwrap());
        SyncChangelogRequest::new(metadata, GitForge::Github(github)).with_pr_number(Some(42))
    }

    #[tokio::test]
    async fn edited_pr_body_is_committed_to_the_release_pr_branch() {
        test_logs::init();
        let temporary = tempdir().unwrap();
        let directory = to_utf8_path(temporary.path()).unwrap();
        let repo = release_pr_repo(directory);
        let head_sha = repo.current_commit_hash().unwrap();
        let server = MockServer::start().await;
        // The GitHub web editor saves the PR body with CRLF line endings.
        let pr_body = "## New release\r\n<!-- release-plz:changelog-start my-crate 0.2.0 -->\r\n### Added\r\n- support rustls\r\n<!-- release-plz:changelog-end -->\r\n";
        mock_release_pr(&server, &head_sha, pr_body).await;
        let expected_changelog = CHANGELOG.replace(
            "### Added\n- add a rustls config\n\n### Fixed\n- same line\n\n## [v0.1.0]",
            "### Added\n- support rustls\n\n## [v0.1.0]",
        );
        Mock::given(method("POST"))
            .and(path("/api/graphql"))
            .and(body_partial_json(json!({"variables": {"input": {
                "branch": {"repositoryNameWithOwner": "owner/repo", "branchName": "release-plz-test"},
                "expectedHeadOid": head_sha,
                "message": {"headline": "chore: release v0.2.0"},
                "fileChanges": {
                    "additions": [
                        {"path": "CHANGELOG.md", "contents": BASE64_STANDARD.encode(&expected_changelog)}
                    ],
                    "deletions": []
                }
            }}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"createCommitOnBranch": {"commit": {"oid": "new-sha"}}}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let is_commit_pushed = sync_changelog(&sync_changelog_request(directory, &server))
            .await
            .unwrap();

        assert!(is_commit_pushed);
        server.verify().await;
    }

    #[tokio::test]
    async fn unedited_pr_body_creates_no_commit() {
        test_logs::init();
        let temporary = tempdir().unwrap();
        let directory = to_utf8_path(temporary.path()).unwrap();
        let repo = release_pr_repo(directory);
        let server = MockServer::start().await;
        let pr_body = "<!-- release-plz:changelog-start my-crate 0.2.0 -->\n### Added\n- add a rustls config\n\n### Fixed\n- same line\n<!-- release-plz:changelog-end -->";
        mock_release_pr(&server, &repo.current_commit_hash().unwrap(), pr_body).await;

        // No GraphQL mock is mounted, so a commit attempt would fail.
        let is_commit_pushed = sync_changelog(&sync_changelog_request(directory, &server))
            .await
            .unwrap();

        assert!(!is_commit_pushed);
        assert!(repo.is_clean().is_ok());
    }

    #[tokio::test]
    async fn outdated_checkout_is_rejected() {
        test_logs::init();
        let temporary = tempdir().unwrap();
        let directory = to_utf8_path(temporary.path()).unwrap();
        release_pr_repo(directory);
        let server = MockServer::start().await;
        mock_release_pr(&server, "another-sha", "").await;

        let error = sync_changelog(&sync_changelog_request(directory, &server))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("isn't the head another-sha"));
    }

    const CHANGELOG: &str = "\
# Changelog

## [v0.2.0](https://github.com/owner/repo/compare/v0.1.0...v0.2.0) - 2026-01-02

### Added
- add a rustls config

### Fixed
- same line

## [v0.1.0](https://github.com/owner/repo/releases/tag/v0.1.0) - 2026-01-01

### Fixed
- same line
";

    fn guarded_changelog(version: &str, notes: &str) -> GuardedChangelog {
        GuardedChangelog {
            package: "my-crate".to_string(),
            version: version.to_string(),
            notes: notes.to_string(),
        }
    }

    #[test]
    fn notes_of_the_latest_release_are_replaced() {
        let new_changelog = replace_release_notes(
            CHANGELOG,
            &guarded_changelog("0.2.0", "### Added\n- support rustls"),
        )
        .unwrap();

        expect_test::expect![[r"
            # Changelog

            ## [v0.2.0](https://github.com/owner/repo/compare/v0.1.0...v0.2.0) - 2026-01-02

            ### Added
            - support rustls

            ## [v0.1.0](https://github.com/owner/repo/releases/tag/v0.1.0) - 2026-01-01

            ### Fixed
            - same line
        "]]
        .assert_eq(&new_changelog);
    }

    #[test]
    fn notes_of_an_older_release_are_replaced() {
        let new_changelog =
            replace_release_notes(CHANGELOG, &guarded_changelog("0.1.0", "- initial release"))
                .unwrap();

        expect_test::expect![[r"
            # Changelog

            ## [v0.2.0](https://github.com/owner/repo/compare/v0.1.0...v0.2.0) - 2026-01-02

            ### Added
            - add a rustls config

            ### Fixed
            - same line

            ## [v0.1.0](https://github.com/owner/repo/releases/tag/v0.1.0) - 2026-01-01

            - initial release
        "]]
        .assert_eq(&new_changelog);
    }

    #[test]
    fn unchanged_notes_leave_the_changelog_untouched() {
        let new_changelog = replace_release_notes(
            CHANGELOG,
            &guarded_changelog(
                "0.2.0",
                "### Added\n- add a rustls config\n\n### Fixed\n- same line",
            ),
        )
        .unwrap();

        assert_eq!(new_changelog, CHANGELOG);
    }

    #[test]
    fn unknown_release_is_an_error() {
        let error =
            replace_release_notes(CHANGELOG, &guarded_changelog("0.3.0", "- new")).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("release 0.3.0 not found in changelog")
        );
    }
}
