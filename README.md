# maquina-engine

The canonical backend-neutral VM lifecycle trait for pleme-io *máquinas*.

One trait — `MaquinaEngine` — unifying the two pre-existing, tested VM-runtime
traits in the fleet, so a caller drives a VM without knowing which backend is
underneath:

| Backend | Crate | Platform |
|---|---|---|
| Virtualization.framework | `kasou` | macOS |
| libkrun | `tateru` | Linux |

## Usage

```toml
[dependencies]
maquina-engine = "0.1"
```

Implement `MaquinaEngine` for a backend, or take one as a trait object and let
the call site stay backend-neutral.

## License

MIT — see [LICENSE](./LICENSE).
