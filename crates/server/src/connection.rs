//! Local connection targets and bounded dedicated-server readiness.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU16;
use std::time::Duration;

use ephervex_model::ConnectionTarget;
use tokio::net::TcpStream;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

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
pub async fn wait_for_server_readiness(
    target: ConnectionTarget,
    timeout: Duration,
    cancel: &CancellationToken,
) -> ServerResult<()> {
    let deadline = Instant::now() + timeout;
    let mut retry = tokio::time::interval(Duration::from_millis(100));
    retry.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ServerError::ReadinessCanceled),
            () = tokio::time::sleep_until(deadline) => {
                return Err(ServerError::ReadinessTimeout {
                    target: target.to_string(),
                    timeout_seconds: timeout.as_secs(),
                });
            }
            result = TcpStream::connect(target.socket_address()) => {
                if result.is_ok() {
                    return Ok(());
                }
            }
        }

        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ServerError::ReadinessCanceled),
            () = tokio::time::sleep_until(deadline) => {
                return Err(ServerError::ReadinessTimeout {
                    target: target.to_string(),
                    timeout_seconds: timeout.as_secs(),
                });
            }
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

    #[tokio::test]
    async fn readiness_succeeds_only_after_the_target_accepts_connections() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let target = ConnectionTarget {
            address: address.ip(),
            port: NonZeroU16::new(address.port()).expect("port"),
        };

        wait_for_server_readiness(target, Duration::from_secs(1), &CancellationToken::new())
            .await
            .expect("ready");
    }

    #[tokio::test]
    async fn readiness_timeout_and_cancellation_are_typed() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let target = ConnectionTarget {
            address: address.ip(),
            port: NonZeroU16::new(address.port()).expect("port"),
        };
        let error =
            wait_for_server_readiness(target, Duration::from_millis(25), &CancellationToken::new())
                .await
                .expect_err("closed port times out");
        assert!(matches!(error, ServerError::ReadinessTimeout { .. }));

        let canceled = CancellationToken::new();
        canceled.cancel();
        let error = wait_for_server_readiness(target, Duration::from_secs(1), &canceled)
            .await
            .expect_err("canceled");
        assert!(matches!(error, ServerError::ReadinessCanceled));
    }
}
