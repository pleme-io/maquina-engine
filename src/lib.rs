//! # maquina-engine — the canonical VM lifecycle trait for pleme-io máquinas
//!
//! One backend-neutral trait, [`MaquinaEngine`], unifying the two pre-existing,
//! tested VM-runtime traits in the fleet:
//!
//! - `kikai::VmRuntime` over `ClusterConfig` — the rich one (launch / stop /
//!   pause / resume / save / restore / snapshot), implemented by `KasouRuntime`
//!   on Apple Virtualization.framework (VZ);
//! - `libkrun-builder::VmRuntime` over `BuilderConfig` — the lean one, with a
//!   full `MockVmRuntime`, implemented by `TateruRuntime` on libkrun
//!   (Hypervisor.framework — the lowest blessed macOS VM interface).
//!
//! This crate is **its own thing, maximally built of reusable parts**: it does
//! not invent a third runtime — it lifts the proven shapes onto one neutral
//! surface keyed on [`MaquinaId`] + [`VmShape`] + [`BootClosure`], so both
//! backends become impls of *this* trait and every consumer (kikai, tatara-vm,
//! the convergence reconciler) speaks one vocabulary.
//!
//! - [`VmState`] mirrors Apple's `VZVirtualMachineState` (kasou's proven state
//!   machine — the richest backend); libkrun maps onto the subset it supports.
//! - [`BootClosure`] generalizes `kikai::BootFiles` (kernel / initrd / init)
//!   and is produced **nix-store-friendly** by sui + tatara-nix from a
//!   `(defsystem …)` value — see `theory/MAQUINA.md` §VII.
//! - [`LaunchResult`] mirrors `kikai::LaunchResult`.
//! - [`mock::MockEngine`] mirrors the proven `MockVmRuntime`.
//!
//! **Zero shell-out** (the cardinal invariant): the impls drive libkrun/VZ
//! in-process via FFI / framework bindings. No `vfkit` / `qemu` / `virsh` /
//! `nix` subprocess in the hot path.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Stable identity of a máquina instance — conventionally the guest hostname.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MaquinaId(String);

impl MaquinaId {
    /// Construct an id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MaquinaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Normalized VM lifecycle state.
///
/// Mirrors Apple's `VZVirtualMachineState` (kasou's proven model — the richest
/// backend). libkrun maps onto the subset it supports. `Error` is terminal: the
/// máquina must be recreated, not recovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmState {
    Stopped,
    Starting,
    Running,
    Pausing,
    Paused,
    Resuming,
    Stopping,
    Saving,
    Restoring,
    Error,
}

impl VmState {
    /// Whether this state represents a live VM that holds host resources.
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Running
                | Self::Pausing
                | Self::Paused
                | Self::Resuming
                | Self::Saving
                | Self::Restoring
        )
    }

    /// Whether this is a terminal state (the VM cannot be restarted).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Error)
    }

    /// Whether a transition to `target` is a valid lifecycle move
    /// (Apple's `VZVirtualMachineState` rules); any active state may fault to
    /// `Error`.
    #[must_use]
    pub fn can_transition_to(self, target: Self) -> bool {
        matches!(
            (self, target),
            (Self::Stopped, Self::Starting)
                | (Self::Starting, Self::Running)
                | (Self::Running, Self::Pausing)
                | (Self::Pausing, Self::Paused)
                | (Self::Paused, Self::Resuming)
                | (Self::Resuming, Self::Running)
                | (Self::Running, Self::Stopping)
                | (Self::Paused, Self::Stopping)
                | (Self::Stopping, Self::Stopped)
                | (Self::Running, Self::Stopped)
                | (Self::Paused, Self::Saving)
                | (Self::Saving, Self::Paused)
                | (Self::Stopped, Self::Restoring)
                | (Self::Restoring, Self::Paused)
        ) || (target == Self::Error && self.is_active())
    }
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Pausing => "pausing",
            Self::Paused => "paused",
            Self::Resuming => "resuming",
            Self::Stopping => "stopping",
            Self::Saving => "saving",
            Self::Restoring => "restoring",
            Self::Error => "error",
        };
        f.write_str(s)
    }
}

/// How a máquina attaches to the network. Per-máquina **projection** of the
/// fleet `(defrede …)` domain (see `theory/MAQUINA.md` § Networking).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NetworkMode {
    /// NAT via the host.
    Nat,
    /// Bridge to a named host interface.
    Bridge { interface: String },
    /// No network.
    None,
}

