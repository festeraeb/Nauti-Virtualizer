//! Data paths: bulk bytes between nodes, kept off the control link.
//!
//! TRANSPORT_ANALYSIS.md: control messages ride Iroh/QUIC; "bulk data paths remain resource-specific: remote GPU,
//! storage, and memory should not be forced through one generic byte transport".  This module is the storage half
//! of that: an engine on one node reads a model's bytes that live on another (c4 reads Le Chonk's 233 GB from
//! t440's SSDs), and the adapter here turns a leased store into a mapped file the engine reads directly, through
//! the NFS client and its page cache, with no copy through the fabric's own sockets.
//!
//! * [`MappedFile`]: a read-only mapping of a file, with the calls a reader of missed experts needs (read a range,
//!   ask the kernel to read ahead, forget what this host cached so a measurement is cold again).
//! * [`NfsMmapAdapter`]: the [`ResourceAdapter`] that attaches a `store.nfs_export` to the mount this host has of
//!   it and maps the file the lease names.  Health is never invented: an export that is not mounted here fails the
//!   attach, and the attachment carries the mount's options and a measured first-page read.
//! * [`ChunkSource`] and [`bench`]: one measurement over every data path (this mapping, the QUIC and TCP data
//!   protocol in `data_quic`, RDMA reads in `rdma`), so the paths are compared on the same bytes with the same
//!   access pattern: scattered chunks the size of a routed expert, as an engine's misses are.
use crate::{AdapterReport, Attachment, FabricError, Lease, Resource, ResourceAdapter, ResourceKind};

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// A read-only mapping of a whole file.
pub struct MappedFile {
    ptr: *const u8,
    len: usize,
    file: File,
    path: PathBuf,
}

// The mapping is read-only and lives as long as the struct: sharing it between threads is sound.
unsafe impl Send for MappedFile {}
unsafe impl Sync for MappedFile {}

impl MappedFile {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let len = file.metadata()?.len() as usize;
        let ptr = if len == 0 {
            std::ptr::null()
        } else {
            // SAFETY: a fresh read-only shared mapping of an open file; the result is checked below.
            let p = unsafe {
                libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, file.as_raw_fd(), 0)
            };
            if p == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            p as *const u8
        };
        Ok(Self { ptr, len, file, path })
    }

    pub fn len(&self) -> u64 {
        self.len as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The mapping's address of byte `offset`: what an RDMA peer reads from once the range is registered.
    pub fn addr_of(&self, offset: u64) -> io::Result<*const u8> {
        if offset > self.len as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "offset past the end of the file"));
        }
        // SAFETY: offset is within the mapping (or one past its end).
        Ok(unsafe { self.ptr.add(offset as usize) })
    }

    fn check(&self, offset: u64, len: u64) -> io::Result<()> {
        if offset.checked_add(len).is_none_or(|end| end > self.len as u64) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} bytes at {offset} run past the end of {} ({} bytes)", len, self.path.display(), self.len),
            ));
        }
        Ok(())
    }

    /// Copies `dst.len()` bytes from `offset`.  A page this host has not cached is read from the file, which on an
    /// NFS mount is a request to the server.
    pub fn read_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        self.check(offset, dst.len() as u64)?;
        // SAFETY: the range is within the mapping (check) and dst is a distinct, writable slice.
        unsafe { std::ptr::copy_nonoverlapping(self.ptr.add(offset as usize), dst.as_mut_ptr(), dst.len()) };
        Ok(())
    }

    fn page_range(&self, offset: u64, len: u64) -> (usize, usize) {
        let page = page_size();
        let start = (offset as usize) / page * page;
        let end = ((offset + len) as usize).div_ceil(page) * page;
        (start, end.min(self.len) - start)
    }

    /// Tells the kernel the range will be read (madvise WILLNEED), so the read-ahead starts now.
    pub fn will_need(&self, offset: u64, len: u64) {
        if self.check(offset, len).is_err() || len == 0 {
            return;
        }
        let (start, bytes) = self.page_range(offset, len);
        // SAFETY: a page-aligned range within the mapping; the advice cannot corrupt memory.
        unsafe { libc::madvise(self.ptr.add(start) as *mut libc::c_void, bytes, libc::MADV_WILLNEED) };
    }

    /// Drops what this host caches of the range (the mapping's pages and the page cache behind them), so the next
    /// read goes to the file again: on an NFS mount, to the server.  The server's own cache is untouched.
    pub fn forget(&self, offset: u64, len: u64) -> io::Result<()> {
        self.check(offset, len)?;
        if len == 0 {
            return Ok(());
        }
        let (start, bytes) = self.page_range(offset, len);
        // SAFETY: a page-aligned range within a read-only shared mapping: DONTNEED only unmaps its pages.
        unsafe { libc::madvise(self.ptr.add(start) as *mut libc::c_void, bytes, libc::MADV_DONTNEED) };
        // SAFETY: an open fd and a range within the file.
        let rc = unsafe {
            libc::posix_fadvise(self.file.as_raw_fd(), start as libc::off_t, bytes as libc::off_t, libc::POSIX_FADV_DONTNEED)
        };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        Ok(())
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: the mapping made in open(), unmapped once.
            unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
        }
    }
}

