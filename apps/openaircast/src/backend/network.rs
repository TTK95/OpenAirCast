//! Cancellable local network-change monitoring (Task 17, Step 3).
//!
//! Windows reports one physical interface change as a burst of
//! `NotifyIpInterfaceChange` callbacks. Acting on each of them would restart
//! discovery and the AirPlay group several times for a single unplugged cable,
//! so [`NetworkMonitor`] folds one burst into exactly one
//! [`SystemEvent::NetworkChanged`] edge after the mandated two-second
//! interface-settle period.
//!
//! Privacy boundary: this module is the only place in the backend that sees
//! local IP addresses. They never leave it. The monitor compares successive
//! address tables through an opaque [`BindingFingerprint`] and publishes a
//! single boolean -- whether an active local binding moved -- so no address
//! can reach a snapshot, an event, or the diagnostics feed.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::backend::command::SystemEvent;
use crate::backend::controller::BackendTimer;

/// Interface-settle period observed before one coalesced burst is published.
pub const NETWORK_SETTLE: Duration = Duration::from_secs(2);

/// Bounded queue between the OS callback and the coalescing task.
///
/// The callback runs on a Windows thread-pool thread and must never block, so
/// it publishes with `try_send`; a full queue simply means a burst is already
/// pending, which is precisely what the coalescer is about to report.
const PULSE_CAPACITY: usize = 32;

/// Failure of a network-change subscription.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NetworkError {
    /// `NotifyIpInterfaceChange` refused the registration.
    #[error("network change notifications are unavailable (status {0})")]
    Unavailable(u32),
    /// The running platform offers no interface-change notifications.
    #[error("network change notifications are unsupported on this platform")]
    Unsupported,
}

/// Source of local interface-change notifications.
pub trait NetworkChangeSource: Send + Sync {
    /// Registers for interface changes; every callback pushes one pulse.
    fn subscribe(
        &self,
        tx: mpsc::Sender<SystemEvent>,
    ) -> Result<Box<dyn NetworkSubscription>, NetworkError>;
}

/// A live registration whose OS handle is released by [`Self::cancel`].
pub trait NetworkSubscription: Send {
    /// Deregisters the notification and releases its handle. Idempotent.
    fn cancel(&mut self);
}

/// Opaque digest of the machine's active local unicast bindings.
///
/// Deliberately not reversible into addresses: only equality is meaningful,
/// and only equality ever leaves this module.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct BindingFingerprint(u64);

impl BindingFingerprint {
    /// Builds a fingerprint from an already-hashed value.
    pub fn from_hash(value: u64) -> Self {
        Self(value)
    }
}

/// Folds per-row digests of the address table into one fingerprint.
///
/// Deliberately order-independent: `GetUnicastIpAddressTable` promises no row
/// order, and the adapter re-enumeration that fires the change callback is
/// exactly when the order shifts. Feeding rows into one sequential hasher
/// would turn a reshuffle of unchanged addresses into a reported binding move,
/// and a reported move tears every RTSP/RTP/PTP session down.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BindingAccumulator {
    /// Commutative mixing of the row digests.
    xor: u64,
    /// Second commutative channel, so a repeated row cannot cancel itself out.
    sum: u64,
    /// Row count, so "two identical rows" differs from "one".
    count: u64,
}

impl BindingAccumulator {
    /// Absorbs one row digest.
    fn absorb(&mut self, row_digest: u64) {
        self.xor ^= row_digest;
        self.sum = self.sum.wrapping_add(row_digest);
        self.count = self.count.wrapping_add(1);
    }

    /// Collapses the accumulated rows into the published fingerprint.
    fn finish(self) -> BindingFingerprint {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.xor.hash(&mut hasher);
        self.sum.hash(&mut hasher);
        self.count.hash(&mut hasher);
        BindingFingerprint::from_hash(hasher.finish())
    }
}

/// Reader of the machine's currently active local bindings.
pub trait LocalBindingSource: Send + Sync {
    /// Samples the active local unicast bindings as an opaque digest.
    fn fingerprint(&self) -> BindingFingerprint;
}