impl Default for NetworkMode {
    fn default() -> Self {
        Self::Nat
    }
}

/// A virtiofs share mounted into the guest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareMount {
    pub host: PathBuf,
    pub guest: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

/// The runtime shape of a máquina — the hardware envelope the engine programs.
///
/// The authoring→runtime bridge `From<tatara_vm::VmSpec>` lives in `tatara-vm`,
/// so this crate stays free of the lisp/authoring dependency tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmShape {
    pub cpus: u32,
    pub memory_mib: u32,
    /// Deterministic MAC (kasou's fix for vfkit's `mac=` bug). `None` = engine
    /// default.
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub network: NetworkMode,
    #[serde(default)]
    pub shares: Vec<ShareMount>,
}

impl Default for VmShape {
    fn default() -> Self {
        Self {
            cpus: 2,
            memory_mib: 2048,
            mac: None,
            network: NetworkMode::Nat,
            shares: Vec::new(),
        }
    }
}

/// The realized, content-addressed boot closure for a máquina.
///
/// Generalizes `kikai::BootFiles`. Produced **nix-store-friendly** by sui +
/// tatara-nix from a `(defsystem …)` value — the paths are `/nix/store` paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootClosure {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    /// Init path inside the guest
    /// (e.g. `/nix/store/…-tatara-init/bin/tatara-init`).
    pub init: String,
    /// Root filesystem image, when the guest boots from a disk rather than the
    /// initrd.
    #[serde(default)]
    pub rootfs: Option<PathBuf>,
    /// Kernel command line, joined with spaces by the engine.
    #[serde(default)]
    pub cmdline: Vec<String>,
}

/// Result of a successful launch. Mirrors `kikai::LaunchResult`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchResult {
    /// PID of the supervising process (in-process engine) or child VM process.
    pub pid: u32,
    /// IP discovered via DHCP lease lookup, when a MAC was configured.
    #[serde(default)]
    pub ip: Option<String>,
}

/// Errors surfaced by a [`MaquinaEngine`].
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The engine does not implement this operation.
    #[error("operation `{op}` is not supported by this engine")]
    Unsupported { op: &'static str },
    /// No running máquina with this id.
    #[error("máquina `{0}` is not running")]
    NotRunning(MaquinaId),
    /// The máquina is in the terminal `Error` state and must be recreated.
    #[error("máquina `{0}` is in terminal Error state; recreate it")]
    Terminal(MaquinaId),
    /// A requested state change is not a valid lifecycle transition.
    #[error("invalid transition: {from} → {to}")]
    InvalidTransition { from: VmState, to: VmState },
    /// The underlying backend (kasou/VZ, tateru/libkrun) failed.
    #[error("backend error: {0}")]
    Backend(String),
}

/// The canonical, backend-neutral VM lifecycle trait for pleme-io máquinas.
///
/// One trait; two production impls (`kikai::KasouRuntime` on VZ,
/// `libkrun-builder::TateruRuntime` on libkrun) plus [`mock::MockEngine`].
/// Object-safe (`Box<dyn MaquinaEngine>`).
#[async_trait::async_trait]
pub trait MaquinaEngine: Send + Sync {
    /// Create + start a máquina from its shape and realized boot closure.
    ///
    /// # Errors
    /// [`EngineError::Backend`] if the hypervisor rejects the configuration.
    async fn launch(
        &self,
        id: &MaquinaId,
        shape: &VmShape,
        boot: &BootClosure,
    ) -> Result<LaunchResult, EngineError>;

    /// Observe the current lifecycle state.
    ///
    /// # Errors
    /// [`EngineError::Backend`] if the state cannot be read.
    fn state(&self, id: &MaquinaId) -> Result<VmState, EngineError>;

    /// Whether the máquina is live. Defaults to `state().is_active()`.
    ///
    /// # Errors
    /// Propagates [`Self::state`] errors.
    fn is_running(&self, id: &MaquinaId) -> Result<bool, EngineError> {
        Ok(self.state(id)?.is_active())
    }

    /// Gracefully stop the máquina, waiting up to `timeout_secs`.
    ///
    /// # Errors
    /// [`EngineError::Backend`] on shutdown failure.
    async fn stop(&self, id: &MaquinaId, timeout_secs: u64) -> Result<(), EngineError>;

    /// Pause (VZ freeze, zero CPU). Default: unsupported.
    ///
    /// # Errors
    /// [`EngineError::Unsupported`] unless the backend overrides.
    async fn pause(&self, id: &MaquinaId) -> Result<(), EngineError> {
        let _ = id;
        Err(EngineError::Unsupported { op: "pause" })
    }