fn page_size() -> usize {
    // SAFETY: a query with no arguments.
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n > 0 { n as usize } else { 4096 }
}

/// One NFS mount of this host, as /proc/self/mounts lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfsMount {
    pub source: String,
    pub target: PathBuf,
    pub fstype: String,
    pub options: String,
}

impl NfsMount {
    /// The options a data path's speed depends on: the protocol version, transport, read size, connections.
    pub fn data_options(&self) -> String {
        self.options
            .split(',')
            .filter(|o| {
                ["vers=", "proto=", "rsize=", "nconnect=", "hard", "soft", "ro", "rw", "addr="]
                    .iter()
                    .any(|k| o.starts_with(k) && (k.ends_with('=') || *o == *k))
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// /proc/self/mounts unescapes "\040" and friends in paths.
fn unescape_mount_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v as char);
                i += 4;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// Parses mounts in /proc/self/mounts form; only the NFS ones.
pub fn parse_nfs_mounts(text: &str) -> Vec<NfsMount> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 || !f[2].starts_with("nfs") {
                return None;
            }
            Some(NfsMount {
                source: unescape_mount_field(f[0]),
                target: PathBuf::from(unescape_mount_field(f[1])),
                fstype: f[2].to_string(),
                options: f[3].to_string(),
            })
        })
        .collect()
}

/// This host's NFS mounts.
pub fn nfs_mounts() -> Vec<NfsMount> {
    std::fs::read_to_string("/proc/self/mounts").map(|t| parse_nfs_mounts(&t)).unwrap_or_default()
}

/// The mount of `export` ("host:/path"), if this host has one.  A trailing slash on either side does not matter.
pub fn find_nfs_mount<'a>(mounts: &'a [NfsMount], export: &str) -> Option<&'a NfsMount> {
    let want = export.trim_end_matches('/');
    mounts.iter().find(|m| m.source.trim_end_matches('/') == want)
}

/// Attaches a leased NFS store as a mapped file.
///
/// The resource is a [`ResourceKind::Storage`] with `store.nfs_export` ("host:/path") and, for a file to map,
/// `data.file` (relative to the export).  The attachment says where the file is on this host, through which
/// mount, with what options, and how long the first page took to read.  Nothing is mounted by the adapter: the
/// fleet's mounts are the operator's (fstab), and an export this host does not have is reported, not invented.
#[derive(Default)]
pub struct NfsMmapAdapter {
    mounts_file: Option<PathBuf>,   // tests: a file in /proc/self/mounts form
}

impl NfsMmapAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mounts_file(path: impl Into<PathBuf>) -> Self {
        Self { mounts_file: Some(path.into()) }
    }

    fn mounts(&self) -> Vec<NfsMount> {
        match &self.mounts_file {
            Some(p) => std::fs::read_to_string(p).map(|t| parse_nfs_mounts(&t)).unwrap_or_default(),
            None => nfs_mounts(),
        }
    }
}

pub const NFS_MMAP_ADAPTER: &str = "nfs-mmap";

impl ResourceAdapter for NfsMmapAdapter {
    fn name(&self) -> &str {
        NFS_MMAP_ADAPTER
    }

