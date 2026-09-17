//! End-to-end behavior of the managed runtime provider against a loopback
//! HTTP server.
//!
//! Nothing here touches Adoptium. CI that depends on a third party's uptime
//! fails for reasons that have nothing to do with the change under test, so the
//! API response is a fixture served by `wiremock` and the
//! "JDK" is a tarball this file builds - including one built specifically to
//! escape its extraction directory.
//!
//! The synthetic JDK's `bin/java` is a shell script that prints the same
//! property block a real JVM does. That is the entire contract the provider
//! relies on, which is what makes the substitution honest rather than a
//! shortcut: if the provider ever needed more than that, this test would stop
//! being representative and would have to be replaced rather than patched.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::path::Path;

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter,
};
use ephervex_java::{JavaDiscovery, JavaError, ManagedJavaProvider};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RELEASE_NAME: &str = "jdk-21.0.12+8";
const ARCHIVE_ROUTE: &str = "/download/OpenJDK21U-jdk_x64_linux_hotspot_21.0.12_8.tar.gz";

/// The architecture the running build asks Adoptium for.
///
/// Mirrors what the provider itself computes, so the mocked route matches on
/// every machine the suite runs on rather than only on x86-64.
fn architecture() -> &'static str {
    ephervex_java::AdoptiumArchitecture::current()
        .expect("this build runs on an architecture Adoptium publishes for")
        .as_str()
}

/// A `bin/java` that reports the given version the way a real JVM does.
fn java_script(version: &str, home: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.home = {home}\n    \
         java.runtime.version = {version}+8\n    java.vendor = Eclipse Adoptium\n    \
         java.version = {version}\n    os.arch = amd64\nEOF\n"
    )
    .into_bytes()
}

/// A tarball shaped like Temurin's: everything under one wrapper directory,
/// including the relative `legal/` symlinks the real archive is full of.
fn temurin_archive() -> Vec<u8> {
    let home = format!("/opt/{RELEASE_NAME}");
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));

    for (name, mode, contents) in [
        (
            format!("{RELEASE_NAME}/release"),
            0o644,
            b"JAVA_VERSION=\"21.0.12\"\n".to_vec(),
        ),
        (
            format!("{RELEASE_NAME}/bin/java"),
            0o755,
            java_script("21.0.12", &home),
        ),
        (
            format!("{RELEASE_NAME}/legal/java.base/LICENSE"),
            0o644,
            b"GPLv2 with Classpath Exception\n".to_vec(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder
            .append_data(&mut header, &name, contents.as_slice())
            .expect("append");
    }

    // Verbatim in shape from the live Temurin 16 archive, which carries 208 of
    // these. An extractor that refuses relative symlinks refuses the product,
    // so the fixture has to contain one or the offline suite would keep passing
    // while the real download failed.
    let mut link = tar::Header::new_gnu();
    link.set_size(0);
    link.set_mode(0o777);
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_link_name("../java.base/LICENSE")
        .expect("link name");
    link.set_path(format!("{RELEASE_NAME}/legal/jdk.naming.rmi/LICENSE"))
        .expect("path");
    link.set_cksum();
    builder
        .append(&link, std::io::empty())
        .expect("append link");

    builder.into_inner().expect("finish").finish().expect("gz")
}

/// A tarball that tries to write outside wherever it is extracted.
///
/// `tar::Builder` refuses to *write* a `..` path, so the header's name field is
/// filled in directly - a hostile archive was not built with this crate.
fn escaping_archive() -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, contents) in [
        (
            format!("{RELEASE_NAME}/bin/java"),
            java_script("21.0.12", "/opt"),
        ),
        (
            format!("{RELEASE_NAME}/../../../../../../tmp/ephervex-pwned"),
            b"owned\n".to_vec(),
        ),
    ] {
        let mut header = tar::Header::new_old();
        let bytes = name.as_bytes();
        header.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append(&header, contents.as_slice())
            .expect("append");
    }
    builder.into_inner().expect("finish").finish().expect("gz")
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// The `/v3/assets/latest/{major}/hotspot` response, in the live shape.
fn release_index(server: &MockServer, checksum: &str, size: usize) -> String {
    format!(
        r#"[
          {{
            "binary": {{
              "architecture": "{arch}",
              "image_type": "jdk",
              "jvm_impl": "hotspot",
              "os": "linux",
              "package": {{
                "checksum": "{checksum}",
                "checksum_link": "{base}{route}.sha256.txt",
                "link": "{base}{route}",
                "name": "OpenJDK21U-jdk_x64_linux_hotspot_21.0.12_8.tar.gz",
                "size": {size}
              }}
            }},
            "release_link": "{base}/releases/tag/{release}",
            "release_name": "{release}",
            "vendor": "eclipse",
            "version": {{"major": 21, "minor": 0, "security": 12, "build": 8}}
          }}
        ]"#,
        arch = architecture(),
        base = server.uri(),
        route = ARCHIVE_ROUTE,
        release = RELEASE_NAME,
    )
}

