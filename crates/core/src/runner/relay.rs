//! The runner's side of the pipe to its starter.

use serde::Serialize;
use tokio::io::{AsyncWrite, AsyncWriteExt as _};

/// Where a runner's messages go, and whether anyone is still reading them.
///
/// A starter that has exited closes the pipe. The host then runs on without
/// being told who watches it, so a failed write ends the relay and nothing else.
pub(super) struct Relay<W> {
    out: W,
    open: bool,
}

impl<W: AsyncWrite + Unpin> Relay<W> {
    pub(super) const fn new(out: W) -> Self {
        Self { out, open: true }
    }

    pub(super) async fn send<M: Serialize>(&mut self, message: &M) {
        if !self.open {
            return;
        }
        let Ok(mut line) = serde_json::to_vec(message) else {
            return;
        };
        line.push(b'\n');
        if self.out.write_all(&line).await.is_err() || self.out.flush().await.is_err() {
            self.open = false;
        }
    }
}