    fn attach(&self, resource: &Resource, lease: &Lease) -> Result<Attachment, FabricError> {
        if resource.kind != ResourceKind::Storage {
            return Err(FabricError::IncompatibleResourceKind {
                adapter: NFS_MMAP_ADAPTER.into(),
                expected: ResourceKind::Storage,
                actual: resource.kind,
            });
        }
        let export = resource
            .attributes
            .get("store.nfs_export")
            .cloned()
            .ok_or_else(|| FabricError::MissingResourceAttribute("store.nfs_export".into()))?;
        let mounts = self.mounts();
        let mount = find_nfs_mount(&mounts, &export).ok_or_else(|| FabricError::AdapterBackendUnavailable {
            adapter: NFS_MMAP_ADAPTER.into(),
            reason: format!("{export} is not mounted on this host ({} NFS mounts seen)", mounts.len()),
        })?;
        let mut details = BTreeMap::from([
            ("data.kind".to_string(), "nfs-mmap".to_string()),
            ("data.mount".into(), mount.target.display().to_string()),
            ("data.mount_source".into(), mount.source.clone()),
            ("data.mount_fstype".into(), mount.fstype.clone()),
            ("data.mount_options".into(), mount.data_options()),
        ]);
        if let Some(file) = resource.attributes.get("data.file") {
            let local = mount.target.join(file.trim_start_matches('/'));
            let map = MappedFile::open(&local).map_err(|e| FabricError::AdapterBackendUnavailable {
                adapter: NFS_MMAP_ADAPTER.into(),
                reason: format!("{}: {e}", local.display()),
            })?;
            let mut page = [0u8; 4096];
            let probe = if map.is_empty() {
                0
            } else {
                let n = page.len().min(map.len() as usize);
                let t0 = Instant::now();
                map.read_at(0, &mut page[..n]).map_err(|e| FabricError::AdapterBackendUnavailable {
                    adapter: NFS_MMAP_ADAPTER.into(),
                    reason: format!("{}: first page: {e}", local.display()),
                })?;
                t0.elapsed().as_micros()
            };
            details.insert("data.local_path".into(), local.display().to_string());
            details.insert("data.bytes".into(), map.len().to_string());
            details.insert("data.map".into(), "mmap,read-only,shared".into());
            details.insert("probe.first_page_us".into(), probe.to_string());
        }
        Ok(Attachment {
            resource_id: resource.id.clone(),
            lease_id: lease.id,
            adapter: NFS_MMAP_ADAPTER.into(),
            details,
        })
    }

    fn capability_report(&self) -> AdapterReport {
        let mounts = self.mounts();
        let mut detail = BTreeMap::new();
        for (i, m) in mounts.iter().enumerate() {
            detail.insert(format!("mount.{i}"), format!("{} -> {} ({})", m.source, m.target.display(), m.data_options()));
        }
        AdapterReport {
            name: NFS_MMAP_ADAPTER.into(),
            scope: "a leased NFS export's file, mapped read-only; the engine reads its misses through the NFS client"
                .into(),
            healthy: !mounts.is_empty(),
            detail,
        }
    }
}

/// A data path under measurement: chunks of one file, by offset.
pub trait ChunkSource {
    /// What the path is, for the table: transport, options, the route it took.
    fn describe(&self) -> String;
    /// `dst.len()` bytes from `offset` of the file.
    fn fetch(&mut self, offset: u64, dst: &mut [u8]) -> io::Result<()>;
    /// Drops what this host caches of the range, so the next fetch reads it over the path again.  Paths that
    /// cache nothing here need do nothing.
    fn forget(&mut self, _offset: u64, _len: u64) -> io::Result<()> {
        Ok(())
    }
}

impl ChunkSource for MappedFile {
    fn describe(&self) -> String {
        let mounts = nfs_mounts();
        let mount = mounts
            .iter()
            .filter(|m| self.path.starts_with(&m.target))
            .max_by_key(|m| m.target.as_os_str().len());
        match mount {
            Some(m) => format!("nfs-mmap {} ({})", m.source, m.data_options()),
            None => "mmap (local file)".to_string(),
        }
    }

    fn fetch(&mut self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        self.read_at(offset, dst)
    }

    fn forget(&mut self, offset: u64, len: u64) -> io::Result<()> {
        MappedFile::forget(self, offset, len)
    }
}

/// One path's measurement.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BenchResult {
    pub path: String,
    pub chunk: usize,
    pub count: usize,
    pub cold: bool,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub mean_us: f64,
    pub min_us: f64,
    pub max_us: f64,
    pub mb_s: f64,
    pub total_s: f64,
    /// FNV-1a of every chunk's bytes, in order: the same on every path, or the paths did not read the same bytes.
    pub checksum: u64,
}

/// `count` distinct chunk-aligned offsets within [window_offset, window_offset + window_len), in a scattered order
/// that `seed` fixes, so every path reads the same chunks in the same order.
pub fn scattered_offsets(window_offset: u64, window_len: u64, chunk: u64, count: usize, seed: u64) -> Vec<u64> {
    let slots = (window_len / chunk.max(1)).max(1);
    let mut order: Vec<u64> = (0..slots).collect();
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    for i in (1..order.len()).rev() {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = (x >> 33) as usize % (i + 1);
        order.swap(i, j);
    }
    order.into_iter().cycle().take(count).map(|s| window_offset + s * chunk).collect()
}

fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Reads every offset's chunk through `src`, timing each fetch alone (the checksum is taken outside the timing).
/// With `cold`, the host forgets each chunk right before fetching it.
pub fn bench(src: &mut dyn ChunkSource, offsets: &[u64], chunk: usize, cold: bool) -> io::Result<BenchResult> {
    let mut buf = vec![0u8; chunk];
    let mut samples = Vec::with_capacity(offsets.len());
    let mut checksum = 0xcbf29ce484222325u64;
    let t_all = Instant::now();
    for &off in offsets {
        if cold {
            src.forget(off, chunk as u64)?;
        }
        let t0 = Instant::now();
        src.fetch(off, &mut buf)?;
        samples.push(t0.elapsed().as_secs_f64() * 1e6);
        checksum = fnv1a(checksum, &buf);
    }
    let total_s = t_all.elapsed().as_secs_f64();
    let mut sorted = samples.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let sum_us: f64 = samples.iter().sum();
    Ok(BenchResult {
        path: src.describe(),
        chunk,
        count: offsets.len(),
        cold,
        p50_us: percentile(&sorted, 0.5),
        p95_us: percentile(&sorted, 0.95),
        p99_us: percentile(&sorted, 0.99),
        mean_us: if samples.is_empty() { 0.0 } else { sum_us / samples.len() as f64 },
        min_us: sorted.first().copied().unwrap_or(0.0),
        max_us: sorted.last().copied().unwrap_or(0.0),
        mb_s: if sum_us > 0.0 { (chunk as f64 * offsets.len() as f64) / (sum_us / 1e6) / 1e6 } else { 0.0 },
        total_s,
        checksum,
    })
}

