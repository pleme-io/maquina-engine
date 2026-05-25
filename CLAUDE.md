# maquina-engine — canonical VM lifecycle trait

Part of the **máquina** primitive. Canonical spec:
[`theory/MAQUINA.md`](../theory/MAQUINA.md). CSE / Blackmatter Development
apply; read the org-level `CLAUDE.md` first.

One backend-neutral trait, `MaquinaEngine`, unifying the fleet's two tested
VM-runtime traits onto one surface keyed on `(MaquinaId, VmShape, BootClosure)`:

- `kikai::VmRuntime` / VZ via `KasouRuntime` (the rich one — snapshot/pause/resume),
- `libkrun-builder::VmRuntime` / libkrun via `TateruRuntime` (+ the proven Mock).

## Principles

- **Its own primitive, maximally reused.** `VmState` ← kasou's VZ state machine;
  `BootClosure` ← `kikai::BootFiles`; `MockEngine` ← the proven `MockVmRuntime`.
  No third runtime is invented.
- **Zero shell-out** (cardinal invariant, MAQUINA.md §I): impls drive libkrun/VZ
  in-process via FFI/framework bindings. No `vfkit`/`qemu`/`virsh`/`nix`
  subprocess in the hot path.
- **nix-store-friendly via sui.** `BootClosure` paths are `/nix/store` paths
  produced by sui + tatara-nix from a `(defsystem …)` value. All authoring
  lowers to the underlying sui (MAQUINA.md §VII).

## M0 wiring (consumers)

- `kikai::KasouRuntime` → `impl MaquinaEngine` (VZ).
- `libkrun-builder::TateruRuntime` → `impl MaquinaEngine` (libkrun; **default**).
- `tatara-vm`: `From<VmSpec> for VmShape` + `BootClosure` realization (the
  authoring→runtime bridge); `tatara-vmctl` drives `MaquinaEngine` in-process.

## Status

v0.1: trait + neutral types + `MockEngine` + tests. `cargo test`. Next:
`flake.nix` + auto-release (via the `rust-library` substrate builder) and the
two backend adapters.

skip-shigoto: pure-trait crate, no work graph.
skip-typed-spec-triplet: single-file trait primitive, no authored algorithm.
skip-catalog: single primitive, not a multi-domain substrate.
skip-mass-synthesis: single-variant, not a synthesizer.
