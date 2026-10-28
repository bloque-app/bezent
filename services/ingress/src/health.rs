// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-ingress health [<config-path>]`: the container's health check (issue #61).
//!
//! The running ingress answers `GET /health` with 200 only when its store answers a query, and it
//! binds its address only after the store was reached and the login checked ([`crate::serve`]). So
//! the check asks the running instance, on the loopback, at the port the same configuration file
//! names. It opens no store connection of its own and sends no credential: the request carries
//! nothing, and nothing it prints comes from the file.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::Config;

/// The configuration path the image's `CMD` names; `health` uses it when it is given none.
pub const DEFAULT_CONFIG: &str = "/etc/bezent/ingress.toml";

/// How long the whole probe may take: inside the 5 s a health command keeps to.
pub const TIMEOUT: Duration = Duration::from_secs(4);

/// The loopback address that reaches a listener bound at `listen`: an unspecified address
/// (`0.0.0.0`, `[::]`) is reached through the loopback of its family, any other as it is.
pub fn target(listen: SocketAddr) -> SocketAddr {
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, listen.port())
}

/// Asks the instance at `addr` for `/health`: healthy only on a 200, within `timeout`.
pub async fn probe(addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let ask = async {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        stream
            .write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        // The status line is all that is read.
        let mut head = [0u8; 12];
        stream.read_exact(&mut head).await?;
        Ok::<_, std::io::Error>(head)
    };
    match tokio::time::timeout(timeout, ask).await {
        Err(_) => Err("no answer in time".to_owned()),
        Ok(Err(_)) => Err("the ingress does not answer".to_owned()),
        Ok(Ok(head)) if &head == b"HTTP/1.1 200" || &head == b"HTTP/1.0 200" => Ok(()),
        Ok(Ok(_)) => Err("the ingress answers that it is not healthy".to_owned()),
    }
}

/// The check: reads the configuration the service reads, and probes the address it names. A
/// configuration that cannot be read is reported with fixed text, never with the parser's message,
/// which can quote a line of the file.
pub async fn check(config: &Path) -> Result<(), String> {
    let config = Config::from_file(config)
        .map_err(|_| bezent_health::CONFIGURATION_UNREADABLE.to_owned())?;
    probe(target(config.listen), TIMEOUT).await
}