    /// Resume a paused máquina. Default: unsupported.
    ///
    /// # Errors
    /// [`EngineError::Unsupported`] unless the backend overrides.
    async fn resume(&self, id: &MaquinaId) -> Result<(), EngineError> {
        let _ = id;
        Err(EngineError::Unsupported { op: "resume" })
    }

    /// Snapshot state to `path`. Default: unsupported.
    ///
    /// # Errors
    /// [`EngineError::Unsupported`] unless the backend overrides.
    async fn save_state(&self, id: &MaquinaId, path: &Path) -> Result<(), EngineError> {
        let _ = (id, path);
        Err(EngineError::Unsupported { op: "save_state" })
    }

    /// Restore state from `path`. Default: unsupported.
    ///
    /// # Errors
    /// [`EngineError::Unsupported`] unless the backend overrides.
    async fn restore_state(&self, id: &MaquinaId, path: &Path) -> Result<(), EngineError> {
        let _ = (id, path);
        Err(EngineError::Unsupported { op: "restore_state" })
    }

    /// Create from shape/closure, restore the snapshot, and resume — one
    /// transaction so callers don't sequence Apple's
    /// (Stopped → Paused → Running) ordering. Default: unsupported.
    ///
    /// # Errors
    /// [`EngineError::Unsupported`] unless the backend overrides.
    async fn launch_from_snapshot(
        &self,
        id: &MaquinaId,
        shape: &VmShape,
        boot: &BootClosure,
        snapshot: &Path,
    ) -> Result<LaunchResult, EngineError> {
        let _ = (id, shape, boot, snapshot);
        Err(EngineError::Unsupported {
            op: "launch_from_snapshot",
        })
    }
}

/// libkrun backend (`tateru`, Hypervisor.framework) — the default máquina
/// engine. Behind the `tateru-backend` feature (macOS).
#[cfg(all(target_os = "macos", feature = "tateru-backend"))]
pub mod tateru_backend;

/// Apple Virtualization.framework backend (`kasou`) — the rich engine
/// (snapshots, pause/resume, Rosetta-x86_64, Darwin guests, deterministic MAC).
/// Behind the `kasou-backend` feature (macOS).
#[cfg(all(target_os = "macos", feature = "kasou-backend"))]
pub mod kasou_backend;

pub mod mock {
    //! In-memory [`MaquinaEngine`] for testing — mirrors the proven
    //! `MockVmRuntime` from `kikai` / `libkrun-builder`.
    use super::{
        BootClosure, EngineError, LaunchResult, MaquinaEngine, MaquinaId, VmShape, VmState,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Recorded engine operation, for test assertions.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Call {
        Launch(MaquinaId),
        Stop(MaquinaId),
        Pause(MaquinaId),
        Resume(MaquinaId),
    }

    /// Records calls and tracks per-máquina [`VmState`] in memory.
    #[derive(Default)]
    pub struct MockEngine {
        calls: Mutex<Vec<Call>>,
        states: Mutex<HashMap<MaquinaId, VmState>>,
    }

    impl MockEngine {
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// All recorded calls, in order.
        #[must_use]
        pub fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("mock mutex poisoned").clone()
        }

        fn set(&self, id: &MaquinaId, s: VmState) {
            self.states
                .lock()
                .expect("mock mutex poisoned")
                .insert(id.clone(), s);
        }

        fn record(&self, c: Call) {
            self.calls.lock().expect("mock mutex poisoned").push(c);
        }
    }

    #[async_trait::async_trait]
    impl MaquinaEngine for MockEngine {
        async fn launch(
            &self,
            id: &MaquinaId,
            _shape: &VmShape,
            _boot: &BootClosure,
        ) -> Result<LaunchResult, EngineError> {
            self.record(Call::Launch(id.clone()));
            self.set(id, VmState::Running);
            Ok(LaunchResult {
                pid: 4242,
                ip: None,
            })
        }

        fn state(&self, id: &MaquinaId) -> Result<VmState, EngineError> {
            Ok(self
                .states
                .lock()
                .expect("mock mutex poisoned")
                .get(id)
                .copied()
                .unwrap_or(VmState::Stopped))
        }

        async fn stop(&self, id: &MaquinaId, _timeout_secs: u64) -> Result<(), EngineError> {
            self.record(Call::Stop(id.clone()));
            self.set(id, VmState::Stopped);
            Ok(())
        }

