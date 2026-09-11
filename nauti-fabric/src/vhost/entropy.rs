//! Remote entropy stub — the M7 remote-resource variant.
//!
//! The vhost-user protocol itself cannot cross hosts: the frontend passes
//! guest-RAM file descriptors to the backend with SCM_RIGHTS, which only
//! works over a Unix socket on the same machine. What *can* cross hosts is
//! the device payload. This module is the honest stub for that boundary:
//!
//! * [`entropy_serve`] runs on the host that owns the resource (the entropy
//!   source). It answers length-prefixed requests with random bytes read
//!   from `/dev/urandom`.
//! * [`entropy_pump`] runs on the VM host and forwards bytes from a remote
//!   server into a local FIFO, which the virtio-rng backend then serves to
//!   the guest (see `super::rng`).
//!
//! Wire format: the client sends one `u32` big-endian length per request
//! (1..=1 MiB); the server replies with exactly that many random bytes and
//! keeps the connection open for the next request. A length of 0 or above
//! the cap is a protocol error and the server closes the connection. There
//! is no framing beyond this — it is a stub, not a transport.

use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    thread,
    time::Duration,
};

/// Largest single request the server will honor (1 MiB).
pub const MAX_REQUEST_BYTES: u32 = 1 << 20;
/// Default chunk the pump asks for per request (64 KiB).
pub const DEFAULT_CHUNK: u32 = 64 << 10;

#[derive(Debug, thiserror::Error)]
pub enum EntropyError {
    #[error("cannot bind entropy server at {0}: {1}")]
    Bind(String, std::io::Error),
    #[error("cannot connect to entropy server at {0}: {1}")]
    Connect(String, std::io::Error),
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Serve entropy requests on `listen` (host:port), one thread per
/// connection, forever. Bytes come from `/dev/urandom` on the serving host.
pub fn entropy_serve(listen: &str) -> Result<(), EntropyError> {
    let listener = TcpListener::bind(listen)
        .map_err(|e| EntropyError::Bind(listen.to_string(), e))?;
    entropy_serve_on(listener)
}

/// Serve on an already-bound listener (split out so tests can bind port 0).
pub fn entropy_serve_on(listener: TcpListener) -> Result<(), EntropyError> {
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = serve_conn(stream);
                });
            }
            Err(e) => return Err(EntropyError::Io(e)),
        }
    }
    Ok(())
}

fn serve_conn(mut stream: TcpStream) -> Result<(), EntropyError> {
    let mut urandom = std::fs::File::open("/dev/urandom")?;
    loop {
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf) {
            Ok(()) => {}
            // Clean disconnect between requests is a normal shutdown.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(EntropyError::Io(e)),
        }
        let len = u32::from_be_bytes(len_buf);
        if len == 0 || len > MAX_REQUEST_BYTES {
            return Err(EntropyError::Protocol(format!(
                "requested length {len} outside 1..={MAX_REQUEST_BYTES}"
            )));
        }
        let mut buf = vec![0u8; len as usize];
        urandom.read_exact(&mut buf)?;
        stream.write_all(&buf)?;
    }
}

