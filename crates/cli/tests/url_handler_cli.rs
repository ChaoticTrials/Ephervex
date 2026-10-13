//! Command-line coverage for `url-handler unregister`: what it reports comes
//! from the status read after removal, because a package-installed entry is
//! not Ephervex's to delete and keeps handling links.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;

const ENTRY: &str = "ephervex-url-handler.desktop";

struct Sandbox {
    directory: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        for child in ["config", "data", "cache", "state", "tmp", "system"] {
            std::fs::create_dir_all(directory.path().join(child)).expect("create root");
        }
        Self { directory }
    }

    fn base(&self) -> &Path {
        self.directory.path()
    }

    fn user_entry(&self) -> PathBuf {
        self.base().join("data").join("applications").join(ENTRY)
    }

    fn system_entry(&self) -> PathBuf {
        self.base().join("system").join("applications").join(ENTRY)
    }

    fn write_entry(path: &Path, program: &str) {
        std::fs::create_dir_all(path.parent().expect("applications directory")).expect("dir");
        std::fs::write(
            path,
            format!("[Desktop Entry]\nType=Application\nExec={program} %u\n"),
        )
        .expect("write a desktop entry");
    }

    fn install_user_entry(&self) {
        Self::write_entry(&self.user_entry(), "/usr/bin/true");
    }

    fn install_system_entry(&self) {
        Self::write_entry(&self.system_entry(), "/usr/bin/true");
    }

    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
        let output = binary_sandbox::ephervex_command()
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_DATA_DIRS", base.join("system"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run the ephervex binary");
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_slice(&self.run(args).stdout).expect("JSON on stdout")
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.run(args).stdout).expect("UTF-8 output")
    }
}

#[test]
fn unregister_keeps_a_package_installed_entry_and_reports_that_links_still_open() {
    let sandbox = Sandbox::new();
    sandbox.install_user_entry();
    sandbox.install_system_entry();

    let report = sandbox.json(&["--json", "url-handler", "unregister"]);

    assert!(!sandbox.user_entry().exists(), "the user's own entry goes");
    assert!(
        sandbox.system_entry().exists(),
        "a package manager's file is never touched"
    );
    assert_eq!(report["removed"], true);
    assert_eq!(report["registered"], true);
    assert_eq!(
        report["entry"],
        sandbox.system_entry().to_string_lossy().as_ref()
    );
}

#[test]
fn unregister_text_names_the_package_entry_that_still_handles_links() {
    let sandbox = Sandbox::new();
    sandbox.install_user_entry();
    sandbox.install_system_entry();

    let text = sandbox.text(&["url-handler", "unregister"]);

    assert!(
        !text.contains("no longer opens"),
        "links still open, so this must not say they stopped:\n{text}"
    );
    assert!(text.contains("still opens ephervex:// links"), "{text}");
    assert!(
        text.contains(&sandbox.system_entry().display().to_string()),
        "the remaining entry's path is named:\n{text}"
    );
}

#[test]
fn unregister_without_an_own_entry_does_not_claim_links_were_not_opening() {
    let sandbox = Sandbox::new();
    sandbox.install_system_entry();

    let report = sandbox.json(&["--json", "url-handler", "unregister"]);
    assert_eq!(report["removed"], false);
    assert_eq!(report["registered"], true);

    let text = sandbox.text(&["url-handler", "unregister"]);
    assert!(
        !text.contains("was not opening"),
        "a package entry does open links:\n{text}"
    );
    assert!(text.contains("still opens ephervex:// links"), "{text}");
    assert!(sandbox.system_entry().exists());
}

#[test]
fn unregister_with_only_an_own_entry_stops_links_opening() {
    let sandbox = Sandbox::new();
    sandbox.install_user_entry();

    let text = sandbox.text(&["url-handler", "unregister"]);
    assert!(text.contains("no longer opens ephervex:// links"), "{text}");

    sandbox.install_user_entry();
    let report = sandbox.json(&["--json", "url-handler", "unregister"]);
    assert_eq!(report["removed"], true);
    assert_eq!(report["registered"], false);
}

#[test]
fn unregister_with_nothing_installed_reports_nothing_was_opening_links() {
    let sandbox = Sandbox::new();

    let report = sandbox.json(&["--json", "url-handler", "unregister"]);
    assert_eq!(report["removed"], false);
    assert_eq!(report["registered"], false);

    let text = sandbox.text(&["url-handler", "unregister"]);
    assert!(text.contains("was not opening ephervex:// links"), "{text}");
}
