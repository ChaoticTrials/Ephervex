//! A server started with `--detach` is supervised by a background process, so
//! it keeps running after the command that started it and stays controllable
//! from later commands.
//!
//! The server is a shell script standing in for the JVM: it answers Ephervex's
//! Java probe like a JVM would and otherwise behaves like a server that reads
//! its console. Everything runs in a sandbox whose roots the child processes
//! resolve from the environment, so nothing here touches real servers or needs
//! a network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ephervex_model::{
    APP_ID, JavaOverride, LoaderSelection, ServerInstallation, ServerLaunch, ServerRelativePath,
    StorageClass,
};
use ephervex_storage::{Config, NewServer, Paths, Registry};
use tempfile::TempDir;

mod binary_sandbox;

struct Sandbox {
    directory: TempDir,
}

/// Kills a server the test started, so a failed assertion never leaves one running.
struct ServerGuard<'a> {
    sandbox: &'a Sandbox,
    id: String,
}

impl Drop for ServerGuard<'_> {
    fn drop(&mut self) {
        let _ = self.sandbox.run(&["server", "kill", &self.id]);
    }
}

impl Sandbox {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        for child in ["config", "data", "cache", "state", "tmp"] {
            std::fs::create_dir_all(directory.path().join(child)).expect("create root");
        }
        Self { directory }
    }

    fn base(&self) -> &Path {
        self.directory.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
        binary_sandbox::ephervex_command()
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run the ephervex binary")
    }

    fn paths(&self) -> Paths {
        use std::os::unix::fs::MetadataExt;
        let base = self.base();
        let uid = std::fs::metadata(base).expect("sandbox metadata").uid();
        Paths::from_roots(
            base.join("config").join(APP_ID),
            base.join("data").join(APP_ID),
            base.join("cache").join(APP_ID),
            base.join("state").join(APP_ID),
            base.join("tmp").join(format!("{APP_ID}-{uid}")),
        )
    }

    /// An installed persistent server whose Java is the stand-in script, with
    /// the global EULA accepted.
    fn installed_server(&self) -> String {
        let java = self.stand_in_java();
        let paths = self.paths();
        let registry = Registry::open(paths.clone()).expect("open the registry");
        let mut server = registry
            .create_server(
                NewServer::new("Detached", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create the server");
        server.installation = ServerInstallation::Installed {
            launch: ServerLaunch::Jar {
                path: ServerRelativePath::new("server.jar").expect("a relative path"),
            },
        };
        server.java_override = Some(JavaOverride::new(java));
        registry
            .update_server(&mut server)
            .expect("record the installation");

        let mut config = Config::load_or_default(&paths).expect("config");
        config.eula.accepted = true;
        config.eula.accepted_at = chrono::Utc::now().to_rfc3339();
        "https://aka.ms/MinecraftEULA".clone_into(&mut config.eula.reference);
        config.save(&paths).expect("save the consent");
        server.id.to_string()
    }

    /// A script that is a JVM to the probe and a console-reading server otherwise.
    fn stand_in_java(&self) -> PathBuf {
        let home = self.base().join("java");
        let executable = home.join("bin/java");
        std::fs::create_dir_all(executable.parent().expect("a parent")).expect("java dir");
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "amd64"
        };
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in\n\
                 *XshowSettings:properties*)\n\
                 cat >&2 <<'EOF'\n\
                 Property settings:\n    java.home = {home}\n    java.runtime.version = 17.0.9+9\n    \
                 java.vendor = Eclipse Adoptium\n    java.version = 17.0.9\n    os.arch = {arch}\n\
                 EOF\n\
                 exit 0;;\n\
                 esac\n\
                 echo 'Done (1s)! For help, type \"help\"'\n\
                 while read -r line; do\n\
                 case \"$line\" in\n\
                 stop) exit 0;;\n\
                 *) echo \"console: $line\";;\n\
                 esac\n\
                 done\n",
                home = home.display(),
            ),
        )
        .expect("write the stand-in");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("make it executable");
        std::fs::canonicalize(executable).expect("canonical path")
    }

    fn runtime_of(&self, id: &str) -> String {
        let output = self.run(&["server", "status", id, "--json"]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        let status: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("status is JSON");
        status["runtime_classification"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn wait_until_runtime_is(&self, id: &str, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let runtime = self.runtime_of(id);
            if runtime.contains(expected) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the runtime stayed {runtime}, expected {expected}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_until_output_contains(&self, id: &str, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let output = self.run(&["server", "output", id, "--lines", "100"]);
            if stdout_of(&output).contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the output never contained {needle}: {}",
                stdout_of(&output)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The session a process belongs to, from `/proc/<pid>/stat`.
fn session_of(pid: &str) -> u32 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("a live process");
    let after_name = stat.rsplit_once(") ").expect("a stat line").1;
    after_name
        .split_whitespace()
        .nth(3)
        .expect("a session field")
        .parse()
        .expect("a session id")
}

fn detached_pid(output: &Output) -> String {
    serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<serde_json::Value>()
        .filter_map(Result::ok)
        .find(|event| event["event"] == "detached")
        .and_then(|event| event["pid"].as_u64())
        .map_or_else(
            || panic!("no detached event in {}", stdout_of(output)),
            |pid| pid.to_string(),
        )
}

#[test]
fn a_detached_server_outlives_the_command_that_started_it_and_answers_later_commands() {
    let sandbox = Sandbox::new();
    let id = sandbox.installed_server();
    let _cleanup = ServerGuard {
        sandbox: &sandbox,
        id: id.clone(),
    };

    let started = sandbox.run(&["server", "start", &id, "--detach", "--json"]);
    assert!(started.status.success(), "{}", stderr_of(&started));
    let pid = detached_pid(&started);

    // The starting command has exited. The server is still its own process, in
    // a session that is not the starter's, and a new command finds it.
    assert!(Path::new(&format!("/proc/{pid}")).exists());
    assert_ne!(session_of(&pid), session_of("self"));
    sandbox.wait_until_runtime_is(&id, "controlled");

    let sent = sandbox.run(&["server", "console", &id, "hello after the starter left"]);
    assert!(sent.status.success(), "{}", stderr_of(&sent));
    sandbox.wait_until_output_contains(&id, "console: hello after the starter left");

    let stopped = sandbox.run(&["server", "stop", &id]);
    assert!(stopped.status.success(), "{}", stderr_of(&stopped));
    sandbox.wait_until_runtime_is(&id, "stopped");
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn a_detached_server_that_ends_by_itself_leaves_nothing_behind() {
    let sandbox = Sandbox::new();
    let id = sandbox.installed_server();
    let _cleanup = ServerGuard {
        sandbox: &sandbox,
        id: id.clone(),
    };

    let started = sandbox.run(&["server", "start", &id, "--detach", "--json"]);
    assert!(started.status.success(), "{}", stderr_of(&started));
    sandbox.wait_until_runtime_is(&id, "controlled");

    // Typed as a console line, `stop` ends the stand-in the way an operator's
    // `stop` ends a real server.
    let sent = sandbox.run(&["server", "console", &id, "stop"]);
    assert!(sent.status.success(), "{}", stderr_of(&sent));

    sandbox.wait_until_runtime_is(&id, "stopped");
    let again = sandbox.run(&["server", "start", &id, "--detach"]);
    assert!(
        again.status.success(),
        "a server whose runner has finished starts again: {}",
        stderr_of(&again)
    );
}

#[test]
fn a_second_detached_start_of_a_running_server_is_refused() {
    let sandbox = Sandbox::new();
    let id = sandbox.installed_server();
    let _cleanup = ServerGuard {
        sandbox: &sandbox,
        id: id.clone(),
    };
    let started = sandbox.run(&["server", "start", &id, "--detach"]);
    assert!(started.status.success(), "{}", stderr_of(&started));

    let second = sandbox.run(&["server", "start", &id, "--detach"]);

    assert!(!second.status.success(), "{}", stdout_of(&second));
    assert!(
        sandbox.runtime_of(&id).contains("controlled"),
        "the first run is untouched"
    );
}
