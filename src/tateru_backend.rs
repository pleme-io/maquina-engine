//! libkrun backend — `TateruEngine` implements [`crate::MaquinaEngine`] over
//! `tateru` (direct libkrun FFI / Apple Hypervisor.framework). This is the
//! **default** máquina engine: the lowest blessed macOS VM interface, driven
//! in-process, zero shell-out.
//!
//! Reuses the `tateru` library directly — the same API `libkrun-builder`
//! drives — so this is the generic-máquina counterpart of `libkrun-builder`'s
//! builder-specific `TateruRuntime` (which can later consume this).

use std::path::Path;
use std::sync::Mutex;

use crate::{BootClosure, EngineError, LaunchResult, MaquinaEngine, MaquinaId, VmShape, VmState};

/// A libkrun-backed máquina engine. Owns the running VM handle in-process for
/// as long as the engine lives.
#[derive(Default)]
pub struct TateruEngine {
    handle: Mutex<Option<tateru::VmHandle>>,
}

impl TateruEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn backend<E: std::fmt::Display>(e: E) -> EngineError {
    EngineError::Backend(e.to_string())
}

/// Derive a POSIX-safe virtiofs mount tag from a guest path
/// (non-alphanumerics → `_`); mirrors tatara-vm's tag derivation so both
/// sides agree.
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
impl MaquinaEngine for TateruEngine {
    async fn launch(
        &self,
        _id: &MaquinaId,
        shape: &VmShape,
        boot: &BootClosure,
    ) -> Result<LaunchResult, EngineError> {
        // libkrun boots from an EFI disk image; the rootfs closure is required.
        let image = boot.rootfs.as_ref().ok_or_else(|| {
            EngineError::Backend(
                "libkrun (tateru) engine requires boot.rootfs (a disk image)".to_string(),
            )
        })?;

        // tateru's `.memory()` accepts a size string; pass plain MiB (no
        // `format!()` — typed-emission discipline).
        let mem = shape.memory_mib.to_string() + "MiB";

        let mut builder = tateru::Vm::builder(tateru::LibkrunEngine::new())
            .cpus(u8::try_from(shape.cpus).unwrap_or(2))
            .map_err(backend)?
            .memory(&mem)
            .map_err(backend)?
            .disk(tateru::DiskConfig {
                path: image.clone(),
                format: tateru::DiskFormat::Qcow2,
                read_only: false,
            });

        for s in &shape.shares {
            builder = builder.virtiofs(tateru::VirtioFsMount {
                host_path: s.host.clone(),
                mount_tag: mount_tag(&s.guest),
            });
        }

        let vm = builder.launch().await.map_err(backend)?;
        *self
            .handle
            .lock()
            .expect("TateruEngine handle mutex poisoned") = Some(vm);

        // In-process FFI VM: the supervising pid is this process; no DHCP IP.
        Ok(LaunchResult {
            pid: std::process::id(),
            ip: None,
        })
    }

    fn state(&self, _id: &MaquinaId) -> Result<VmState, EngineError> {
        let running = self
            .handle
            .lock()
            .expect("TateruEngine handle mutex poisoned")
            .as_ref()
            .is_some_and(tateru::VmHandle::is_running);
        Ok(if running {
            VmState::Running
        } else {
            VmState::Stopped
        })
    }

    async fn stop(&self, _id: &MaquinaId, _timeout_secs: u64) -> Result<(), EngineError> {
        let mut guard = self
            .handle
            .lock()
            .expect("TateruEngine handle mutex poisoned");
        if let Some(ref mut handle) = *guard {
            handle.stop().map_err(backend)?;
        }
        *guard = None;
        Ok(())
    }
}
