use anyhow::{Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use tokio::net::TcpListener;

pub async fn bind_listener(addr: SocketAddr) -> Result<TcpListener> {
    if is_dual_stack_address(addr) {
        let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_only_v6(false)?;
        socket.set_reuse_address(true)?;
        socket.bind(&addr.into())?;
        socket.listen(1024)?;
        socket.set_nonblocking(true)?;
        let listener: StdTcpListener = socket.into();
        return TcpListener::from_std(listener).context("creating dual-stack listener");
    }

    TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding listener on {addr}"))
}

pub fn is_dual_stack_address(addr: SocketAddr) -> bool {
    addr.is_ipv6() && addr.ip().is_unspecified()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unspecified_ipv6_is_dual_stack() {
        let addr: SocketAddr = "[::]:11435".parse().expect("parse");
        assert!(is_dual_stack_address(addr));
    }

    #[test]
    fn loopback_ipv6_is_not_marked_dual_stack() {
        let addr: SocketAddr = "[::1]:11435".parse().expect("parse");
        assert!(!is_dual_stack_address(addr));
    }
}
