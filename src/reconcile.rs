//! Convergence — reconcile a máquina's running state toward its desired
//! `(defmaquina …)` spec. **The Viggy Method at machine scope.**
//!
//! One tick is Observe → Diff → Decide → Act: read the engine's [`VmState`],
//! compare it to the [`MaquinaDesired`] intent (+ the last applied
//! [`BootClosure`]), and call the engine to close the gap. Per the org rule
//! "every reconcile loop is a `shigoto::Dag`", the tick is wrapped as a
//! [`shigoto_types::RecordingJob`] ([`MaquinaReconcileJob`]) whose typed
//! `Output` is the [`ReconcileOutcome`].
//!
//! v0.1 ships the pure decide+act core + the Job. The next increment is the
//! `defguest` daemon that ticks an `InProcessScheduler` over the source file
//! (sui-watch → re-eval → reconcile, *edit → converge <1s*) and attests each
//! outcome to an OutcomeChain.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shigoto_types::{JobScope, JobSubject, OutputSink, RecordingJob};

use crate::{BootClosure, EngineError, MaquinaEngine, MaquinaId, VmShape, VmState};

/// The desired state of a máquina — what the `(defmaquina …)` source declares.
/// The `Maquina` caixa kind (G6) produces this; the reconciler consumes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaquinaDesired {
    pub id: MaquinaId,
    pub shape: VmShape,
    pub boot: BootClosure,
    /// Lifecycle intent — should this máquina be running?
    pub want_running: bool,
}

/// What the reconciler did this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReconcileAction {
    /// Observed already matches desired; nothing to do.
    NoOp,
    /// Was inactive, wanted running → launched.
    Launched,
    /// Was active, wanted stopped → stopped.
    Stopped,
    /// Boot closure changed → stopped + relaunched.
    Restarted,
    /// Observed terminal `Error` — recreate, not recover; surfaced for the
    /// daemon/escalation layer to act on.
    TerminalError,
}

/// Typed outcome of one reconcile tick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileOutcome {
    pub id: MaquinaId,
    pub action: ReconcileAction,
    pub observed: VmState,
}

/// One convergence tick: observe the engine, diff against `desired` (+ the
/// `last_boot` previously applied, if any), and act to close the gap.
///
/// Idempotent — calling it when already converged yields
/// [`ReconcileAction::NoOp`].
///
/// # Errors
/// Propagates [`EngineError`] from the engine's observe/act calls.
pub async fn reconcile_once(
    engine: &dyn MaquinaEngine,
    desired: &MaquinaDesired,
    last_boot: Option<&BootClosure>,
) -> Result<ReconcileOutcome, EngineError> {
    let observed = engine.state(&desired.id)?;

    let action = if observed.is_terminal() {
        // Terminal Error: recreate, not recover (per VmState). The daemon /
        // escalation layer decides; here we surface it.
        ReconcileAction::TerminalError
    } else if desired.want_running {
        let closure_changed = last_boot.is_some_and(|prev| prev != &desired.boot);
        if closure_changed && observed.is_active() {
            engine.stop(&desired.id, 30).await?;
            engine
                .launch(&desired.id, &desired.shape, &desired.boot)
                .await?;
            ReconcileAction::Restarted
        } else if observed.is_active() {
            ReconcileAction::NoOp
        } else {
            engine
                .launch(&desired.id, &desired.shape, &desired.boot)
                .await?;
            ReconcileAction::Launched
        }
    } else if observed.is_active() {
        engine.stop(&desired.id, 30).await?;
        ReconcileAction::Stopped
    } else {
        ReconcileAction::NoOp
    };

    Ok(ReconcileOutcome {
        id: desired.id.clone(),
        action,
        observed,
    })
}

/// A `shigoto` Job that runs one reconcile tick for a máquina. The scheduler
/// ticks it; the typed `Output` ([`ReconcileOutcome`]) flows to an optional
/// [`OutputSink`] for receipts / attestation.
pub struct MaquinaReconcileJob {
    engine: Arc<dyn MaquinaEngine>,
    desired: MaquinaDesired,
    last_boot: Option<BootClosure>,
    sink: Option<Arc<dyn OutputSink<ReconcileOutcome>>>,
}