/// Fetch exactly `len` random bytes from the server at `addr`, with a
/// connect+read deadline. This is the client half; [`entropy_pump`] wraps it
/// in a loop, and tests use it directly.
pub fn entropy_fetch(addr: &str, len: u32, deadline: Duration) -> Result<Vec<u8>, EntropyError> {
    if len == 0 || len > MAX_REQUEST_BYTES {
        return Err(EntropyError::Protocol(format!(
            "requested length {len} outside 1..={MAX_REQUEST_BYTES}"
        )));
    }
    let mut stream = TcpStream::connect(addr)
        .map_err(|e| EntropyError::Connect(addr.to_string(), e))?;
    stream.set_read_timeout(Some(deadline))?;
    stream.set_write_timeout(Some(deadline))?;
    stream.write_all(&len.to_be_bytes())?;
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

/// Pump entropy from the remote server at `connect` into the local file at
/// `out` — normally a FIFO the virtio-rng backend reads (created with
/// `mkfifo` here if absent). Runs forever; when the remote server dies the
/// pump returns a typed error and closes the FIFO writer. The backend holds
/// the FIFO read-write, so the guest sees stalled reads — never a zero-byte
/// completion.
pub fn entropy_pump(connect: &str, out: &Path, chunk: u32) -> Result<(), EntropyError> {
    if chunk == 0 || chunk > MAX_REQUEST_BYTES {
        return Err(EntropyError::Protocol(format!(
            "pump chunk {chunk} outside 1..={MAX_REQUEST_BYTES}"
        )));
    }
    if !out.exists() {
        let status = std::process::Command::new("mkfifo")
            .arg(out)
            .status()
            .map_err(|e| EntropyError::Protocol(format!("mkfifo unavailable: {e}")))?;
        if !status.success() {
            return Err(EntropyError::Protocol(format!(
                "mkfifo {} failed with {status}",
                out.display()
            )));
        }
    }

    let mut file = OpenOptions::new().write(true).open(out)?;
    loop {
        let bytes = entropy_fetch(connect, chunk, Duration::from_secs(30))?;
        file.write_all(&bytes)?;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// Start a server on a loopback ephemeral port; return its address.
    fn spawn_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let _ = entropy_serve_on(listener);
        });
        addr
    }

    #[test]
    fn roundtrip_returns_exact_length() {
        let addr = spawn_server();
        let a = entropy_fetch(&addr, 64, Duration::from_secs(5)).unwrap();
        let b = entropy_fetch(&addr, 64, Duration::from_secs(5)).unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
        // Two draws being identical is a 2^-512 event; treat as never.
        assert_ne!(a, b, "two entropy draws must differ");
    }

    #[test]
    fn rejects_bad_lengths() {
        let addr = spawn_server();
        assert!(matches!(
            entropy_fetch(&addr, 0, Duration::from_secs(2)),
            Err(EntropyError::Protocol(_))
        ));
        assert!(matches!(
            entropy_fetch(&addr, MAX_REQUEST_BYTES + 1, Duration::from_secs(2)),
            Err(EntropyError::Protocol(_))
        ));
    }

    #[test]
    fn dead_server_is_typed_connect_error() {
        // Bind then drop to find a port nothing listens on.
        let gone = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .to_string();
        assert!(matches!(
            entropy_fetch(&gone, 16, Duration::from_secs(2)),
            Err(EntropyError::Connect(_, _))
        ));
    }

    #[test]
    fn server_closes_on_protocol_violation() {
        let addr = spawn_server();
        let mut stream = TcpStream::connect(&addr).unwrap();
        // Length 0 is illegal; the server must close rather than comply.
        stream.write_all(&0u32.to_be_bytes()).unwrap();
        stream.flush().unwrap();
        let mut buf = [0u8; 1];
        let start = Instant::now();
        let mut saw_eof = false;
        while start.elapsed() < Duration::from_secs(3) {
            match stream.read(&mut buf) {
                Ok(0) => {
                    saw_eof = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    saw_eof = true; // connection reset also proves closure
                    break;
                }
            }
        }
        assert!(saw_eof, "server must close on protocol violation");
    }

    #[test]
    fn pump_writes_fifo_from_server() {
        let addr = spawn_server();
        let dir = std::env::temp_dir().join(format!("nauti-pump-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("entropy.fifo");

        let pump_addr = addr.clone();
        let pump_fifo = fifo.clone();
        let pump = thread::spawn(move || entropy_pump(&pump_addr, &pump_fifo, 256));
        // Give the pump a moment to mkfifo and connect.
        thread::sleep(Duration::from_millis(300));

        // Read one chunk out of the FIFO.
        let mut f = OpenOptions::new().read(true).write(true).open(&fifo).unwrap();
        let mut buf = vec![0u8; 256];
        f.read_exact(&mut buf).unwrap();
        assert!(buf.iter().any(|&b| b != 0), "entropy must not be all zeros");

        drop(f);
        drop(pump); // pump thread is detached; test process reaps on exit
        std::fs::remove_dir_all(&dir).ok();
    }
}

