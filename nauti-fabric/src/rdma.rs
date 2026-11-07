//! The RoCE data path (feature `rdma`): a peer's registered memory read by RDMA, kernel bypassed on both sides.
//!
//! The server registers a window of its mapped model file (read-only, remote read); the client registers its own
//! buffer and RDMA READs chunks straight into it.  The queue pairs find each other through the control link: the
//! numbers in [`QpInfo`] travel in `data_quic`'s RdmaHello, which is the fabric's shape (TRANSPORT_ANALYSIS.md):
//! control over Iroh, bulk bytes over the hardware path.  Measured c4 <-> t440 (BCM57414, RoCE v2): 8 us a 4 KiB
//! write.  libibverbs is reached through csrc/rdma_shim.c.
use std::ffi::{c_char, c_int, c_void, CString};
use std::io;

#[repr(C)]
pub struct nauti_rdma {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn nauti_rdma_open(dev: *const c_char, port: c_int, gid_index: c_int, err: *mut c_char, errlen: usize) -> *mut nauti_rdma;
    fn nauti_rdma_reg(h: *mut nauti_rdma, addr: *mut c_void, len: usize, remote_read: c_int, lkey: *mut u32,
                      rkey: *mut u32, err: *mut c_char, errlen: usize) -> *mut c_void;
    fn nauti_rdma_dereg(mr: *mut c_void) -> c_int;
    fn nauti_rdma_local(h: *mut nauti_rdma, qpn: *mut u32, psn: *mut u32, gid: *mut u8, lid: *mut u16, mtu: *mut c_int);
    fn nauti_rdma_connect(h: *mut nauti_rdma, qpn: u32, psn: u32, gid: *const u8, lid: u16, peer_mtu: c_int,
                          err: *mut c_char, errlen: usize) -> c_int;
    fn nauti_rdma_read(h: *mut nauti_rdma, local: *mut c_void, lkey: u32, remote: u64, rkey: u32, len: u32,
                       timeout_ms: c_int, err: *mut c_char, errlen: usize) -> c_int;
    fn nauti_rdma_close(h: *mut nauti_rdma);
}

pub use crate::data_quic::QpInfo;

/// A registered memory region of this side.
pub struct MemoryRegion {
    mr: *mut c_void,
    pub addr: u64,
    pub len: usize,
    pub lkey: u32,
    pub rkey: u32,
}

impl Drop for MemoryRegion {
    fn drop(&mut self) {
        // SAFETY: the mr came from nauti_rdma_reg and is deregistered once.
        unsafe { nauti_rdma_dereg(self.mr) };
    }
}

/// One reliable-connected queue pair on a RoCE port.
pub struct RdmaPort {
    h: *mut nauti_rdma,
    pub device: String,
    pub port: i32,
    pub gid_index: i32,
}

// The handle is used from one thread at a time by the owner; libibverbs objects may move between threads.
unsafe impl Send for RdmaPort {}
unsafe impl Send for MemoryRegion {}

fn errbuf() -> [c_char; 256] {
    [0; 256]
}