/// A size as "24K", "23M", "1G" or plain bytes.
pub fn parse_size(s: &str) -> Option<usize> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1usize << 10),
        'm' | 'M' => (&s[..s.len() - 1], 1 << 20),
        'g' | 'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    let n: f64 = num.trim().parse().ok()?;
    Some((n * mult as f64) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceState;
    use std::io::Write;

    const MOUNTS: &str = "\
sysfs /sys sysfs rw,nosuid 0 0
10.0.10.61:/mnt/ssd9/models /mnt/t440/ssd9-models nfs ro,noatime,vers=3,rsize=1048576,wsize=1048576,hard,proto=tcp,nconnect=8,addr=10.0.10.61 0 0
10.0.10.61:/mnt/raid0 /mnt/t440/raid0 nfs rw,relatime,vers=3,rsize=1048576,proto=tcp,nconnect=8 0 0
t440:/data /mnt/with\\040space nfs4 rw,vers=4.2 0 0
";

    fn storage(attrs: &[(&str, &str)]) -> Resource {
        Resource {
            id: "store.nfs.ssd9".into(),
            kind: ResourceKind::Storage,
            capacity: 1,
            unit: "export".into(),
            node: "t440".into(),
            state: ResourceState::Available,
            exclusive: false,
            attributes: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    fn lease() -> Lease {
        Lease { id: 7, resource_id: "store.nfs.ssd9".into(), owner: "test".into() }
    }

    #[test]
    fn mounts_are_parsed_with_their_data_options() {
        let m = parse_nfs_mounts(MOUNTS);
        assert_eq!(m.len(), 3, "only the NFS lines");
        assert_eq!(m[0].source, "10.0.10.61:/mnt/ssd9/models");
        assert_eq!(m[0].data_options(), "ro,vers=3,rsize=1048576,hard,proto=tcp,nconnect=8,addr=10.0.10.61");
        assert_eq!(m[2].target, PathBuf::from("/mnt/with space"), "octal escapes are undone");
        assert!(find_nfs_mount(&m, "10.0.10.61:/mnt/raid0/").is_some(), "a trailing slash does not matter");
        assert!(find_nfs_mount(&m, "10.0.10.61:/mnt/ssd8").is_none());
    }

    #[test]
    fn an_unmounted_export_is_reported_not_invented() {
        let dir = tempfile::tempdir().unwrap();
        let mounts = dir.path().join("mounts");
        std::fs::write(&mounts, MOUNTS).unwrap();
        let a = NfsMmapAdapter::with_mounts_file(&mounts);
        let err = a.attach(&storage(&[("store.nfs_export", "c3:/nowhere")]), &lease()).unwrap_err();
        assert!(matches!(err, FabricError::AdapterBackendUnavailable { .. }), "{err}");
        let err = a.attach(&storage(&[]), &lease()).unwrap_err();
        assert!(matches!(err, FabricError::MissingResourceAttribute(ref k) if k == "store.nfs_export"));
        let mut gpu = storage(&[("store.nfs_export", "10.0.10.61:/mnt/raid0")]);
        gpu.kind = ResourceKind::Gpu;
        assert!(matches!(a.attach(&gpu, &lease()).unwrap_err(), FabricError::IncompatibleResourceKind { .. }));
    }

    #[test]
    fn a_mounted_export_attaches_with_its_options_and_maps_the_file() {
        // the "mount" is a temp dir standing in for the mount target
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mnt");
        std::fs::create_dir(&target).unwrap();
        let mut f = std::fs::File::create(target.join("model.gguf")).unwrap();
        f.write_all(&[7u8; 10000]).unwrap();
        let mounts = dir.path().join("mounts");
        std::fs::write(
            &mounts,
            format!("t440:/mnt/ssd9/models {} nfs ro,vers=3,rsize=1048576,nconnect=8,proto=tcp 0 0\n", target.display()),
        )
        .unwrap();
        let a = NfsMmapAdapter::with_mounts_file(&mounts);
        let at = a
            .attach(&storage(&[("store.nfs_export", "t440:/mnt/ssd9/models"), ("data.file", "model.gguf")]), &lease())
            .unwrap();
        assert_eq!(at.adapter, "nfs-mmap");
        assert_eq!(at.details["data.bytes"], "10000");
        assert_eq!(at.details["data.mount_options"], "ro,vers=3,rsize=1048576,nconnect=8,proto=tcp");
        assert_eq!(at.details["data.local_path"], target.join("model.gguf").display().to_string());
        assert!(at.details.contains_key("probe.first_page_us"));
        // the mount is there but the file is not: the attach says so
        let err = a
            .attach(&storage(&[("store.nfs_export", "t440:/mnt/ssd9/models"), ("data.file", "missing.gguf")]), &lease())
            .unwrap_err();
        assert!(matches!(err, FabricError::AdapterBackendUnavailable { .. }));
        let report = a.capability_report();
        assert!(report.healthy);
        assert_eq!(report.detail.len(), 1);
    }

    #[test]
    fn a_mapped_file_reads_ranges_and_refuses_past_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let bytes: Vec<u8> = (0..20000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &bytes).unwrap();
        let m = MappedFile::open(&p).unwrap();
        assert_eq!(m.len(), 20000);
        let mut dst = [0u8; 300];
        m.read_at(5000, &mut dst).unwrap();
        assert_eq!(&dst[..], &bytes[5000..5300]);
        assert!(m.read_at(19900, &mut dst).is_err(), "300 bytes at 19900 run past the end");
        m.will_need(0, 20000);
        m.forget(4096, 8192).unwrap();
        m.read_at(5000, &mut dst).unwrap();
        assert_eq!(&dst[..], &bytes[5000..5300], "the same bytes after a forget");
        assert_eq!(m.addr_of(100).unwrap() as usize - m.addr_of(0).unwrap() as usize, 100);
    }

    #[test]
    fn the_bench_reads_the_same_chunks_in_the_same_order_on_every_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let bytes: Vec<u8> = (0..(64 * 1024u32)).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        std::fs::write(&p, &bytes).unwrap();
        let offsets = scattered_offsets(4096, 60 * 1024, 1024, 50, 7);
        assert_eq!(offsets.len(), 50);
        assert!(offsets.iter().all(|o| *o >= 4096 && o + 1024 <= 64 * 1024 && o % 1024 == 0));
        assert_ne!(offsets[0..5], offsets[5..10], "scattered, not sequential");
        assert_eq!(offsets, scattered_offsets(4096, 60 * 1024, 1024, 50, 7), "the seed fixes the order");
        let mut a = MappedFile::open(&p).unwrap();
        let mut b = MappedFile::open(&p).unwrap();
        let ra = bench(&mut a, &offsets, 1024, false).unwrap();
        let rb = bench(&mut b, &offsets, 1024, true).unwrap();
        assert_eq!(ra.checksum, rb.checksum, "the same bytes whatever the path or the cache");
        assert_eq!(ra.count, 50);
        assert!(ra.p50_us <= ra.p99_us && ra.min_us <= ra.p50_us && ra.p99_us <= ra.max_us);
        assert!(ra.mb_s > 0.0);
        assert!(rb.cold);
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("24K"), Some(24 * 1024));
        assert_eq!(parse_size("23M"), Some(23 << 20));
        assert_eq!(parse_size("1.5G"), Some(3 << 29));
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("x"), None);
    }
}
