//! IMAP server.

mod fetch;
mod proto;
mod search;
mod session;

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpStream;

use crate::Shared;

pub(crate) use session::parse_xoauth2;

pub(crate) async fn handle_connection(stream: TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
    session::run(stream, peer, shared).await;
}