fn errstr(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

impl RdmaPort {
    /// Opens `device` (as /sys/class/infiniband names it), `port`, with the RoCE v2 GID at `gid_index`.
    pub fn open(device: &str, port: i32, gid_index: i32) -> io::Result<Self> {
        let dev = CString::new(device).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let mut err = errbuf();
        // SAFETY: valid C string and buffer; the shim writes within errlen.
        let h = unsafe { nauti_rdma_open(dev.as_ptr(), port, gid_index, err.as_mut_ptr(), err.len()) };
        if h.is_null() {
            return Err(io::Error::other(format!("{device}: {}", errstr(&err))));
        }
        Ok(Self { h, device: device.into(), port, gid_index })
    }

    /// Registers `len` bytes at `addr`.  `remote_read`: for a peer to read (a read-only mapping takes only this);
    /// otherwise this side's own buffer, written by its reads.  The caller keeps the memory alive and in place for
    /// the region's life.
    pub fn register(&self, addr: *const u8, len: usize, remote_read: bool) -> io::Result<MemoryRegion> {
        let (mut lkey, mut rkey) = (0u32, 0u32);
        let mut err = errbuf();
        // SAFETY: the caller vouches for [addr, addr + len); the shim only registers it.
        let mr = unsafe {
            nauti_rdma_reg(self.h, addr as *mut c_void, len, remote_read as c_int, &mut lkey, &mut rkey,
                           err.as_mut_ptr(), err.len())
        };
        if mr.is_null() {
            return Err(io::Error::other(format!("register {len} bytes: {}", errstr(&err))));
        }
        Ok(MemoryRegion { mr, addr: addr as u64, len, lkey, rkey })
    }

    pub fn local_info(&self) -> QpInfo {
        let (mut qpn, mut psn, mut lid, mut mtu) = (0u32, 0u32, 0u16, 0 as c_int);
        let mut gid = [0u8; 16];
        // SAFETY: valid out-pointers.
        unsafe { nauti_rdma_local(self.h, &mut qpn, &mut psn, gid.as_mut_ptr(), &mut lid, &mut mtu) };
        QpInfo { qpn, psn, gid: gid.iter().map(|b| format!("{b:02x}")).collect(), lid, mtu }
    }

    /// Connects to the peer's queue pair; both sides call this with the other's numbers.
    pub fn connect(&self, peer: &QpInfo) -> io::Result<()> {
        let gid = peer.gid_bytes()?;
        let mut err = errbuf();
        // SAFETY: valid pointers; the shim reads 16 GID bytes.
        let rc = unsafe {
            nauti_rdma_connect(self.h, peer.qpn, peer.psn, gid.as_ptr(), peer.lid, peer.mtu, err.as_mut_ptr(), err.len())
        };
        if rc != 0 {
            return Err(io::Error::other(format!("connect to qp {}: {}", peer.qpn, errstr(&err))));
        }
        Ok(())
    }

    /// Reads `len` bytes of the peer's memory at `remote_addr` (its region's address space) into `local` (within
    /// a region of this side), and waits for it.
    pub fn read(&self, local: &MemoryRegion, local_offset: usize, remote_addr: u64, rkey: u32, len: usize,
                timeout_ms: i32) -> io::Result<()> {
        if local_offset + len > local.len {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the read runs past the local region"));
        }
        if len > u32::MAX as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "one RDMA READ is at most 4 GiB"));
        }
        let mut err = errbuf();
        // SAFETY: the local range is within a registered region of this side.
        let rc = unsafe {
            nauti_rdma_read(self.h, (local.addr as usize + local_offset) as *mut c_void, local.lkey, remote_addr, rkey,
                            len as u32, timeout_ms, err.as_mut_ptr(), err.len())
        };
        if rc != 0 {
            return Err(io::Error::other(errstr(&err)));
        }
        Ok(())
    }
}

impl Drop for RdmaPort {
    fn drop(&mut self) {
        // SAFETY: the handle came from nauti_rdma_open and is closed once.  Regions must be dropped first by the
        // owner (they borrow nothing, so the order is the owner's duty).
        unsafe { nauti_rdma_close(self.h) };
    }
}

/// "device:port:gid_index" as the CLI gives it, e.g. "rocep1s0f1:1:5".
pub fn parse_rdma_spec(s: &str) -> io::Result<(String, i32, i32)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "an RDMA spec is device:port:gid_index"));
    }
    let port = parts[1].parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "port is a number"))?;
    let gid = parts[2].parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "gid_index is a number"))?;
    Ok((parts[0].to_string(), port, gid))
}

/// A pinned, registered buffer the bench reads into: `chunk` bytes.
pub struct ReadBuffer {
    pub bytes: Vec<u8>,
    pub region: MemoryRegion,
}

impl ReadBuffer {
    pub fn new(port: &RdmaPort, len: usize) -> io::Result<Self> {
        let bytes = vec![0u8; len];
        let region = port.register(bytes.as_ptr(), len, false)?;
        Ok(Self { bytes, region })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse() {
        assert_eq!(parse_rdma_spec("rocep1s0f1:1:5").unwrap(), ("rocep1s0f1".to_string(), 1, 5));
        assert!(parse_rdma_spec("rocep1s0f1").is_err());
    }
}