async fn mount(server: &MockServer, archive: Vec<u8>, checksum: &str) {
    let size = archive.len();
    Mock::given(method("GET"))
        .and(path("/v3/assets/latest/21/hotspot"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(release_index(server, checksum, size)),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(ARCHIVE_ROUTE))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .mount(server)
        .await;
}

fn provider(server: &MockServer, cache: &Path, java_dir: &Path) -> ManagedJavaProvider {
    let manager = DownloadManager::new(DownloaderConfig {
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("download manager");
    ManagedJavaProvider::new(manager, CacheLayout::new(cache), java_dir)
        .expect("provider")
        .with_api_base(&server.uri())
        .expect("api base")
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("managed-java-test")
}

#[tokio::test]
async fn a_missing_runtime_is_installed_under_the_data_directory_and_is_executable() {
    let server = MockServer::start().await;
    let archive = temurin_archive();
    let checksum = sha256(&archive);
    mount(&server, archive, &checksum).await;

    let temp = tempfile::tempdir().expect("temp");
    let java_dir = temp.path().join("data/java");
    let provider = provider(&server, &temp.path().join("cache"), &java_dir);

    let installed = provider
        .install(21, &reporter(), &CancellationToken::new())
        .await
        .expect("the managed provider installs a runtime");

    assert!(installed.newly_installed);
    assert_eq!(installed.runtime.version.major, 21);
    assert_eq!(installed.runtime.version.update, 12);

    // the install goes under `$XDG_DATA_HOME/ephervex/java/<major>/<vendor-build>/`
    // and nowhere else. Everything below the temporary root is writable by the
    // test user, which is the whole "no root privileges" claim made concrete.
    assert_eq!(
        installed.directory,
        java_dir.join("21/eclipse-jdk-21.0.12+8")
    );
    assert!(installed.directory.starts_with(&java_dir));

    let executable = installed.directory.join("bin/java");
    assert!(executable.is_file(), "the runtime must exist on disk");

    // The archive's relative symlink is preserved as a symlink rather than
    // refused or flattened into a copy.
    #[cfg(unix)]
    {
        let license = installed.directory.join("legal/jdk.naming.rmi/LICENSE");
        assert!(
            license
                .symlink_metadata()
                .expect("symlink metadata")
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&license)
                .expect("read through the link")
                .starts_with("GPLv2")
        );
    }
    assert_eq!(
        installed
            .runtime
            .executable
            .canonicalize()
            .expect("canonical"),
        executable.canonicalize().expect("canonical"),
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&executable)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "an inert bin/java is not a runtime");
    }
}

#[tokio::test]
async fn an_installed_runtime_is_then_found_by_discovery_and_marked_managed() {
    let server = MockServer::start().await;
    let archive = temurin_archive();
    let checksum = sha256(&archive);
    mount(&server, archive, &checksum).await;

    let temp = tempfile::tempdir().expect("temp");
    let java_dir = temp.path().join("data/java");
    provider(&server, &temp.path().join("cache"), &java_dir)
        .install(21, &reporter(), &CancellationToken::new())
        .await
        .expect("install");

    // The layout the installer writes and the layout discovery scans have to
    // agree, and nothing else in the suite checks both ends at once.
    let inventory = JavaDiscovery::isolated()
        .with_managed_root(&java_dir)
        .discover()
        .await;
    assert_eq!(inventory.runtimes.len(), 1);
    assert!(inventory.runtimes[0].is_managed());
    assert_eq!(inventory.runtimes[0].version.major, 21);
}

#[tokio::test]
async fn installing_twice_reuses_what_is_already_there() {
    let server = MockServer::start().await;
    let archive = temurin_archive();
    let checksum = sha256(&archive);
    mount(&server, archive, &checksum).await;

    let temp = tempfile::tempdir().expect("temp");
    let java_dir = temp.path().join("data/java");
    let provider = provider(&server, &temp.path().join("cache"), &java_dir);

    let first = provider
        .install(21, &reporter(), &CancellationToken::new())
        .await
        .expect("install");
    let second = provider
        .install(21, &reporter(), &CancellationToken::new())
        .await
        .expect("install again");

    assert!(first.newly_installed);
    assert!(
        !second.newly_installed,
        "a second install must not re-download 200 MB of JDK"
    );
    assert_eq!(first.directory, second.directory);
}

#[tokio::test]
async fn an_archive_whose_checksum_does_not_match_is_never_extracted() {
    let server = MockServer::start().await;
    let archive = temurin_archive();
    // A digest of the right shape for a different file: what a tampered CDN
    // response or a corrupted transfer looks like.
    let wrong = sha256(b"something else entirely");
    mount(&server, archive, &wrong).await;

    let temp = tempfile::tempdir().expect("temp");
    let java_dir = temp.path().join("data/java");

    let error = provider(&server, &temp.path().join("cache"), &java_dir)
        .install(21, &reporter(), &CancellationToken::new())
        .await
        .expect_err("a checksum mismatch must fail the install");
    assert!(
        matches!(error, JavaError::ManagedDownloadFailed { .. }),
        "{error:?}"
    );
    assert!(
        !java_dir.join("21/eclipse-jdk-21.0.12+8").exists(),
        "nothing may be installed from an archive that failed verification"
    );
}