        async fn pause(&self, id: &MaquinaId) -> Result<(), EngineError> {
            self.record(Call::Pause(id.clone()));
            self.set(id, VmState::Paused);
            Ok(())
        }

        async fn resume(&self, id: &MaquinaId) -> Result<(), EngineError> {
            self.record(Call::Resume(id.clone()));
            self.set(id, VmState::Running);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{Call, MockEngine};
    use super::{BootClosure, EngineError, MaquinaEngine, MaquinaId, NetworkMode, VmShape, VmState};

    fn boot() -> BootClosure {
        BootClosure {
            kernel: "/nix/store/x-kernel/bzImage".into(),
            initrd: "/nix/store/x-initrd/initrd.cpio.gz".into(),
            init: "/bin/tatara-init".into(),
            rootfs: None,
            cmdline: vec!["console=hvc0".into()],
        }
    }

    #[test]
    fn vmstate_active_and_terminal() {
        assert!(VmState::Running.is_active());
        assert!(!VmState::Stopped.is_active());
        assert!(VmState::Error.is_terminal());
        assert!(!VmState::Running.is_terminal());
    }

    #[test]
    fn vmstate_transitions() {
        assert!(VmState::Stopped.can_transition_to(VmState::Starting));
        assert!(VmState::Running.can_transition_to(VmState::Error));
        assert!(!VmState::Stopped.can_transition_to(VmState::Running));
        assert!(!VmState::Stopped.can_transition_to(VmState::Error));
    }

    #[test]
    fn ids_and_display() {
        let id = MaquinaId::new("rio-builder");
        assert_eq!(id.as_str(), "rio-builder");
        assert_eq!(id.to_string(), "rio-builder");
        assert_eq!(VmState::Running.to_string(), "running");
    }

    #[test]
    fn defaults() {
        let s = VmShape::default();
        assert_eq!(s.cpus, 2);
        assert_eq!(s.memory_mib, 2048);
        assert_eq!(s.network, NetworkMode::Nat);
    }

    #[tokio::test]
    async fn mock_launch_running_stop() {
        let e = MockEngine::new();
        let id = MaquinaId::new("vm0");
        assert_eq!(e.state(&id).unwrap(), VmState::Stopped);
        assert!(!e.is_running(&id).unwrap());

        let r = e.launch(&id, &VmShape::default(), &boot()).await.unwrap();
        assert_eq!(r.pid, 4242);
        assert_eq!(e.state(&id).unwrap(), VmState::Running);
        assert!(e.is_running(&id).unwrap());

        e.stop(&id, 10).await.unwrap();
        assert_eq!(e.state(&id).unwrap(), VmState::Stopped);
        assert_eq!(e.calls(), vec![Call::Launch(id.clone()), Call::Stop(id)]);
    }

    #[tokio::test]
    async fn mock_pause_resume() {
        let e = MockEngine::new();
        let id = MaquinaId::new("vm1");
        e.launch(&id, &VmShape::default(), &boot()).await.unwrap();
        e.pause(&id).await.unwrap();
        assert_eq!(e.state(&id).unwrap(), VmState::Paused);
        e.resume(&id).await.unwrap();
        assert_eq!(e.state(&id).unwrap(), VmState::Running);
    }

    #[tokio::test]
    async fn unsupported_defaults_surface_typed_error() {
        // A minimal engine that implements only the required methods.
        struct Bare;
        #[async_trait::async_trait]
        impl MaquinaEngine for Bare {
            async fn launch(
                &self,
                _: &MaquinaId,
                _: &VmShape,
                _: &BootClosure,
            ) -> Result<super::LaunchResult, EngineError> {
                Ok(super::LaunchResult { pid: 1, ip: None })
            }
            fn state(&self, _: &MaquinaId) -> Result<VmState, EngineError> {
                Ok(VmState::Running)
            }
            async fn stop(&self, _: &MaquinaId, _: u64) -> Result<(), EngineError> {
                Ok(())
            }
        }
        let e = Bare;
        let id = MaquinaId::new("vm2");
        assert!(matches!(
            e.pause(&id).await,
            Err(EngineError::Unsupported { op: "pause" })
        ));
        assert!(matches!(
            e.save_state(&id, std::path::Path::new("/tmp/s")).await,
            Err(EngineError::Unsupported { op: "save_state" })
        ));
    }

    #[test]
    fn object_safe() {
        fn _takes(_: &dyn MaquinaEngine) {}
    }
}
