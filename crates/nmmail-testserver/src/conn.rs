//! Connection plumbing shared by IMAP and SMTP: the accept loop, the registry used by
//! `drop_all_connections` / `freeze_connections`, and a socket wrapper that honours
//! those controls.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::{Shared, lock};

/// Control state of one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnState {
    Normal,
    /// Neither reads nor writes until resumed (simulated half-open connection).
    Frozen,
    /// Close the socket abruptly as soon as possible.
    Killed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnKind {
    Imap,
    Smtp,
}

struct Entry {
    kind: ConnKind,
    ctl: watch::Sender<ConnState>,
}

/// Every open connection, with the channel used to control it.
#[derive(Default)]
pub(crate) struct Registry {
    next_id: u64,
    conns: HashMap<u64, Entry>,
}

impl Registry {
    pub(crate) fn count(&self, kind: ConnKind) -> usize {
        self.conns.values().filter(|e| e.kind == kind).count()
    }

    pub(crate) fn kill_all(&self) {
        for entry in self.conns.values() {
            entry.ctl.send_replace(ConnState::Killed);
        }
    }

    pub(crate) fn freeze_all(&self, frozen: bool) {
        for entry in self.conns.values() {
            entry.ctl.send_if_modified(|state| match (*state, frozen) {
                (ConnState::Normal, true) => {
                    *state = ConnState::Frozen;
                    true
                }
                (ConnState::Frozen, false) => {
                    *state = ConnState::Normal;
                    true
                }
                _ => false,
            });
        }
    }
}

/// Removes the connection from the registry when dropped.
pub(crate) struct ConnGuard {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        lock(&self.shared.conns).conns.remove(&self.id);
    }
}

pub(crate) fn register(
    shared: &Arc<Shared>,
    kind: ConnKind,
) -> (ConnGuard, watch::Receiver<ConnState>) {
    let (tx, rx) = watch::channel(ConnState::Normal);
    let mut registry = lock(&shared.conns);
    registry.next_id += 1;
    let id = registry.next_id;
    registry.conns.insert(id, Entry { kind, ctl: tx });
    (
        ConnGuard {
            shared: shared.clone(),
            id,
        },
        rx,
    )
}

/// Accepts connections until shutdown is signalled, then aborts the connection tasks.
pub(crate) async fn accept_loop<F, Fut>(
    listener: TcpListener,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
    handler: F,
) where
    F: Fn(TcpStream, SocketAddr, Arc<Shared>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    loop {
        if *shutdown.borrow_and_update() {
            break;
        }
        tokio::select! {
            res = listener.accept() => match res {
                Ok((stream, peer)) => {
                    let _ = stream.set_nodelay(true);
                    tasks.spawn(handler(stream, peer, shared.clone()));
                }
                Err(err) => {
                    tracing::warn!("accept failed: {err}");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
            res = shutdown.changed() => {
                if res.is_err() {
                    break;
                }
            }
        }
        while tasks.try_join_next().is_some() {}
    }
    drop(listener);
    tasks.shutdown().await;
}

pub(crate) fn killed_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "connection closed by the test server",
    )
}

pub(crate) fn is_killed_error(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::ConnectionAborted
}

/// A TCP stream with a read buffer whose reads and writes honour the control state
/// (blocked while frozen, failing once killed). All operations are cancel safe with
/// respect to the buffered input.
pub(crate) struct ConnIo {
    stream: TcpStream,
    rbuf: Vec<u8>,
    ctl: watch::Receiver<ConnState>,
}

impl ConnIo {
    pub(crate) fn new(stream: TcpStream, ctl: watch::Receiver<ConnState>) -> Self {
        ConnIo {
            stream,
            rbuf: Vec::with_capacity(8192),
            ctl,
        }
    }

    /// Waits while the connection is frozen; fails once it is killed.
    async fn wait_active(ctl: &mut watch::Receiver<ConnState>) -> io::Result<()> {
        loop {
            let state = *ctl.borrow_and_update();
            match state {
                ConnState::Normal => return Ok(()),
                ConnState::Killed => return Err(killed_error()),
                ConnState::Frozen => {
                    if ctl.changed().await.is_err() {
                        return Err(killed_error());
                    }
                }
            }
        }
    }

    /// Reads more bytes into the buffer.
    async fn fill(&mut self) -> io::Result<()> {
        loop {
            Self::wait_active(&mut self.ctl).await?;
            if self.rbuf.capacity() - self.rbuf.len() < 4096 {
                self.rbuf.reserve(16 * 1024);
            }
            tokio::select! {
                res = self.stream.read_buf(&mut self.rbuf) => {
                    return match res? {
                        0 => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed the connection")),
                        _ => Ok(()),
                    };
                }
                res = self.ctl.changed() => {
                    if res.is_err() {
                        return Err(killed_error());
                    }
                }
            }
        }
    }

    /// Reads one line and returns it without its line terminator (CRLF or LF).
    /// Fails with `InvalidData` if the line grows beyond `max` bytes.
    pub(crate) async fn read_line(&mut self, max: usize) -> io::Result<Vec<u8>> {
        let mut scanned = 0;
        loop {
            if let Some(pos) = self.rbuf[scanned..].iter().position(|&b| b == b'\n') {
                let end = scanned + pos;
                let mut line: Vec<u8> = self.rbuf.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            scanned = self.rbuf.len();
            if scanned > max {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            self.fill().await?;
        }
    }

    /// Reads exactly `n` bytes.
    pub(crate) async fn read_exact_bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        while self.rbuf.len() < n {
            let missing = n - self.rbuf.len();
            self.rbuf.reserve(missing.min(1024 * 1024));
            self.fill().await?;
        }
        Ok(self.rbuf.drain(..n).collect())
    }

    /// Writes all of `data` (waits while frozen, fails once killed).
    pub(crate) async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        Self::wait_active(&mut self.ctl).await?;
        let ConnIo { stream, ctl, .. } = self;
        tokio::select! {
            res = stream.write_all(data) => res,
            _ = wait_killed(ctl) => Err(killed_error()),
        }
    }

    /// Closes the socket with a TCP reset.
    pub(crate) fn abort(self) {
        let _ = self.stream.set_zero_linger();
        drop(self.stream);
    }
}

async fn wait_killed(ctl: &mut watch::Receiver<ConnState>) {
    loop {
        if *ctl.borrow_and_update() == ConnState::Killed {
            return;
        }
        if ctl.changed().await.is_err() {
            return;
        }
    }
}
