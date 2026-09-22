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
