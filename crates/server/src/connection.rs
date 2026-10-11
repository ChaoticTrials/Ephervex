//! Local connection targets and bounded dedicated-server readiness.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU16;
use std::path::Path;
use std::time::Duration;

use ephervex_model::ConnectionTarget;
use tokio::net::TcpStream;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::runtime::{ListenerOwnership, runtime_child_listens_on};
use crate::{NetworkProperties, ServerBind, ServerError, ServerResult};

impl NetworkProperties {
    /// Derives a usable local destination without changing the configured
    /// bind or the server's properties file.
    pub fn connection_target(&self) -> ServerResult<ConnectionTarget> {
        self.validate()?;
        let address = match self.bind {
            ServerBind::Localhost | ServerBind::AllInterfaces => IpAddr::V4(Ipv4Addr::LOCALHOST),
            ServerBind::Custom(IpAddr::V4(address)) if address.is_unspecified() => {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            }
            ServerBind::Custom(IpAddr::V6(address)) if address.is_unspecified() => {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            }
            ServerBind::Custom(address) => address,
        };
        Ok(ConnectionTarget {
            address,
            port: NonZeroU16::new(self.port).ok_or_else(|| ServerError::InvalidProperty {
                key: crate::SERVER_PORT_KEY.to_owned(),
                reason: "port zero is not valid",
            })?,
        })
    }
}

