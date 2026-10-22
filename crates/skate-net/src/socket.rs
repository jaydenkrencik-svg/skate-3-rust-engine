//! Datagram queues must hold a render frame's bulk traffic, including relay IPC.
use std::{io, net::UdpSocket};
#[cfg(not(target_arch = "wasm32"))]
pub fn configure(socket: &UdpSocket) -> io::Result<()> {
    let socket = socket2::SockRef::from(socket);
    socket.set_recv_buffer_size(4 * 1024 * 1024)?;
    socket.set_send_buffer_size(4 * 1024 * 1024)?;
    socket.set_nonblocking(true)
}
/// Browsers have no UDP; web builds carry datagrams over the server's WebSocket relay.
#[cfg(target_arch = "wasm32")]
pub fn configure(_socket: &UdpSocket) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "UDP sockets are unavailable in the browser"))
}
