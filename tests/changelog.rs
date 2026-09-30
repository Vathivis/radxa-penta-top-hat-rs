use std::path::PathBuf;
use std::process::{Command, Output};

struct Repository(PathBuf);

impl Repository {
    fn new() -> Self {
        let path = checked(Command::new("mktemp").args(["-d", "/tmp/radxa-changelog-test.XXXXXX"]));
        let repo = Self(PathBuf::from(path.trim()));
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::fs::create_dir_all(repo.0.join("packaging/debian")).unwrap();
        for file in [
            "packaging/read-version.sh",
            "packaging/detect-release.sh",
            "packaging/prepare-release-source.sh",
            "packaging/release-notes.sh",
            "packaging/debian/generate-changelog.sh",
        ] {
            std::fs::copy(source.join(file), repo.0.join(file)).unwrap();
        }
        std::fs::write(
            repo.0.join("packaging/debian/changelog"),
            "radxa-penta-top-hat-rs (1.0.3) stable; urgency=medium\n\n  * Archived release\n\n -- Test <test@example.invalid>  Tue, 01 Sep 2026 00:00:00 +0000\n",
        )
        .unwrap();
        repo.git(&["init", "--quiet"]);
        repo.git(&["config", "user.name", "Changelog test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "core.hooksPath", "/dev/null"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        checked(Command::new("git").current_dir(&self.0).args(args));
    }

    fn commit_version(&self, version: &str, subject: &str) {
        std::fs::write(
            self.0.join("Cargo.toml"),
            format!("[package]\nname = \"radxa-penta-top-hat-rs\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
        self.git(&["add", "."]);
        self.git(&[
            "commit",
            "--quiet",
            "-m",
            &format!("{subject}\n\n- Record a fixture change for version {version}"),
        ]);
    }

    fn changelog(&self, version: &str) -> Output {
        Command::new("sh")
            .current_dir(&self.0)
            .args([
                "packaging/debian/generate-changelog.sh",
                version,
                "1790035200",
            ])
            .output()
            .unwrap()
    }

    fn changelog_from(&self, version: &str, base: &str) -> Output {
        Command::new("sh")
            .current_dir(&self.0)
            .args([
                "packaging/debian/generate-changelog.sh",
                version,
                "1790035200",
                base,
            ])
            .output()
            .unwrap()
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn checked(command: &mut Command) -> String {
    successful_output(command.output().unwrap())
}

fn successful_output(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn unpublished_version_is_retried_after_a_same_version_merge() {
    let repo = Repository::new();
    repo.commit_version("1.0.3", "feat: establish archived release");
    repo.git(&["tag", "v1.0.3"]);
    repo.commit_version("1.0.4", "chore: bump version to 1.0.4");
    let before = checked(
        Command::new("git")
            .current_dir(&repo.0)
            .args(["rev-parse", "HEAD"]),
    );
    let detect = |before: &str| {
        checked(
            Command::new("sh")
                .current_dir(&repo.0)
                .args(["packaging/detect-release.sh", before]),
        )
    };
    assert!(detect("HEAD^").contains("should_release=true\n"));

    // An intervening merge rejects the earlier workflow's lockfile push.
    // Its own workflow must retry publication despite an unchanged version.
    std::fs::write(
        repo.0.join("README.md"),
        "Intervening documentation change\n",
    )
    .unwrap();
    repo.commit_version("1.0.4", "docs: update the documentation");
    let pending = detect(before.trim());
    assert!(pending.contains("version=1.0.4\n"));
    assert!(pending.contains("should_release=true\n"));
    assert!(pending.contains("base_ref=v1.0.3\n"));

    // Once published, same-version pushes must not publish another release.
    repo.git(&["tag", "v1.0.4"]);
    assert!(detect(before.trim()).contains("should_release=false\n"));
    repo.commit_version("1.0.5", "chore: bump version to 1.0.5");
    let next_release = detect("HEAD^");
    assert!(next_release.contains("should_release=true\n"));
    assert!(next_release.contains("base_ref=v1.0.4\n"));
}

#[test]
fn source_export_builds_keep_the_current_and_archived_changelog_entries() {
    let repo = Repository::new();
    repo.commit_version("1.0.4", "fix: support source exports");
    std::fs::remove_dir_all(repo.0.join(".git")).unwrap();

    let changelog = successful_output(repo.changelog("1.0.4"));
    assert!(changelog.starts_with("radxa-penta-top-hat-rs (1.0.4)"));
    assert!(changelog.contains("Git release history is unavailable"));
    assert!(changelog.contains("radxa-penta-top-hat-rs (1.0.3)"));
    assert!(changelog.contains("Archived release"));
}

#[test]
fn manifest_only_bump_prepares_a_committed_lockfile_for_locked_consumers() {
    let repo = Repository::new();
    std::fs::create_dir(repo.0.join("src")).unwrap();
    std::fs::write(repo.0.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        repo.0.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"radxa-penta-top-hat-rs\"\nversion = \"1.0.3\"\n",
    )
    .unwrap();
    repo.commit_version("1.0.3", "feat: establish archived release");
    repo.git(&["tag", "v1.0.3"]);
    repo.commit_version("1.0.4", "fix: prepare the next release");
    let original = checked(
        Command::new("git")
            .current_dir(&repo.0)
            .args(["rev-parse", "HEAD"]),
    );
    let prepared = checked(
        Command::new("sh")
            .current_dir(&repo.0)
            .arg("packaging/prepare-release-source.sh")
            .env("CARGO_NET_OFFLINE", "true")
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z"),
    );
    assert_ne!(prepared, original);
    let changed = checked(Command::new("git").current_dir(&repo.0).args([
        "diff",
        "--name-only",
        original.trim(),
        prepared.trim(),
    ]));
    assert_eq!(changed.trim(), "Cargo.lock");
    repo.git(&["tag", "v1.0.4", prepared.trim()]);
    let lockfile = checked(
        Command::new("git")
            .current_dir(&repo.0)
            .args(["show", "v1.0.4:Cargo.lock"]),
    );
    assert!(lockfile.contains("version = \"1.0.4\""));
    successful_output(
        Command::new("cargo")
            .current_dir(&repo.0)
            .args(["build", "--locked", "--offline"])
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap(),
    );

    let unchanged = checked(
        Command::new("sh")
            .current_dir(&repo.0)
            .arg("packaging/prepare-release-source.sh")
            .env("CARGO_NET_OFFLINE", "true"),
    );
    assert_eq!(unchanged, prepared);
    repo.git(&["checkout", "--quiet", "--detach", original.trim()]);
    let retried = checked(
        Command::new("sh")
            .current_dir(&repo.0)
            .arg("packaging/prepare-release-source.sh")
            .env("CARGO_NET_OFFLINE", "true")
            .env("GIT_AUTHOR_DATE", "2021-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2021-01-01T00:00:00Z"),
    );
    assert_eq!(retried, prepared);
    let notes = checked(
        Command::new("sh")
            .current_dir(&repo.0)
            .args(["packaging/release-notes.sh", "v1.0.3"]),
    );
    assert!(notes.contains("fix: prepare the next release"));
    assert!(!notes.contains("synchronize release lockfile"));
}

#[test]
fn unpublished_version_bumps_do_not_block_later_release_history() {
    let repo = Repository::new();
    repo.commit_version("1.0.3", "feat: establish archived release");
    repo.git(&["tag", "v1.0.3"]);
    repo.commit_version("1.0.4", "fix: retain an unpublished change");
    repo.commit_version("1.0.5", "fix: complete the next release");

    // Two bumps before publication: only the current version gets an entry,
    // but both versions' changes belong to it.
    let current = successful_output(repo.changelog("1.0.5"));
    assert!(current.starts_with("radxa-penta-top-hat-rs (1.0.5)"));
    assert!(!current.contains("radxa-penta-top-hat-rs (1.0.4)"));
    assert!(current.contains("fix: retain an unpublished change"));
    assert!(current.contains("fix: complete the next release"));

    // The workflow looks for the latest tag reachable from the prior main
    // commit. Pass that explicit base through both release output paths.
    let base = checked(Command::new("git").current_dir(&repo.0).args([
        "describe",
        "--tags",
        "--abbrev=0",
        "--match",
        "v[0-9]*",
        "HEAD^",
    ]));
    assert_eq!(base.trim(), "v1.0.3");
    let notes = checked(
        Command::new("sh")
            .current_dir(&repo.0)
            .args(["packaging/release-notes.sh", base.trim()]),
    );
    let package_changelog = successful_output(repo.changelog_from("1.0.5", base.trim()));
    for output in [&notes, &package_changelog] {
        assert!(output.contains("fix: retain an unpublished change"));
        assert!(output.contains("fix: complete the next release"));
    }

    repo.git(&["tag", "v1.0.5"]);
    repo.commit_version("1.0.6", "fix: add a later release change");

    // Subsequent builds must reconstruct the tagged release, including the
    // skipped bump's commits, without attributing them to the current release.
    let later = successful_output(repo.changelog("1.0.6"));
    let (current_entry, historical) = later.split_once("radxa-penta-top-hat-rs (1.0.5)").unwrap();
    let (prior_entry, archive) = historical
        .split_once("radxa-penta-top-hat-rs (1.0.3)")
        .unwrap();
    assert!(current_entry.starts_with("radxa-penta-top-hat-rs (1.0.6)"));
    assert!(current_entry.contains("fix: add a later release change"));
    assert!(!current_entry.contains("fix: retain an unpublished change"));
    assert!(prior_entry.contains("fix: retain an unpublished change"));
    assert!(prior_entry.contains("fix: complete the next release"));
    assert!(!prior_entry.contains("fix: add a later release change"));
    assert!(archive.contains("Archived release"));
    assert!(!later.contains("radxa-penta-top-hat-rs (1.0.4)"));
}