/// Owns one subscription plus the task that coalesces its callback bursts.
pub struct NetworkMonitor {
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl NetworkMonitor {
    /// Subscribes and starts coalescing.
    ///
    /// The first pulse of a burst opens a fixed [`NETWORK_SETTLE`] window;
    /// every further pulse inside it is absorbed rather than extending it, so
    /// a continuously flapping adapter still yields one edge every two seconds
    /// instead of never yielding one at all. When the window closes the
    /// bindings are re-sampled and exactly one edge is published, carrying
    /// whether an active local binding actually moved.
    pub fn start(
        source: Arc<dyn NetworkChangeSource>,
        bindings: Arc<dyn LocalBindingSource>,
        timer: Arc<dyn BackendTimer>,
        out: mpsc::Sender<SystemEvent>,
    ) -> Result<Self, NetworkError> {
        let (pulse_tx, mut pulse_rx) = mpsc::channel(PULSE_CAPACITY);
        let mut subscription = source.subscribe(pulse_tx)?;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let join = tokio::spawn(async move {
            let mut last = bindings.fingerprint();
            loop {
                let pulse = tokio::select! {
                    () = task_cancel.cancelled() => None,
                    pulse = pulse_rx.recv() => pulse,
                };
                if pulse.is_none() {
                    break;
                }
                let settle = timer.sleep(NETWORK_SETTLE);
                tokio::pin!(settle);
                let mut closed = false;
                loop {
                    // Absorb everything already queued BEFORE testing the
                    // deadline. Leaving a queued pulse behind would let one
                    // burst survive its own window and open a second one --
                    // the exact multiplication this coalescer exists to stop.
                    loop {
                        match pulse_rx.try_recv() {
                            Ok(_) => continue,
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => {
                                closed = true;
                                break;
                            }
                        }
                    }
                    if closed {
                        break;
                    }
                    tokio::select! {
                        // Biased on the deadline: a continuously flapping
                        // adapter must still yield one edge every window
                        // rather than starve the settle forever.
                        biased;
                        () = &mut settle => break,
                        () = task_cancel.cancelled() => { closed = true; break; }
                        pulse = pulse_rx.recv() => {
                            if pulse.is_none() {
                                closed = true;
                                break;
                            }
                        }
                    }
                }
                if closed {
                    break;
                }
                let sampled = bindings.fingerprint();
                let local_binding_changed = sampled != last;
                last = sampled;
                // Cancellable like every other await in this task. A full
                // consumer queue must never outrank a shutdown: the actor
                // loop stops draining before `shutdown()` is called, so an
                // uncancellable publish would hold the OS notification handle
                // for the rest of the process's life.
                let publish = out.send(SystemEvent::NetworkChanged {
                    local_binding_changed,
                });
                let delivered = tokio::select! {
                    biased;
                    () = task_cancel.cancelled() => break,
                    result = publish => result.is_ok(),
                };
                if !delivered {
                    break;
                }
            }
            // The OS handle is released here and nowhere else, so a cancelled
            // monitor cannot leave a callback pointing at a freed context.
            subscription.cancel();
        });
        Ok(Self { cancel, join })
    }

    /// Cancels the subscription and joins the coalescing task.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.join.await;
    }
}

