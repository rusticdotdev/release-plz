//! HTML comments that delimit the changelog of each package in the release PR body.
//!
//! The guards are invisible in the rendered PR, and they let `sync-changelog`
//! find the text that the user edited:
//!
//! ```text
//! <!-- release-plz:changelog-start my-crate 0.10.0 -->
//! ### Added
//!
//! - add a rustls config
//! <!-- release-plz:changelog-end -->
//! ```

use anyhow::Context as _;

const START_GUARD_PREFIX: &str = "<!-- release-plz:changelog-start ";
const GUARD_SUFFIX: &str = "-->";
const END_GUARD: &str = "<!-- release-plz:changelog-end -->";

/// Changelog of one package, read from between the guards of a release PR body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedChangelog {
    pub package: String,
    pub version: String,
    /// Release notes, without the release title.
    pub notes: String,
}

/// Wrap the changelog of a package in the guards.
pub(crate) fn guard_changelog(package: &str, version: &str, changelog: &str) -> String {
    format!("{START_GUARD_PREFIX}{package} {version} {GUARD_SUFFIX}\n{changelog}\n{END_GUARD}")
}

/// Read the changelogs between the guards of a release PR body.
///
/// Returns an empty list if the body has no guards,
/// e.g. because the changelog was too long to fit in the PR body.
pub fn guarded_changelogs(pr_body: &str) -> anyhow::Result<Vec<GuardedChangelog>> {
    // The GitHub web editor saves the PR body with CRLF line endings.
    let pr_body = pr_body.replace("\r\n", "\n");
    let mut changelogs = vec![];
    let mut rest = pr_body.as_str();
    while let Some(start) = rest.find(START_GUARD_PREFIX) {
        let after_prefix = &rest[start + START_GUARD_PREFIX.len()..];
        let (guard_args, after_start_guard) = after_prefix
            .split_once(GUARD_SUFFIX)
            .context("changelog start guard isn't closed by `-->`")?;
        let [package, version] = guard_args.split_whitespace().collect::<Vec<_>>()[..] else {
            anyhow::bail!(
                "changelog start guard must contain a package name and a version, found `{}`",
                guard_args.trim()
            );
        };
        let (notes, after_end_guard) = after_start_guard
            .split_once(END_GUARD)
            .with_context(|| format!("changelog guard of package `{package}` has no end guard"))?;
        anyhow::ensure!(
            !notes.contains(START_GUARD_PREFIX),
            "changelog guard of package `{package}` has no end guard"
        );
        changelogs.push(GuardedChangelog {
            package: package.to_string(),
            version: version.to_string(),
            notes: notes.trim().to_string(),
        });
        rest = after_end_guard;
    }
    Ok(changelogs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_changelog_is_read_back() {
        let body = format!(
            "## New release\n\n{}\n\nfooter",
            guard_changelog("my-crate", "0.10.0", "### Added\n\n- add a rustls config")
        );

        assert_eq!(
            guarded_changelogs(&body).unwrap(),
            vec![GuardedChangelog {
                package: "my-crate".to_string(),
                version: "0.10.0".to_string(),
                notes: "### Added\n\n- add a rustls config".to_string(),
            }]
        );
    }

    #[test]
    fn changelogs_of_multiple_packages_are_read() {
        let body = format!(
            "{}\n## `b`\n{}",
            guard_changelog("a", "0.1.1", "- fix a"),
            guard_changelog("b", "2.0.0", "- break b")
        );

        let changelogs = guarded_changelogs(&body).unwrap();

        let packages_and_notes: Vec<_> = changelogs
            .iter()
            .map(|c| (c.package.as_str(), c.version.as_str(), c.notes.as_str()))
            .collect();
        assert_eq!(
            packages_and_notes,
            vec![("a", "0.1.1", "- fix a"), ("b", "2.0.0", "- break b")]
        );
    }

    #[test]
    fn crlf_line_endings_are_normalized() {
        let body = "<!-- release-plz:changelog-start a 0.1.1 -->\r\n- one\r\n- two\r\n<!-- release-plz:changelog-end -->\r\n";

        let changelogs = guarded_changelogs(body).unwrap();

        assert_eq!(changelogs[0].notes, "- one\n- two");
    }

    #[test]
    fn body_without_guards_has_no_changelogs() {
        assert_eq!(guarded_changelogs("## New release").unwrap(), vec![]);
    }

    #[test]
    fn missing_end_guard_is_an_error() {
        let body = "<!-- release-plz:changelog-start a 0.1.1 -->\n- one";

        let error = guarded_changelogs(body).unwrap_err();

        assert!(error.to_string().contains("has no end guard"));
    }

    #[test]
    fn start_guard_before_the_end_guard_is_an_error() {
        let body = "<!-- release-plz:changelog-start a 0.1.1 -->\n- one\n<!-- release-plz:changelog-start b 0.1.1 -->\n- two\n<!-- release-plz:changelog-end -->";

        let error = guarded_changelogs(body).unwrap_err();

        assert!(error.to_string().contains("package `a` has no end guard"));
    }

    #[test]
    fn start_guard_without_version_is_an_error() {
        let body =
            "<!-- release-plz:changelog-start a -->\n- one\n<!-- release-plz:changelog-end -->";

        let error = guarded_changelogs(body).unwrap_err();

        assert!(error.to_string().contains("package name and a version"));
    }
}
