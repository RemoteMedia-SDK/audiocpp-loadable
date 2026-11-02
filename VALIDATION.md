# Validation record

Validated on Windows x64 with MSVC 19.44, Rust 1.91, and CUDA 13.0 / RTX 4090. Native source: `06c13737cc39481787a03465697b58b1c954b56f`. SDK ABI: `44e02a9d24b1427cce48bc32ef837142787d0ba3`.

| Requirement | Evidence | Result |
|---|---|---|
| Native CPU build | Full upstream model composite, C API and package-manager bridge built with MSVC | Passed |
| Native CUDA build | Full composite built with Ninja/CUDA 13, architecture 89 | Passed |
| Loadable registration | Standalone host loaded the release DLL through `NodePluginRef` and discovered both factories | Passed for CPU and CUDA packages |
| Offline adapter contracts | Five tests: malformed audio/config, named audio and metadata/artifacts, callback failure, duplicate final-waveform prevention | Passed |
| Catalog/import lifecycle | Real ABI tests: schema, catalog, imports, persistence after node recreation, failed load, unknown operations, safe unregister | Passed for CPU and CUDA packages |
| Download lifecycle | Native download/progress, repeat request, completion, cancellation and installed-state checks | Passed for CPU and CUDA packages |
| Model lifecycle | PulseVAD download/load/inference/unload/removal | Passed with both native builds |
| Streaming output | PocketTTS generated speech through the ABI `OutputSink`, with `alba`, seed 42; finite non-silent PCM and final waveform deduplication | Passed with CPU and CUDA backends |
| Offline transcription | Moonshine streaming-tiny model transcribed PocketTTS output | Passed with CPU and CUDA backends |
| Streaming input/isolation | Moonshine start/push/finish/reset, two explicit session IDs through one node, unload blocked until both streams release | Passed with CPU and CUDA backends |
| Local packaging | `build.ps1` created `dist/cpu` and `dist/cuda` and ran their standalone hosts | Passed |
| User-facing download command | `run.ps1 -Method download` installed PulseVAD into a persistent cache and waited until completion | Passed |
| Static checks | `cargo clippy --all-targets -- -D warnings` and `cargo fmt` | Passed |
| OpenSpec | `bun x --bun @fission-ai/openspec validate add-audiocpp-loadable-plugin --strict` | Passed |

Final CPU run: **5 offline tests + 4 native ABI tests passed**. Final CUDA run: **4 native ABI tests passed**. Native ABI tests loaded the packaged release DLLs, not a statically linked implementation.

The CPU speech smoke transcribed: “Hello from Remote Media. This is a test of local speech.” CUDA produced a slightly different transcript (“Hello, Farmer Mo Media…”). These are execution smoke tests, not recognition-accuracy or CPU/CUDA numerical-parity claims.

## Coverage boundaries

- The adapter exposes the complete pinned C API task/option vocabulary. Live models exercised were PulseVAD, PocketTTS and Moonshine. Other families, including source separation and artifact-producing models, have mapping/fixture coverage but have not been certified with real weights here.
- Linux/macOS configuration is provided but unexecuted. Windows CPU and CUDA are the acceptance platforms tested.
- Model management uses JSON-RPC envelopes through the node data port. The host-pinned native ABI lacks a typed control-bus RPC reply hook. This plugin does not claim compatibility with the host's typed `call_rpc` proxy.
- The same ABI drops per-frame host session labels. Use explicit structured `session_id` for multiplexing; each node also has a separate default stream.
- One worker serializes handle access per library/cache directory. Blocking native inference can delay management calls. Cancellation takes effect at native cooperative boundaries; running kernels are not forcibly interrupted.
- Native libraries are pinned until process exit to avoid an observed upstream unload/reload access violation. Model/session/job resources are still released and their lifecycle is covered by the tests.
- CUDA packages include required toolkit math/runtime DLLs; NVIDIA drivers and MSVC runtimes remain system prerequisites. No public release was published.
- Ripwire's quality-delta command could not establish a baseline through this workspace's WSL wrapper. Focused compiler/lint checks and native ABI tests provide the recorded evidence instead.

Reproduction commands and package configuration are in [README.md](README.md). Build/test artifacts and native source checkouts are excluded from version control.