/// Waits until a managed server accepts TCP connections at its effective
/// target. Each attempt is bounded by the operation deadline, cancellation is
/// immediate, and connection refusal causes a deliberate retry rather than an
/// arbitrary startup sleep.
///
/// A connection counts only while the server's recorded Java process holds the
/// listening socket, so another program on the same port never stands in for it.
/// When the deadline passes while such a program was listening there, the error
/// says so instead of reporting a plain timeout.
pub async fn wait_for_server_readiness(
    server_root: &Path,
    target: ConnectionTarget,
    timeout: Duration,
    cancel: &CancellationToken,
) -> ServerResult<()> {
    let deadline = Instant::now() + timeout;
    let mut retry = tokio::time::interval(Duration::from_millis(100));
    retry.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let root: std::sync::Arc<Path> = server_root.into();
    let address = target.socket_address();
    let mut foreign_listener = false;
    let timed_out = |foreign_listener: bool| {
        let target = target.to_string();
        let timeout_seconds = timeout.as_secs();
        if foreign_listener {
            ServerError::ForeignListener {
                target,
                timeout_seconds,
            }
        } else {
            ServerError::ReadinessTimeout {
                target,
                timeout_seconds,
            }
        }
    };
    loop {
        // Ownership first, so a program that merely shares the port is never connected to.
        let check_root = std::sync::Arc::clone(&root);
        let owned =
            tokio::task::spawn_blocking(move || runtime_child_listens_on(&check_root, address));
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ServerError::ReadinessCanceled),
            () = tokio::time::sleep_until(deadline) => return Err(timed_out(foreign_listener)),
            result = async {
                let owned = match owned.await {
                    Ok(owned) => owned?,
                    // A panic in the check is a bug, not a state to retry or report.
                    Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                    Err(_) => return Err(ServerError::ReadinessCanceled),
                };
                let ready = owned == ListenerOwnership::Child
                    && TcpStream::connect(address).await.is_ok();
                Ok::<_, ServerError>((owned, ready))
            } => {
                let (owned, ready) = result?;
                if ready {
                    return Ok(());
                }
                foreign_listener = owned == ListenerOwnership::Foreign;
            }
        }

        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ServerError::ReadinessCanceled),
            () = tokio::time::sleep_until(deadline) => return Err(timed_out(foreign_listener)),
            _ = retry.tick() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn bind_addresses_become_unambiguous_local_connection_targets() {
        let cases = [
            (ServerBind::Localhost, IpAddr::V4(Ipv4Addr::LOCALHOST)),
            (ServerBind::AllInterfaces, IpAddr::V4(Ipv4Addr::LOCALHOST)),
            (
                ServerBind::Custom(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            ),
            (
                ServerBind::Custom(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ),
            (
                ServerBind::Custom("192.0.2.7".parse().expect("address")),
                "192.0.2.7".parse().expect("address"),
            ),
            (
                ServerBind::Custom(IpAddr::V6(Ipv6Addr::LOCALHOST)),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ),
        ];
        for (bind, expected) in cases {
            let target = NetworkProperties {
                online_mode: true,
                bind,
                port: 25_566,
            }
            .connection_target()
            .expect("target");
            assert_eq!(target.address, expected);
            assert_eq!(target.port.get(), 25_566);
        }
    }

    #[test]
    fn port_zero_has_no_connection_target() {
        let error = NetworkProperties {
            port: 0,
            ..NetworkProperties::default()
        }
        .connection_target()
        .expect_err("zero is not connectable");
        assert!(matches!(error, ServerError::InvalidProperty { .. }));
    }

    fn record_child(root: &Path, pid: u32) {
        let mut evidence = crate::RuntimeEvidence::for_current_supervisor().expect("evidence");
        evidence.record_child(pid).expect("child");
        crate::write_runtime_evidence(root, &evidence).expect("write evidence");
    }

    async fn listening_target() -> (tokio::net::TcpListener, ConnectionTarget) {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let target = ConnectionTarget {
            address: address.ip(),
            port: NonZeroU16::new(address.port()).expect("port"),
        };
        (listener, target)
    }

    #[tokio::test]
    async fn readiness_succeeds_once_the_server_process_holds_the_listener() {
        let root = tempfile::tempdir().expect("root");
        // Reserved without listening, then turned into the listener partway
        // through the wait, the way a starting server binds its port.
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket
            .bind((Ipv4Addr::LOCALHOST, 0).into())
            .expect("reserve port");
        let address = socket.local_addr().expect("address");
        let target = ConnectionTarget {
            address: address.ip(),
            port: NonZeroU16::new(address.port()).expect("port"),
        };
        record_child(root.path(), std::process::id());
        let bind = async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            socket.listen(16).expect("listen")
        };

        let cancel = CancellationToken::new();
        let (ready, _listener) = tokio::join!(
            wait_for_server_readiness(root.path(), target, Duration::from_secs(5), &cancel),
            bind
        );
        ready.expect("ready once listening");
    }

    #[tokio::test]
    async fn a_listener_held_by_another_process_ends_the_wait_with_a_foreign_listener_error() {
        let root = tempfile::tempdir().expect("root");
        let (_foreign, target) = listening_target().await;
        let mut server = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("server stand-in");
        record_child(root.path(), server.id());

        let error = wait_for_server_readiness(
            root.path(),
            target,
            Duration::from_secs(2),
            &CancellationToken::new(),
        )
        .await;
        server.kill().expect("kill");
        server.wait().expect("reap");
        assert!(
            matches!(error, Err(ServerError::ForeignListener { .. })),
            "{error:?}"
        );

        let empty = tempfile::tempdir().expect("root");
        let error = wait_for_server_readiness(
            empty.path(),
            target,
            Duration::from_millis(300),
            &CancellationToken::new(),
        )
        .await
        .expect_err("no recorded server process");
        assert!(matches!(error, ServerError::ReadinessTimeout { .. }));
    }

    #[tokio::test]
    async fn a_recorded_server_that_never_listens_ends_the_wait_with_a_plain_timeout() {
        let root = tempfile::tempdir().expect("root");
        let closed = tokio::net::TcpSocket::new_v4().expect("socket");
        closed
            .bind((Ipv4Addr::LOCALHOST, 0).into())
            .expect("reserve port");
        let address = closed.local_addr().expect("address");
        let target = ConnectionTarget {
            address: address.ip(),
            port: NonZeroU16::new(address.port()).expect("port"),
        };
        let mut server = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("server stand-in");
        record_child(root.path(), server.id());

        let error = wait_for_server_readiness(
            root.path(),
            target,
            Duration::from_millis(300),
            &CancellationToken::new(),
        )
        .await;
        server.kill().expect("kill");
        server.wait().expect("reap");
        assert!(
            matches!(error, Err(ServerError::ReadinessTimeout { .. })),
            "{error:?}"
        );
    }

    /// Names the port a co-listener stand-in shares, so running ignored tests
    /// by hand never starts one.
    const CO_LISTENER_PORT: &str = "EPHERVEX_TEST_CO_LISTENER_PORT";
    const CO_LISTENER_READY: &str = "co-listener ready";

    fn reuse_port_listener(port: u16) -> tokio::net::TcpListener {
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket.set_reuseport(true).expect("reuse port");
        socket
            .bind((Ipv4Addr::LOCALHOST, port).into())
            .expect("bind");
        socket.listen(16).expect("listen")
    }

    /// Another process sharing the server's port: it listens until its input closes.
    #[tokio::test]
    #[ignore = "started as a second process by the co-listener test"]
    async fn a_co_listener_stand_in_shares_the_port_until_its_input_closes() {
        let Ok(port) = std::env::var(CO_LISTENER_PORT) else {
            return;
        };
        let _listener = reuse_port_listener(port.parse().expect("port"));
        println!("{CO_LISTENER_READY}");
        let mut input = String::new();
        while std::io::stdin()
            .read_line(&mut input)
            .is_ok_and(|read| read > 0)
        {
            input.clear();
        }
    }

    #[tokio::test]
    async fn a_second_listener_on_the_port_keeps_the_server_from_being_ready() {
        use std::io::BufRead as _;

        let root = tempfile::tempdir().expect("root");
        let own = reuse_port_listener(0);
        let address = own.local_addr().expect("address");
        let target = ConnectionTarget {
            address: address.ip(),
            port: NonZeroU16::new(address.port()).expect("port"),
        };
        record_child(root.path(), std::process::id());
        let mut co_listener = std::process::Command::new(std::env::current_exe().expect("test"))
            .args([
                "--exact",
                "connection::tests::a_co_listener_stand_in_shares_the_port_until_its_input_closes",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CO_LISTENER_PORT, address.port().to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("co-listener");
        let mut output =
            std::io::BufReader::new(co_listener.stdout.take().expect("output")).lines();
        // The harness prefixes the line with the test's name.
        let started = output
            .by_ref()
            .any(|line| line.is_ok_and(|line| line.contains(CO_LISTENER_READY)));
        // Drained to the end, so the stand-in's closing summary never meets a closed pipe.
        let drained = std::thread::spawn(move || output.for_each(drop));

        let shared = wait_for_server_readiness(
            root.path(),
            target,
            Duration::from_secs(2),
            &CancellationToken::new(),
        )
        .await;
        drop(co_listener.stdin.take());
        let status = co_listener.wait().expect("co-listener exits");
        drained.join().expect("output drained");
        assert!(started, "the co-listener started");
        assert!(status.success(), "the co-listener exited cleanly: {status}");
        assert!(
            matches!(shared, Err(ServerError::ForeignListener { .. })),
            "{shared:?}"
        );

        wait_for_server_readiness(
            root.path(),
            target,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .expect("ready once the server is the only listener");
    }

    #[tokio::test]
    async fn readiness_timeout_and_cancellation_are_typed() {
        let root = tempfile::tempdir().expect("root");
        // Bound but never listening, so no other socket can take the port
        // while the test runs, and no server process is recorded.
        let closed = tokio::net::TcpSocket::new_v4().expect("socket");
        closed
            .bind((Ipv4Addr::LOCALHOST, 0).into())
            .expect("reserve port");
        let address = closed.local_addr().expect("address");
        let target = ConnectionTarget {
            address: address.ip(),
            port: NonZeroU16::new(address.port()).expect("port"),
        };
        let error = wait_for_server_readiness(
            root.path(),
            target,
            Duration::from_millis(25),
            &CancellationToken::new(),
        )
        .await
        .expect_err("closed port times out");
        assert!(matches!(error, ServerError::ReadinessTimeout { .. }));

        let canceled = CancellationToken::new();
        canceled.cancel();
        let error =
            wait_for_server_readiness(root.path(), target, Duration::from_secs(1), &canceled)
                .await
                .expect_err("canceled");
        assert!(matches!(error, ServerError::ReadinessCanceled));
    }
}