impl MaquinaReconcileJob {
    #[must_use]
    pub fn new(engine: Arc<dyn MaquinaEngine>, desired: MaquinaDesired) -> Self {
        Self {
            engine,
            desired,
            last_boot: None,
            sink: None,
        }
    }

    /// The boot closure currently applied to the running máquina, so the tick
    /// can detect a closure change and restart.
    #[must_use]
    pub fn with_last_boot(mut self, last_boot: BootClosure) -> Self {
        self.last_boot = Some(last_boot);
        self
    }

    /// Wire an output sink to capture each tick's [`ReconcileOutcome`].
    #[must_use]
    pub fn with_sink(mut self, sink: Arc<dyn OutputSink<ReconcileOutcome>>) -> Self {
        self.sink = Some(sink);
        self
    }
}

#[async_trait::async_trait]
impl RecordingJob for MaquinaReconcileJob {
    type Output = ReconcileOutcome;
    type Error = EngineError;
    const KIND: &'static str = "maquina.reconcile";

    fn scope(&self) -> JobScope {
        JobScope::Global
    }

    fn subject(&self) -> JobSubject {
        JobSubject::Pinned(self.desired.id.as_str().to_string())
    }

    fn output_sink(&self) -> Option<&Arc<dyn OutputSink<Self::Output>>> {
        self.sink.as_ref()
    }

    async fn execute_body(&self) -> Result<ReconcileOutcome, EngineError> {
        reconcile_once(self.engine.as_ref(), &self.desired, self.last_boot.as_ref()).await
    }
}

#[cfg(test)]
mod tests {
    use super::{reconcile_once, MaquinaDesired, ReconcileAction};
    use crate::mock::MockEngine;
    use crate::{BootClosure, MaquinaEngine, MaquinaId, VmShape, VmState};

    fn boot(kernel: &str) -> BootClosure {
        BootClosure {
            kernel: kernel.into(),
            initrd: "/nix/store/x-initrd/initrd.cpio.gz".into(),
            init: "/bin/tatara-init".into(),
            rootfs: None,
            cmdline: vec!["console=hvc0".into()],
        }
    }

    fn desired(want_running: bool, kernel: &str) -> MaquinaDesired {
        MaquinaDesired {
            id: MaquinaId::new("vm0"),
            shape: VmShape::default(),
            boot: boot(kernel),
            want_running,
        }
    }

    #[tokio::test]
    async fn launches_when_inactive_and_wanted() {
        let e = MockEngine::new();
        let d = desired(true, "/k-a");
        let out = reconcile_once(&e, &d, None).await.unwrap();
        assert_eq!(out.action, ReconcileAction::Launched);
        assert_eq!(e.state(&d.id).unwrap(), VmState::Running);
    }

    #[tokio::test]
    async fn noop_when_already_converged() {
        let e = MockEngine::new();
        let d = desired(true, "/k-a");
        reconcile_once(&e, &d, None).await.unwrap(); // launch
        // Second tick: running, same closure → NoOp.
        let out = reconcile_once(&e, &d, Some(&d.boot)).await.unwrap();
        assert_eq!(out.action, ReconcileAction::NoOp);
        assert_eq!(out.observed, VmState::Running);
    }

    #[tokio::test]
    async fn stops_when_running_but_unwanted() {
        let e = MockEngine::new();
        let up = desired(true, "/k-a");
        reconcile_once(&e, &up, None).await.unwrap(); // launch
        let down = desired(false, "/k-a");
        let out = reconcile_once(&e, &down, Some(&down.boot)).await.unwrap();
        assert_eq!(out.action, ReconcileAction::Stopped);
        assert_eq!(e.state(&down.id).unwrap(), VmState::Stopped);
    }

    #[tokio::test]
    async fn restarts_when_closure_changes() {
        let e = MockEngine::new();
        let old = desired(true, "/k-a");
        reconcile_once(&e, &old, None).await.unwrap(); // launch with boot A
        // Desired now carries boot B; last applied was boot A → restart.
        let new = desired(true, "/k-b");
        let out = reconcile_once(&e, &new, Some(&old.boot)).await.unwrap();
        assert_eq!(out.action, ReconcileAction::Restarted);
        assert_eq!(e.state(&new.id).unwrap(), VmState::Running);
    }
}
