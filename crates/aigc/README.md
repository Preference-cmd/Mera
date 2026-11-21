# Sealed AIGC Source Archive (`crates/aigc/`)

> **Status: Read-only source archive.**
> This directory is excluded from the active Cargo workspace (`exclude = ["crates/aigc"]` in the root `Cargo.toml`) and is skipped by active workspace CI, clippy, tests, and module/dependency guards. It is **not guaranteed to compile standalone** as-is.

## Purpose

The active Mera workspace focuses on the host kernel, project/board/collection/library/recipe/dynamic-workflow services, the external mod SDK & manager, and the MCP operator adapter. All domain-specific AIGC inference, Burn/WGPU backends, out-of-process worker protocol/host, HuggingFace model management/acquisition, and legacy static-workflow node-graph runtime crates have been consolidated here for historical reference and future AIGC modularization.

## Sealed Crates (10)

| Directory | Package Name | Original Role |
|---|---|---|
| `aigc/` | `aigc` | Inference composition, worker switching, model services, compute profile DTOs |
| `backend-worker-host/` | `mera-backend-worker-host` | Out-of-process worker installation, inventory, and lifecycle host |
| `backend-worker-protocol/` | `mera-backend-worker-protocol` | Worker gRPC wire protocol, local/TLS transports, and `.proto` schema |
| `inference/` | `mera-inference` | Backend trait contract, router, execution values, executor registry |
| `inference-backends/burn/` | `mera-inference-burn` | Concrete Burn 0.21 inference backend (`wgpu` / `flex`) |
| `inference-backends/burn-worker/` | `mera-inference-burn-worker` | Standalone Burn worker binary over the worker gRPC protocol |
| `model-acquisition/` | `mera-model-acquisition` | HuggingFace catalog search, component resolution, staged download |
| `model-manager/` | `mera-model-manager` | Model manifest scanning, series config, checkpoint resolver |
| `nodes/` | `mera-nodes` | V1 built-in SDXL static node catalog |
| `runtime/` | `mera-runtime` | Legacy static-graph `RuntimeService`, scheduler, and run/value store |

## Archived Artifacts

- `archived-tests/`: Former `crates/app-host/tests/` integration suites and fixtures coupled to AIGC, Burn, workers, and static workflow proposals (`aigc_isolation.rs`, `burn_full_pipeline.rs`, `burn_real_e2e.rs`, `kernel_only.rs`, `run_workflow.rs`, `service_shell.rs`, `topology_integration.rs`, `worker_inventory.rs`, `workflow_proposals.rs`, `sdxl_workflows.rs`, `sdxl-base-burn-smoke-workflow.json`).
- `archived-config/`: Former `crates/config/src/inference_backend.rs` (`InferenceBackendConfig`, `InferenceBackendKind`, `WorkerEndpointConfig`).

## Restoration Notes

Internal `Cargo.toml` manifests inside these sealed crates retain their original relative path references (such as `../core` and `../config`) as a historical snapshot. If any of these crates are restored or extracted into an external `.rmod` plugin in the future, update their path dependencies (for example to `../../core` and `../../config`) and re-evaluate dependencies against the active workspace lockfile.
