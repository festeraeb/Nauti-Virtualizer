//! virtio-rng vhost-user backend.
//!
//! Adapted from the rust-vmm `vhost-device` RNG template
//! (`vhost-device-rng`, SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause,
//! original copyright Linaro Ltd / Mathieu Poirier). This port keeps the
//! proven core — the rate-limiting timer, the single-queue descriptor loop,
//! the `VhostUserBackendMut` contract — and re-points the entropy source at
//! any `ReadVolatile` object so the fabric can back it with a local file or
//! with a FIFO fed by the remote entropy stub (see `super::entropy`).
//!
//! FIFO note: when the source is a named pipe it is opened read-write. That
//! makes the open succeed before the pump attaches, and guarantees the guest
//! never sees a zero-byte "entropy" completion: if the pump dies, reads
//! stall (honest degradation) instead of returning EOF-length payloads.

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    result,
    sync::{Arc, Mutex},
    thread::sleep,
    time::{Duration, Instant},
};

use thiserror::Error as ThisError;
use tracing::warn;
use vhost::vhost_user::message::{VhostUserProtocolFeatures, VhostUserVirtioFeatures};
use vhost_user_backend::{VhostUserBackendMut, VhostUserDaemon, VringRwLock, VringT};
use virtio_bindings::bindings::{
    virtio_config::{VIRTIO_F_NOTIFY_ON_EMPTY, VIRTIO_F_VERSION_1},
    virtio_ring::{VIRTIO_RING_F_EVENT_IDX, VIRTIO_RING_F_INDIRECT_DESC},
};
use virtio_queue::{DescriptorChain, QueueOwnedT};
use vm_memory::{
    Bytes, GuestAddressSpace, GuestMemoryAtomic, GuestMemoryLoadGuard, GuestMemoryMmap,
    ReadVolatile,
};
use vmm_sys_util::{
    epoll::EventSet,
    event::{new_event_consumer_and_notifier, EventConsumer, EventFlag, EventNotifier},
};

const QUEUE_SIZE: usize = 1024;
const NUM_QUEUES: usize = 1;

/// Matches the max period in QEMU's vhost-user-rng and virtio-rng implementations.
pub const MAX_PERIOD_MS: u128 = 65536;

type Result<T> = std::result::Result<T, RngError>;
type RngDescriptorChain = DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap<()>>>;

#[derive(Debug, Eq, PartialEq, ThisError)]
pub enum RngError {
    #[error("descriptor not found")]
    DescriptorNotFound,
    #[error("notification send failed")]
    SendNotificationFailed,
    #[error("can't create eventFd")]
    EventFdError,
    #[error("failed to handle event: not EPOLLIN")]
    HandleEventNotEpollIn,
    #[error("unknown device event")]
    HandleEventUnknownEvent,
    #[error("too many descriptors: {0}")]
    UnexpectedDescriptorCount(usize),
    #[error("unexpected read descriptor")]
    UnexpectedReadDescriptor,
    #[error("failed to access RNG source")]
    SourceAccess,
    #[error("failed to read from the RNG source")]
    SourceRead,
    #[error("entropy source cannot be opened: {0}")]
    SourceOpen(String),
    #[error("daemon failed: {0}")]
    Daemon(String),
    #[error("previous time value is later than current time")]
    UnexpectedTimerValue,
}

impl From<RngError> for io::Error {
    fn from(e: RngError) -> Self {
        io::Error::other(e)
    }
}

/// Configuration for one virtio-rng backend instance (single socket).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RngConfig {
    /// Unix socket the daemon listens on; Cloud Hypervisor connects here.
    pub socket_path: PathBuf,
    /// Entropy source: `/dev/urandom` locally, or a FIFO fed by
    /// `entropy_pump` for the remote-stub variant.
    pub source_path: PathBuf,
    /// Rate-limit window length (ms).
    pub period_ms: u128,
    /// Max bytes served per period (usize::MAX = unlimited).
    pub max_bytes: usize,
}