/// Production wiring for the running platform.
///
/// Returns `None` where the platform offers no interface notifications; the
/// controller then simply runs without a monitor instead of failing to start.
pub fn platform_sources() -> Option<(Arc<dyn NetworkChangeSource>, Arc<dyn LocalBindingSource>)> {
    #[cfg(windows)]
    {
        let source: Arc<dyn NetworkChangeSource> = Arc::new(windows_impl::IpHelperChangeSource);
        let bindings: Arc<dyn LocalBindingSource> = Arc::new(windows_impl::IpHelperBindings);
        Some((source, bindings))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod windows_impl {
    //! `NotifyIpInterfaceChange` / `CancelMibChangeNotify2` behind the traits.

    use std::ffi::c_void;
    use std::hash::{Hash, Hasher};

    use windows_sys::Win32::NetworkManagement::IpHelper::{
        CancelMibChangeNotify2, FreeMibTable, GetUnicastIpAddressTable, NotifyIpInterfaceChange,
        MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, MIB_UNICASTIPADDRESS_TABLE,
    };
    use windows_sys::Win32::Networking::WinSock::{
        IpDadStatePreferred, AF_INET, AF_INET6, AF_UNSPEC,
    };

    use super::*;

    /// Context handed to the OS callback for the lifetime of one registration.
    struct CallbackContext {
        pulses: mpsc::Sender<SystemEvent>,
    }

    /// Windows interface-change notification.
    ///
    /// SAFETY contract of the whole type: the context pointer handed to
    /// `NotifyIpInterfaceChange` is an `Arc` leaked for exactly as long as the
    /// registration lives. `CancelMibChangeNotify2` does not return while a
    /// callback is still running, so reclaiming the `Arc` afterwards cannot
    /// race a live callback.
    pub(super) struct IpHelperChangeSource;

    struct IpHelperSubscription {
        handle: super::windows_impl::NotifyHandle,
        context: *const CallbackContext,
    }

    /// Newtype so the raw notification handle is `Send` with an explicit
    /// justification rather than by accident.
    struct NotifyHandle(windows_sys::Win32::Foundation::HANDLE);

    // SAFETY: the handle is an opaque kernel object reference; it is only ever
    // read by `CancelMibChangeNotify2`, which is thread-safe, and the owning
    // subscription is moved into exactly one task.
    unsafe impl Send for NotifyHandle {}
    // SAFETY: the context pointer is only dereferenced by the OS callback and
    // reclaimed after `CancelMibChangeNotify2` has drained pending callbacks.
    unsafe impl Send for IpHelperSubscription {}

    unsafe extern "system" fn on_interface_change(
        context: *const c_void,
        _row: *const MIB_IPINTERFACE_ROW,
        _notification: MIB_NOTIFICATION_TYPE,
    ) {
        if context.is_null() {
            return;
        }
        // SAFETY: the pointer was produced by `Arc::into_raw` for a context
        // that outlives the registration (see the type-level contract).
        let context = unsafe { &*(context as *const CallbackContext) };
        // Never block a thread-pool callback: a full queue already represents
        // an unreported burst.
        let _ = context.pulses.try_send(SystemEvent::NetworkChanged {
            local_binding_changed: false,
        });
    }

    impl NetworkChangeSource for IpHelperChangeSource {
        fn subscribe(
            &self,
            tx: mpsc::Sender<SystemEvent>,
        ) -> Result<Box<dyn NetworkSubscription>, NetworkError> {
            let context = Arc::new(CallbackContext { pulses: tx });
            let raw = Arc::into_raw(context);
            let mut handle: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
            // SAFETY: `raw` stays alive until the subscription is cancelled,
            // and `handle` is a valid out-parameter.
            let status = unsafe {
                NotifyIpInterfaceChange(
                    AF_UNSPEC,
                    Some(on_interface_change),
                    raw as *const c_void,
                    0,
                    &mut handle,
                )
            };
            if status != 0 {
                // SAFETY: registration failed, so no callback can observe the
                // context; reclaiming it here is the only way not to leak it.
                unsafe { drop(Arc::from_raw(raw)) };
                return Err(NetworkError::Unavailable(status));
            }
            Ok(Box::new(IpHelperSubscription {
                handle: NotifyHandle(handle),
                context: raw,
            }))
        }
    }

    impl NetworkSubscription for IpHelperSubscription {
        fn cancel(&mut self) {
            if self.handle.0.is_null() {
                return;
            }
            // SAFETY: the handle came from a successful registration and is
            // cleared below, so a second call is a no-op.
            unsafe { CancelMibChangeNotify2(self.handle.0) };
            self.handle.0 = std::ptr::null_mut();
            if !self.context.is_null() {
                // SAFETY: `CancelMibChangeNotify2` has returned, so no
                // callback can still hold this pointer.
                unsafe { drop(Arc::from_raw(self.context)) };
                self.context = std::ptr::null();
            }
        }
    }

    impl Drop for IpHelperSubscription {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    /// Reads the machine's active unicast bindings through IP Helper.
    pub(super) struct IpHelperBindings;

    impl LocalBindingSource for IpHelperBindings {
        fn fingerprint(&self) -> BindingFingerprint {
            let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
            // SAFETY: `table` is a valid out-parameter; on success the table is
            // freed below through `FreeMibTable`.
            let status = unsafe { GetUnicastIpAddressTable(AF_UNSPEC, &mut table) };
            if status != 0 || table.is_null() {
                return BindingFingerprint::default();
            }
            // One digest per row, folded commutatively: the table's row order
            // is not part of the machine's binding state, and treating it as
            // such would report a phantom move on every re-enumeration.
            let mut accumulator = BindingAccumulator::default();
            // SAFETY: the OS reported success, so `NumEntries` rows follow the
            // header contiguously.
            unsafe {
                let count = (*table).NumEntries as usize;
                let rows = (*table).Table.as_ptr();
                for index in 0..count {
                    let row = &*rows.add(index);
                    // Only bindings a socket could actually use: tentative,
                    // duplicate, and deprecated addresses would make an
                    // ordinary DHCP renewal look like a topology change.
                    if row.DadState != IpDadStatePreferred {
                        continue;
                    }
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    row.InterfaceLuid.Value.hash(&mut hasher);
                    row.OnLinkPrefixLength.hash(&mut hasher);
                    let family = row.Address.si_family;
                    family.hash(&mut hasher);
                    if family == AF_INET {
                        row.Address.Ipv4.sin_addr.S_un.S_addr.hash(&mut hasher);
                    } else if family == AF_INET6 {
                        row.Address.Ipv6.sin6_addr.u.Byte.hash(&mut hasher);
                    }
                    accumulator.absorb(hasher.finish());
                }
                FreeMibTable(table as *const c_void);
            }
            accumulator.finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;

    /// Timer whose every sleep needs one explicit release from the test.
    ///
    /// Permits rather than a counter edge on purpose: a release published
    /// before the monitor even reaches its sleep must still be honoured, or
    /// the test would depend on when the spawned task happens to be polled.
    struct ManualTimer {
        release: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait::async_trait]
    impl BackendTimer for ManualTimer {
        async fn sleep(&self, _duration: Duration) {
            match self.release.acquire().await {
                Ok(permit) => permit.forget(),
                // A closed semaphore means teardown; parking is the honest
                // answer for a deadline that will never be released.
                Err(_) => std::future::pending::<()>().await,
            }
        }
    }

    #[derive(Default)]
    struct FakeSource {
        senders: Mutex<Vec<mpsc::Sender<SystemEvent>>>,
        cancels: Arc<AtomicUsize>,
    }

    struct FakeSubscription(Arc<AtomicUsize>);

    impl NetworkSubscription for FakeSubscription {
        fn cancel(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl NetworkChangeSource for FakeSource {
        fn subscribe(
            &self,
            tx: mpsc::Sender<SystemEvent>,
        ) -> Result<Box<dyn NetworkSubscription>, NetworkError> {
            self.senders.lock().expect("senders poisoned").push(tx);
            Ok(Box::new(FakeSubscription(Arc::clone(&self.cancels))))
        }
    }

    #[derive(Default)]
    struct FakeBindings(AtomicU64);

    impl LocalBindingSource for FakeBindings {
        fn fingerprint(&self) -> BindingFingerprint {
            BindingFingerprint::from_hash(self.0.load(Ordering::SeqCst))
        }
    }

    /// Bindings source that publishes one permit per sample.
    ///
    /// Sampling is the last thing the coalescer does before it publishes, so
    /// a permit is the test's proof that the task has reached its publish and
    /// nothing else.
    struct SignallingBindings {
        sampled: Arc<tokio::sync::Semaphore>,
    }

    impl LocalBindingSource for SignallingBindings {
        fn fingerprint(&self) -> BindingFingerprint {
            self.sampled.add_permits(1);
            BindingFingerprint::default()
        }
    }

    #[tokio::test]
    async fn one_burst_yields_one_edge_carrying_the_binding_verdict() {
        let source = Arc::new(FakeSource::default());
        let bindings = Arc::new(FakeBindings::default());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let timer = Arc::new(ManualTimer {
            release: Arc::clone(&release),
        });
        let (out_tx, mut out_rx) = mpsc::channel(8);

        let monitor = NetworkMonitor::start(source.clone(), bindings.clone(), timer, out_tx)
            .expect("fake subscription succeeds");

        let senders = source.senders.lock().expect("senders poisoned").clone();
        for _ in 0..20 {
            let _ = senders[0].try_send(SystemEvent::NetworkChanged {
                local_binding_changed: false,
            });
        }
        release.add_permits(1);
        assert_eq!(
            out_rx.recv().await,
            Some(SystemEvent::NetworkChanged {
                local_binding_changed: false
            }),
            "twenty callbacks are one unchanged-binding edge"
        );

        bindings.0.store(7, Ordering::SeqCst);
        let _ = senders[0].try_send(SystemEvent::NetworkChanged {
            local_binding_changed: false,
        });
        release.add_permits(1);
        assert_eq!(
            out_rx.recv().await,
            Some(SystemEvent::NetworkChanged {
                local_binding_changed: true
            }),
            "a moved binding must be reported as such"
        );

        monitor.shutdown().await;
        assert_eq!(
            source.cancels.load(Ordering::SeqCst),
            1,
            "shutdown must release the notification handle"
        );
    }

    /// The publish is the only await the coalescer performs outside a
    /// cancellation-aware `select!`. If it is not cancellable, a full consumer
    /// queue wedges `shutdown()` for ever and the OS notification handle is
    /// never handed back -- and shutdown is the first teardown step, so the
    /// whole backend thread would hang there.
    ///
    /// The guard below is a hang detector, not a timing assertion: a correct
    /// shutdown returns in microseconds, a wedged one never returns at all.
    #[tokio::test]
    async fn shutdown_returns_while_a_publish_is_blocked() {
        let source = Arc::new(FakeSource::default());
        let sampled = Arc::new(tokio::sync::Semaphore::new(0));
        let bindings = Arc::new(SignallingBindings {
            sampled: Arc::clone(&sampled),
        });
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let timer = Arc::new(ManualTimer {
            release: Arc::clone(&release),
        });
        // Capacity one, filled by the test and never drained: the monitor's
        // own publish has nowhere to go. `_out_rx` stays alive so the channel
        // is full rather than closed.
        let (out_tx, _out_rx) = mpsc::channel(1);
        out_tx
            .send(SystemEvent::Suspending)
            .await
            .expect("the receiver is alive");

        let monitor = NetworkMonitor::start(source.clone(), bindings, timer, out_tx)
            .expect("fake subscription succeeds");
        // Sample one: the baseline taken when the task starts.
        let _ = sampled.acquire().await.expect("semaphore stays open");

        let senders = source.senders.lock().expect("senders poisoned").clone();
        let _ = senders[0].try_send(SystemEvent::NetworkChanged {
            local_binding_changed: false,
        });
        release.add_permits(1);
        // Sample two: the window closed, so the very next thing the task does
        // is block on the full consumer queue.
        let _ = sampled.acquire().await.expect("semaphore stays open");

        tokio::time::timeout(Duration::from_secs(10), monitor.shutdown())
            .await
            .expect("a cancelled monitor must not wait on its consumer");
        assert_eq!(
            source.cancels.load(Ordering::SeqCst),
            1,
            "a blocked publish must still release the notification handle"
        );
    }

    mod binding_accumulator {
        use super::*;

        /// `GetUnicastIpAddressTable` guarantees no row order, and adapters are
        /// re-enumerated by exactly the event that fires the callback. A pure
        /// reshuffle of identical rows must therefore not read as a moved
        /// binding -- that would tear down every RTSP/RTP/PTP session for a
        /// change that never happened.
        #[test]
        fn row_order_does_not_change_the_fingerprint() {
            let rows = [0x1234_5678_9abc_def0_u64, 7, u64::MAX, 0, 42];
            let forward = {
                let mut acc = BindingAccumulator::default();
                for row in rows {
                    acc.absorb(row);
                }
                acc.finish()
            };
            let reversed = {
                let mut acc = BindingAccumulator::default();
                for row in rows.iter().rev() {
                    acc.absorb(*row);
                }
                acc.finish()
            };
            let rotated = {
                let mut acc = BindingAccumulator::default();
                for index in 0..rows.len() {
                    acc.absorb(rows[(index + 2) % rows.len()]);
                }
                acc.finish()
            };
            assert_eq!(forward, reversed, "a reversed table changed the verdict");
            assert_eq!(forward, rotated, "a rotated table changed the verdict");
        }

        /// Order independence must not cost sensitivity: the fingerprint still
        /// has to notice a row that actually differs.
        #[test]
        fn a_changed_row_changes_the_fingerprint() {
            let mut before = BindingAccumulator::default();
            before.absorb(1);
            before.absorb(2);
            let mut after = BindingAccumulator::default();
            after.absorb(1);
            after.absorb(3);
            assert_ne!(before.finish(), after.finish(), "a moved binding vanished");
        }

        /// A plain XOR fold would cancel identical rows out in pairs; two
        /// bindings on the same interface must not read as none.
        #[test]
        fn duplicate_rows_do_not_cancel_out() {
            let mut empty = BindingAccumulator::default();
            let mut pair = BindingAccumulator::default();
            pair.absorb(9);
            pair.absorb(9);
            assert_ne!(
                empty.finish(),
                pair.finish(),
                "two identical rows folded away to nothing"
            );
            empty.absorb(0);
            assert_ne!(
                empty.finish(),
                pair.finish(),
                "row count must be part of the digest"
            );
        }
    }
}
