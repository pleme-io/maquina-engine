//! Apple Virtualization.framework backend — `KasouEngine` implements
//! [`crate::MaquinaEngine`] over the `kasou` library (objc2-virtualization).
//!
//! The **rich** engine: native pause/resume + snapshot (save/restore), the
//! deterministic-MAC fix, Rosetta-x86_64, and Darwin guests. In-process, zero
//! shell-out. Reuses `kasou::{VmConfigBuilder, VmHandle, VmState}` directly —
//! the same library `kikai` drives.
//!
//! v0.1 note: `save_state` / `restore_state` are wired but require a
//! `machine_identifier_path` on the VM config for VZ snapshot round-trips; a
//! state-dir convention for that lands with the reconciler (M3).

use std::path::Path;
use std::sync::Mutex;

use crate::{BootClosure, EngineError, LaunchResult, MaquinaEngine, MaquinaId, VmShape, VmState};

/// A VZ-backed máquina engine. Owns the live `kasou::VmHandle` in-process.
#[derive(Default)]
pub struct KasouEngine {
    handle: Mutex<Option<kasou::VmHandle>>,
}

impl KasouEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a `kasou::VmConfig` from the neutral shape + boot closure.
    fn build_config(
        id: &MaquinaId,
        shape: &VmShape,
        boot: &BootClosure,
    ) -> Result<kasou::VmConfig, EngineError> {
        let mut b = kasou::VmConfigBuilder::new(id.as_str())
            .cpus(shape.cpus)
            .memory_mib(u64::from(shape.memory_mib))
            .boot(boot.kernel.clone())
            .initrd(boot.initrd.clone());

        if !boot.cmdline.is_empty() {
            b = b.cmdline(boot.cmdline.join(" "));
        }
        if let Some(rootfs) = &boot.rootfs {
            b = b.disk(rootfs.clone());
        }
        b = match &shape.mac {
            Some(mac) => b.mac(mac.clone()),
            // kasou's reason to exist: a deterministic MAC keyed on the id.
            None => b.deterministic_mac(id.as_str()),
        };
        for s in &shape.shares {
            b = b.shared_dir(mount_tag(&s.guest), s.host.clone(), s.read_only);
        }
        b.build().map_err(backend)
    }
}

fn backend<E: std::fmt::Display>(e: E) -> EngineError {
    EngineError::Backend(e.to_string())
}

/// Map kasou's VZ-derived state onto the neutral [`VmState`]. Variants are 1:1
/// by construction (both mirror `VZVirtualMachineState`).
impl From<kasou::VmState> for VmState {
    fn from(s: kasou::VmState) -> Self {
        match s {
            kasou::VmState::Stopped => Self::Stopped,
            kasou::VmState::Starting => Self::Starting,
            kasou::VmState::Running => Self::Running,
            kasou::VmState::Pausing => Self::Pausing,
            kasou::VmState::Paused => Self::Paused,
            kasou::VmState::Resuming => Self::Resuming,
            kasou::VmState::Stopping => Self::Stopping,
            kasou::VmState::Saving => Self::Saving,
            kasou::VmState::Restoring => Self::Restoring,
            kasou::VmState::Error => Self::Error,
        }
    }
}

/// Derive a POSIX-safe virtiofs tag from a guest path (matches tatara-vm).
fn mount_tag(guest: &Path) -> String {
    let mut tag = String::new();
    for c in guest.to_string_lossy().chars() {
        tag.push(if c.is_ascii_alphanumeric() { c } else { '_' });
    }
    let trimmed: String = tag.chars().skip_while(|c| *c == '_').collect();
    if trimmed.is_empty() {
        "share".to_string()
    } else {
        trimmed
    }
}

#[async_trait::async_trait]
impl MaquinaEngine for KasouEngine {
    async fn launch(
        &self,
        id: &MaquinaId,
        shape: &VmShape,
        boot: &BootClosure,
    ) -> Result<LaunchResult, EngineError> {
        let config = Self::build_config(id, shape, boot)?;
        let handle = kasou::VmHandle::create(config).map_err(backend)?;
        handle.start().map_err(backend)?;
        *self
            .handle
            .lock()
            .expect("KasouEngine handle mutex poisoned") = Some(handle);
        Ok(LaunchResult {
            pid: std::process::id(),
            ip: None,
        })
    }

    fn state(&self, _id: &MaquinaId) -> Result<VmState, EngineError> {
        Ok(self
            .handle
            .lock()
            .expect("KasouEngine handle mutex poisoned")
            .as_ref()
            .map_or(VmState::Stopped, |h| h.state().into()))
    }

    async fn stop(&self, _id: &MaquinaId, _timeout_secs: u64) -> Result<(), EngineError> {
        let mut guard = self
            .handle
            .lock()
            .expect("KasouEngine handle mutex poisoned");
        if let Some(h) = guard.as_ref() {
            h.stop().map_err(backend)?;
        }
        *guard = None;
        Ok(())
    }

    async fn pause(&self, _id: &MaquinaId) -> Result<(), EngineError> {
        self.handle
            .lock()
            .expect("KasouEngine handle mutex poisoned")
            .as_ref()
            .ok_or(EngineError::Unsupported { op: "pause (no live máquina)" })?
            .pause()
            .map_err(backend)
    }

    async fn resume(&self, _id: &MaquinaId) -> Result<(), EngineError> {
        self.handle
            .lock()
            .expect("KasouEngine handle mutex poisoned")
            .as_ref()
            .ok_or(EngineError::Unsupported { op: "resume (no live máquina)" })?
            .resume()
            .map_err(backend)
    }

    async fn save_state(&self, _id: &MaquinaId, path: &Path) -> Result<(), EngineError> {
        self.handle
            .lock()
            .expect("KasouEngine handle mutex poisoned")
            .as_ref()
            .ok_or(EngineError::Unsupported { op: "save_state (no live máquina)" })?
            .save_state(path)
            .map_err(backend)
    }

    async fn restore_state(&self, _id: &MaquinaId, path: &Path) -> Result<(), EngineError> {
        self.handle
            .lock()
            .expect("KasouEngine handle mutex poisoned")
            .as_ref()
            .ok_or(EngineError::Unsupported { op: "restore_state (no live máquina)" })?
            .restore_state(path)
            .map_err(backend)
    }
}
