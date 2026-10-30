//! Minimal systemd notification (READY, WATCHDOG) over `$NOTIFY_SOCKET`; does nothing outside systemd.

pub fn notify(msg: &str) {
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        let Ok(path) = std::env::var("NOTIFY_SOCKET") else { return };
        let Ok(sock) = UnixDatagram::unbound() else { return };
        let addr = match path.strip_prefix('@') {
            Some(name) => SocketAddr::from_abstract_name(name.as_bytes()),
            None => SocketAddr::from_pathname(&path),
        };
        if let Ok(addr) = addr {
            let _ = sock.send_to_addr(msg.as_bytes(), &addr);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = msg;
}