impl RngConfig {
    pub fn validate(&self) -> Result<()> {
        if self.period_ms == 0 || self.period_ms > MAX_PERIOD_MS {
            return Err(RngError::SourceOpen(format!(
                "period {}ms outside 1..={MAX_PERIOD_MS}",
                self.period_ms
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TimerConfig {
    period_ms: u128,
    period_start: Instant,
    max_bytes: usize,
    quota_remaining: usize,
}

impl TimerConfig {
    fn new(period_ms: u128, max_bytes: usize) -> Self {
        TimerConfig {
            period_ms,
            period_start: Instant::now(),
            max_bytes,
            quota_remaining: max_bytes,
        }
    }
}

/// The virtio-rng device model. Generic over the entropy source so tests can
/// substitute a deterministic mock and the remote stub can ride in via FIFO.
pub struct RngBackend<T: ReadVolatile> {
    event_idx: bool,
    timer: TimerConfig,
    source: Arc<Mutex<T>>,
    exit_consumer: EventConsumer,
    exit_notifier: EventNotifier,
    mem: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
}

impl<T: ReadVolatile> RngBackend<T> {
    pub fn new(source: Arc<Mutex<T>>, period_ms: u128, max_bytes: usize) -> io::Result<Self> {
        let (exit_consumer, exit_notifier) = new_event_consumer_and_notifier(EventFlag::NONBLOCK)
            .map_err(|_| RngError::EventFdError)?;
        Ok(RngBackend {
            event_idx: false,
            source,
            timer: TimerConfig::new(period_ms, max_bytes),
            exit_consumer,
            exit_notifier,
            mem: None,
        })
    }

    pub fn process_requests(
        &mut self,
        requests: Vec<RngDescriptorChain>,
        vring: &VringRwLock,
    ) -> Result<bool> {
        if requests.is_empty() {
            return Ok(true);
        }

        for desc_chain in requests {
            let descriptors: Vec<_> = desc_chain.clone().collect();

            if descriptors.len() != 1 {
                return Err(RngError::UnexpectedDescriptorCount(descriptors.len()));
            }

            let descriptor = descriptors[0];
            let mut to_read = descriptor.len() as usize;
            let timer = &mut self.timer;

            if !descriptor.is_write_only() {
                return Err(RngError::UnexpectedReadDescriptor);
            }

            let now = Instant::now();
            match now.checked_duration_since(timer.period_start) {
                Some(duration) => {
                    let elapsed = duration.as_millis();
                    if elapsed >= timer.period_ms {
                        timer.period_start = now;
                        timer.quota_remaining = timer.max_bytes;
                    } else if timer.quota_remaining == 0 {
                        let to_sleep = timer.period_ms - elapsed;
                        sleep(Duration::from_millis(to_sleep as u64));
                        timer.period_start = Instant::now();
                        timer.quota_remaining = timer.max_bytes;
                    }
                }
                None => return Err(RngError::UnexpectedTimerValue),
            }

            if timer.quota_remaining < to_read {
                to_read = timer.quota_remaining;
            }

            let mut source = self.source.lock().map_err(|_| RngError::SourceAccess)?;

            let len = desc_chain
                .memory()
                .read_volatile_from(descriptor.addr(), &mut *source, to_read)
                .map_err(|_| RngError::SourceRead)?;

            timer.quota_remaining -= len;

            if vring.add_used(desc_chain.head_index(), len as u32).is_err() {
                warn!("couldn't return used descriptors to the ring");
            }
        }
        Ok(true)
    }

    fn process_queue(&mut self, vring: &VringRwLock) -> Result<()> {
        let requests: Vec<_> = vring
            .get_mut()
            .get_queue_mut()
            .iter(self.mem.as_ref().unwrap().memory())
            .map_err(|_| RngError::DescriptorNotFound)?
            .collect();

        if self.process_requests(requests, vring)? {
            vring
                .signal_used_queue()
                .map_err(|_| RngError::SendNotificationFailed)?;
        }

        Ok(())
    }
}


impl<T: 'static + ReadVolatile + Sync + Send> VhostUserBackendMut for RngBackend<T> {
    type Vring = VringRwLock;
    type Bitmap = ();

    fn num_queues(&self) -> usize {
        NUM_QUEUES
    }

    fn max_queue_size(&self) -> usize {
        QUEUE_SIZE
    }

    fn features(&self) -> u64 {
        // Matches the libvhost defaults except VHOST_F_LOG_ALL.
        (1 << VIRTIO_F_VERSION_1)
            | (1 << VIRTIO_F_NOTIFY_ON_EMPTY)
            | (1 << VIRTIO_RING_F_INDIRECT_DESC)
            | (1 << VIRTIO_RING_F_EVENT_IDX)
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }

    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        VhostUserProtocolFeatures::MQ
    }

    fn set_event_idx(&mut self, enabled: bool) {
        self.event_idx = enabled;
    }

    fn update_memory(
        &mut self,
        mem: GuestMemoryAtomic<GuestMemoryMmap>,
    ) -> result::Result<(), io::Error> {
        self.mem = Some(mem);
        Ok(())
    }

    fn handle_event(
        &mut self,
        device_event: u16,
        evset: EventSet,
        vrings: &[VringRwLock],
        _thread_id: usize,
    ) -> result::Result<(), io::Error> {
        if evset != EventSet::IN {
            return Err(RngError::HandleEventNotEpollIn.into());
        }

        match device_event {
            0 => {
                let vring = &vrings[0];
                if self.event_idx {
                    // vm-virtio's Queue implementation only checks avail_index
                    // once, so to properly support EVENT_IDX we need to keep
                    // calling process_queue() until it stops finding new
                    // requests on the queue.
                    loop {
                        vring.disable_notification().unwrap();
                        self.process_queue(vring)?;
                        if !vring.enable_notification().unwrap() {
                            break;
                        }
                    }
                } else {
                    // Without EVENT_IDX, a single call is enough.
                    self.process_queue(vring)?;
                }
            }
            _ => {
                warn!("unhandled device_event: {device_event}");
                return Err(RngError::HandleEventUnknownEvent.into());
            }
        }
        Ok(())
    }

    fn exit_event(&self, _thread_index: usize) -> Option<(EventConsumer, EventNotifier)> {
        let consumer = self.exit_consumer.try_clone().ok()?;
        let notifier = self.exit_notifier.try_clone().ok()?;
        Some((consumer, notifier))
    }
}

/// Open the entropy source. FIFOs are opened read-write so the backend
/// starts before the pump attaches and never sees a spurious EOF.
fn open_source(path: &Path) -> Result<File> {
    let is_fifo = path
        .symlink_metadata()
        .map(|m| {
            use std::os::unix::fs::FileTypeExt;
            m.file_type().is_fifo()
        })
        .unwrap_or(false);

    if is_fifo {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| RngError::SourceOpen(format!("{}: {e}", path.display())))
    } else {
        File::open(path).map_err(|e| RngError::SourceOpen(format!("{}: {e}", path.display())))
    }
}

/// Serve one virtio-rng backend on `config.socket_path`, blocking forever.
/// Cloud Hypervisor connects as the vhost-user frontend; when the frontend
/// disconnects the daemon exits the serve loop and returns an error (CH is
/// expected to hold the connection for the VM's lifetime).
pub fn serve_rng(config: RngConfig) -> Result<()> {
    config.validate()?;
    let file = open_source(&config.source_path)?;
    let source = Arc::new(Mutex::new(file));

    let backend = Arc::new(std::sync::RwLock::new(
        RngBackend::new(source, config.period_ms, config.max_bytes)
            .map_err(|e| RngError::SourceOpen(e.to_string()))?,
    ));

    let mut daemon = VhostUserDaemon::new(
        String::from("nauti-vhost-rng"),
        backend,
        GuestMemoryAtomic::new(GuestMemoryMmap::new()),
    )
    .map_err(|e| RngError::Daemon(e.to_string()))?;

    daemon
        .serve(&config.socket_path)
        .map_err(|e| RngError::Daemon(e.to_string()))
}


#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use virtio_bindings::bindings::virtio_ring::{VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
    use virtio_queue::{
        desc::{split::Descriptor as SplitDescriptor, RawDescriptor},
        mock::MockSplitQueue,
        QueueOwnedT,
    };
    use vm_memory::{Bytes, GuestAddress, GuestMemoryAtomic, GuestMemoryMmap};

    use super::*;

    impl<T: ReadVolatile> RngBackend<T> {
        pub(crate) fn set_quota(&mut self, quota: usize) {
            self.timer.quota_remaining = quota;
        }
    }

    // Deterministic mock entropy source; can be flipped to deny access.
    #[derive(Clone, Debug, PartialEq)]
    struct MockRng {
        permission_denied: bool,
    }

    impl MockRng {
        fn new(permission_denied: bool) -> Self {
            MockRng { permission_denied }
        }
    }

    impl ReadVolatile for MockRng {
        fn read_volatile<B: vm_memory::bitmap::BitmapSlice>(
            &mut self,
            buf: &mut vm_memory::VolatileSlice<B>,
        ) -> result::Result<usize, vm_memory::VolatileMemoryError> {
            match self.permission_denied {
                true => Err(vm_memory::VolatileMemoryError::IOError(
                    std::io::Error::from(ErrorKind::PermissionDenied),
                )),
                false => {
                    buf.write_obj(0xABu8, 0)?;
                    Ok(1)
                }
            }
        }
    }

    fn build_desc_chain(num: u16, flags: u16) -> RngDescriptorChain {
        // Region must cover the descriptor buffers placed at 0x1000..0x1200
        // (SplitDescriptor::new(0x1000, 0x200, ...) below). 0x1000 was too
        // small, so every real read failed with an out-of-bounds guest
        // address (surfacing as RngError::SourceRead) and the quota case in
        // verify_process_requests never actually serviced a request.
        let mem = GuestMemoryAtomic::new(
            GuestMemoryMmap::<()>::from_ranges(&[(GuestAddress(0), 0x8000)]).unwrap(),
        );
        let mem_guard = mem.clone().memory();
        let vq = MockSplitQueue::create(&*mem_guard, GuestAddress(0), 16);
        let mut descriptors: Vec<RawDescriptor> = Vec::new();
        for i in 0..num {
            let mut f = flags;
            if i < num - 1 {
                f |= VRING_DESC_F_NEXT as u16;
            }
            let desc = SplitDescriptor::new(0x1000, 0x200, f, i + 1);
            descriptors.push(RawDescriptor::from(desc));
        }
        vq.build_desc_chain(&descriptors).unwrap();

        let mut q: virtio_queue::Queue = vq.create_queue().unwrap();
        let chain: RngDescriptorChain = q.iter(mem.memory()).unwrap().next().unwrap();
        chain
    }

    #[test]
    fn config_validation() {
        let base = RngConfig {
            socket_path: "/tmp/x.sock".into(),
            source_path: "/dev/urandom".into(),
            period_ms: 1000,
            max_bytes: 512,
        };
        assert!(base.validate().is_ok());

        let mut zero = base.clone();
        zero.period_ms = 0;
        assert!(zero.validate().is_err());

        let mut huge = base;
        huge.period_ms = MAX_PERIOD_MS + 1;
        assert!(huge.validate().is_err());
    }


    #[test]
    fn verify_process_requests() {
        let source = Arc::new(Mutex::new(MockRng::new(false)));
        let mut backend = RngBackend::new(source, 1000, 512).unwrap();

        let mem = GuestMemoryAtomic::new(
            GuestMemoryMmap::<()>::from_ranges(&[(GuestAddress(0), 0x1000)]).unwrap(),
        );
        let vring = VringRwLock::new(mem.clone(), 0x1000).unwrap();

        // Empty descriptor chain should be ignored.
        assert!(backend
            .process_requests(Vec::<RngDescriptorChain>::new(), &vring)
            .unwrap());

        // Quota below descriptor capacity: partial service, no panic.
        backend.set_quota(0x100);
        assert!(backend
            .process_requests(vec![build_desc_chain(1, VRING_DESC_F_WRITE as u16)], &vring)
            .unwrap());

        // A read-only descriptor is a protocol violation.
        backend.set_quota(0x100);
        assert_eq!(
            backend
                .process_requests(vec![build_desc_chain(1, 0)], &vring)
                .unwrap_err(),
            RngError::UnexpectedReadDescriptor
        );

        // Multi-descriptor chains are not used by virtio-rng.
        assert_eq!(
            backend
                .process_requests(
                    vec![build_desc_chain(
                        2,
                        (VRING_DESC_F_NEXT | VRING_DESC_F_WRITE) as u16
                    )],
                    &vring
                )
                .unwrap_err(),
            RngError::UnexpectedDescriptorCount(2)
        );

        // A denied source surfaces as a typed read failure, not a panic.
        let denied = Arc::new(Mutex::new(MockRng::new(true)));
        let mut denied_backend = RngBackend::new(denied, 1000, 512).unwrap();
        denied_backend.set_quota(0x100);
        assert_eq!(
            denied_backend
                .process_requests(vec![build_desc_chain(1, VRING_DESC_F_WRITE as u16)], &vring)
                .unwrap_err(),
            RngError::SourceRead
        );
    }

    #[test]
    fn verify_handle_event() {
        let source = Arc::new(Mutex::new(MockRng::new(false)));
        let mut backend = RngBackend::new(source, 1000, 512).unwrap();

        let mem = GuestMemoryAtomic::new(
            GuestMemoryMmap::<()>::from_ranges(&[(GuestAddress(0), 0x1000)]).unwrap(),
        );
        backend.update_memory(mem.clone()).unwrap();

        let vring = VringRwLock::new(mem, 0x1000).unwrap();
        vring.set_queue_info(0x100, 0x200, 0x300).unwrap();
        vring.set_queue_ready(true);

        // Only EventSet::IN is handled.
        assert_eq!(
            backend
                .handle_event(0, EventSet::OUT, &[vring.clone()], 0)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );

        // Only device event 0 exists.
        assert_eq!(
            backend
                .handle_event(1, EventSet::IN, &[vring.clone()], 0)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );

        backend.handle_event(0, EventSet::IN, &[vring.clone()], 0).unwrap();

        backend.set_event_idx(true);
        backend.handle_event(0, EventSet::IN, &[vring], 0).unwrap();
    }


    #[test]
    fn verify_backend_contract() {
        let source = Arc::new(Mutex::new(MockRng::new(false)));
        let mut backend = RngBackend::new(source, 1000, 512).unwrap();

        let mem = GuestMemoryAtomic::new(
            GuestMemoryMmap::<()>::from_ranges(&[(GuestAddress(0), 0x1000)]).unwrap(),
        );
        let _vring = VringRwLock::new(mem.clone(), 0x1000).unwrap();

        assert_eq!(backend.num_queues(), NUM_QUEUES);
        assert_eq!(backend.max_queue_size(), QUEUE_SIZE);
        assert_eq!(backend.features(), 0x171000000);
        assert_eq!(backend.protocol_features(), VhostUserProtocolFeatures::MQ);
        assert_eq!(backend.queues_per_thread(), vec![0xffff_ffff]);
        let empty_config: Vec<u8> = Vec::new();
        assert_eq!(backend.get_config(0, 0), empty_config);
        backend.update_memory(mem).unwrap();

        backend.set_event_idx(true);
        assert!(backend.event_idx);
    }

    #[test]
    fn fifo_source_opens_read_write() {
        // A FIFO opened through open_source must not block and must be
        // read-write (so the guest never sees EOF-length entropy).
        let dir = std::env::temp_dir().join(format!("nauti-fifo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("entropy.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo available");
        assert!(status.success());

        let f = open_source(&fifo).expect("fifo opens read-write without a writer");
        let md = f.metadata().unwrap();
        use std::os::unix::fs::FileTypeExt;
        assert!(md.file_type().is_fifo());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_source_is_typed_error() {
        let err = open_source(Path::new("/nonexistent/urandom")).unwrap_err();
        assert!(matches!(err, RngError::SourceOpen(_)));
    }
}