#[tokio::test]
async fn an_archive_that_tries_to_escape_its_directory_is_refused() {
    let server = MockServer::start().await;
    let archive = escaping_archive();
    let checksum = sha256(&archive);
    mount(&server, archive, &checksum).await;

    let temp = tempfile::tempdir().expect("temp");
    let java_dir = temp.path().join("data/java");

    let error = provider(&server, &temp.path().join("cache"), &java_dir)
        .install(21, &reporter(), &CancellationToken::new())
        .await
        .expect_err("a traversal entry must abort the install");
    assert!(
        matches!(error, JavaError::ManagedArchiveRejected { .. }),
        "{error:?}"
    );

    // The archive is checksum-valid and served over the expected route: only
    // the extractor stands between it and an arbitrary write.
    assert!(
        !java_dir.join("21/eclipse-jdk-21.0.12+8").exists(),
        "a refused archive must leave no install behind"
    );
    assert!(
        !Path::new("/tmp/ephervex-pwned").exists(),
        "the escaping entry must not have been written"
    );
    let staging = java_dir.join("21/.eclipse-jdk-21.0.12+8.incomplete");
    assert!(
        !staging.exists(),
        "the staging directory must be cleaned up"
    );
}

#[tokio::test]
async fn a_major_adoptium_does_not_publish_fails_with_a_clear_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/assets/latest/21/hotspot"))
        // What the live API returns for a combination it does not serve: an
        // empty array with a 200, not an error status.
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;

    let temp = tempfile::tempdir().expect("temp");
    let error = provider(
        &server,
        &temp.path().join("cache"),
        &temp.path().join("data/java"),
    )
    .install(21, &reporter(), &CancellationToken::new())
    .await
    .expect_err("an empty index is not a runtime");
    assert!(
        matches!(error, JavaError::ManagedReleaseMissing { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_installed_runtime_is_removed_with_the_directory_that_held_it() {
    let server = MockServer::start().await;
    let archive = temurin_archive();
    let checksum = sha256(&archive);
    mount(&server, archive, &checksum).await;

    let temp = tempfile::tempdir().expect("temp");
    let java_dir = temp.path().join("data/java");
    let provider = provider(&server, &temp.path().join("cache"), &java_dir);

    let installed = provider
        .install(21, &reporter(), &CancellationToken::new())
        .await
        .expect("install");
    let directory = installed.directory.clone();
    assert!(directory.is_dir());

    let removed = provider
        .remove(&installed.runtime.executable)
        .await
        .expect("a runtime the provider installed is one it can remove");

    assert_eq!(removed.canonicalize().ok(), directory.canonicalize().ok());
    assert!(!directory.exists(), "the install directory has to be gone");
    // The major's own directory stays: it is where the next install of this
    // major goes, and it was not what the user asked to remove.
    assert!(java_dir.join("21").is_dir());

    let inventory = JavaDiscovery::isolated()
        .with_managed_root(&java_dir)
        .discover()
        .await;
    assert!(
        inventory.runtimes.is_empty(),
        "a removed runtime must not still be discovered: {:?}",
        inventory.runtimes
    );
}

/// Removal deletes a directory tree and takes its path from a frontend, so
/// what it accepts is the question that matters. Anything that is not one of
/// this provider's own `<java_dir>/<major>/<build>` installs is refused
/// before anything is deleted.
#[tokio::test]
async fn only_the_providers_own_installs_can_be_removed() {
    let server = MockServer::start().await;
    let temp = tempfile::tempdir().expect("temp");
    let java_dir = temp.path().join("data/java");
    let elsewhere = temp.path().join("usr/lib/jvm/java-21-openjdk-amd64");
    std::fs::create_dir_all(java_dir.join("21/eclipse-jdk-21.0.12+8/bin")).expect("install dir");
    std::fs::create_dir_all(elsewhere.join("bin")).expect("system dir");
    std::fs::write(elsewhere.join("bin/java"), b"#!/bin/sh\n").expect("system java");
    let provider = provider(&server, &temp.path().join("cache"), &java_dir);

    for refused in [
        // A runtime somebody else installed.
        elsewhere.join("bin/java"),
        // The major's directory, which is every build of that major.
        java_dir.join("21/bin/java"),
        // The root, which is every runtime there is.
        java_dir.join("bin/java"),
        // A path that is not shaped like a runtime at all.
        java_dir.join("21/eclipse-jdk-21.0.12+8/release"),
    ] {
        let error = provider
            .remove(&refused)
            .await
            .expect_err("only this provider's own installs may be removed");
        assert!(
            matches!(error, JavaError::NotManaged { .. }),
            "{}: {error:?}",
            refused.display()
        );
    }

    assert!(
        elsewhere.join("bin/java").exists(),
        "a refused removal must not have deleted anything"
    );
    assert!(java_dir.join("21/eclipse-jdk-21.0.12+8").is_dir());
}
