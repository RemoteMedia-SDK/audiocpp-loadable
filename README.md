# audio.cpp loadable plugin

Two standalone Rust factories embed audio.cpp in the host process:

- `AudioCppNode`: native audio/text inference, offline and streaming, with model-specific options and artifacts.
- `AudioCppModelManagerNode`: catalog, downloads, local imports, persistent cache and loaded-model lifecycle.

This repository is the canonical source of the plugin. RemoteMedia SDK examples that link it in-process (for example `examples/live-link-face-avatar`) expect a clone of it next to the `remotemedia-sdk` checkout, at `../audiocpp-loadable`, like the other plugin repositories.

Upstream is pinned to `06c13737cc39481787a03465697b58b1c954b56f`. The plugin SDK is pinned to `605d203e3597a98d8190bf99686a43234624a05c` (remotemedia-plugin-sdk 0.7.0, loadable-node-abi 0.5.0, the same crates as the host's pin). No native inference dependency is added to the main Cargo workspace.

## Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`, the shared
`build-plugin.yml` from remotemedia-plugin-sdk. It builds the plugin library for
x86_64/aarch64 Linux, aarch64 macOS and x86_64 Windows, checks it resolves
loadable-node-abi 0.5.0, and publishes `libaudiocpp-{arch}-{os}.{ext}` with a
`release-manifest.json`, so a manifest can declare `"plugins": ["audiocpp-loadable@v0.1.0"]`.

The release carries only the plugin library. The audio.cpp native libraries
(`audiocpp` and `audiocpp_manager_bridge`) still come from `build.ps1` or the
CMake project. Point `library_dir` (or `AUDIOCPP_LIBRARY_DIR`) at them.

## Build and run on Windows

Requires Rust, CMake 3.24+, libclang (for bindgen), MSVC C++ tools, and Ninja for CUDA. Run from this directory:

```powershell
./build.ps1 -Backend cpu
./build.ps1 -Backend cuda -CudaRoot 'C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.0'
./run.ps1 -Method capabilities
./run.ps1 -Method catalog
./run.ps1 -Method schema
./run.ps1 -Method download -ParamsJson '{"model":"pocket_tts_english_q8_0"}'
```

CUDA defaults to architecture 89 (RTX 4090); use `-CudaArchitectures` for other GPUs. The script initializes MSVC for Ninja. `-SourceDir` accepts an existing checkout with the exact pinned C header; `-BuildDirectory` allows reusing a native build directory. Normal builds fetch the pinned source automatically. Downloads are not needed for `capabilities`, `catalog`, or `schema`.

The script creates `dist/cpu` or `dist/cuda`, including the plugin DLL, native DLLs and `host.exe`, then checks discovery through the real ABI. Keep the native DLLs together. The CUDA package includes the required CUDA math/runtime DLLs from the selected toolkit; the NVIDIA driver and MSVC runtime remain system prerequisites. This is a local distribution, not a public release. Native code is built with upstream's x64 CPU baseline (AVX2). Model-specific external dependencies, such as phonemizers, remain subject to upstream requirements.

Linux/macOS use the same CMake project and Rust crate:

```sh
cmake -S native -B native/build-local -DCMAKE_BUILD_TYPE=Release
cmake --build native/build-local --target remotemedia_audiocpp_native -j 8
cargo build --locked --release
```

Those platforms are build configurations, not claims of validation on this Windows machine.

## Native Android

`build-android.ps1` cross-compiles the native C API, model-manager bridge and CLI
for ARM64 Android; `test-android.ps1` runs inference on an explicitly selected
ADB device. Pixel 6 Pro validation includes real IndexTTS 2.5 GGUF synthesis.
See [ANDROID.md](ANDROID.md) for reproduction, timings and the remaining APK,
loadable-plugin and remote-runner integration work.

[QWEN3_ANDROID.md](QWEN3_ANDROID.md) describes the Qwen3-TTS comparison benchmark,
with separate loading/preparation timings and repeated requests in one session.

## Pipeline configuration

Reference the local `audiocpp_loadable.dll` in the pipeline's `plugins` list. Create nodes with `node_type` equal to one of the two factories and pass these configuration fields through `params`:

| Field | Behavior |
|---|---|
| `library_dir` | Directory containing native DLLs; alternatively `AUDIOCPP_LIBRARY_DIR`. Required. |
| `cache_dir` | Persistent model cache; defaults to the OS user cache under `remotemedia/audiocpp`. |
| `model` | Installed package ID, `local:` import ID, or existing local model path. Required for inference. |
| `task` | Canonical upstream task token, obtained from `capabilities`. Required for inference. |
| `mode` | `offline` (default) or `streaming`. |
| `backend`, `device`, `threads` | Defaults: `cpu`, `0`, `1`. Unsupported backends/tasks fail through the native runtime. |
| `family`, `config_id`, `weight_id`, `model_spec_override` | Optional upstream model selection. |
| `implementation` | Optional upstream session implementation name. |
| `load_options`, `session_options` | Upstream string-to-string maps. |
| `request_options` | Default per-request scalars or string lists. A request's `options` overrides matching keys. |

`schema` returns generated configuration/audio schemas and the operation vocabulary. `model_info` returns the loaded model's declared options, languages and task/mode support. Runtime introspection, rather than a fixed model allowlist, controls task coverage.

## Model-management protocol

The host-pinned native ABI has no typed control-bus RPC reply hook. Management uses **JSON-RPC envelopes on the node data input/output**, not the host's typed `call_rpc` proxy. Send `RuntimeData::Json` containing:

```json
{"jsonrpc":"2.0","id":1,"method":"download","params":{"model":"pulsevad_2_1k_f32"}}
```

Replies preserve `id` and contain either `result` or `error`. Operations:

The standalone host waits for `download` completion before exiting. Live job IDs and loaded models belong to the running pipeline; a new host process does not resume a previous process's live jobs. CLI JSON arguments can be `@filename.json` to avoid shell escaping and command-length limits. `AUDIOCPP_CACHE_DIR` overrides the standalone host's cache directory.

| Method | Parameters/result |
|---|---|
| `schema`, `capabilities` | Schemas, or runtime version/families/tasks. |
| `catalog`, `installed` | Full catalog or only completed installations, plus local imports. |
| `inspect` | `model`: catalog package metadata without loading weights. |
| `import` | `model` beginning with `local:`, and `path`; registers an existing file/directory by reference. |
| `download` | `model`: upstream package ID; returns `job_id`. Repeated active requests share the job. |
| `status`, `cancel` | `job_id`; status reports byte counts and running/completed/failed/cancelled state. Cancellation is cooperative. |
| `load`, `model_info` | `model`, optional selection fields and `load_options`; loads and describes a model. |
| `loaded` | Loaded model keys and active session count. |
| `unload` | Same model selection/load options; rejects models held by active streams. |
| `remove` | `model`; requires it to be unloaded and its download inactive. Imported source files are preserved. |

Downloads run on native background threads and require explicit requests. Install metadata records the resolved upstream revision. A completed installation marker is required before a downloaded package is admitted. The cache has a process lock; another host must use a different cache directory. Credentials use upstream's environment support and are never placed in these request examples. Download errors redact URLs and Hugging Face tokens.

Nodes with the same library/cache directories share loaded models. Models remain cached until `unload` or the last node is dropped. Native libraries remain loaded for the process lifetime because upstream runtime globals are unsafe to unload/reload repeatedly; models, sessions and download jobs are released normally.

## Inference and streaming

Plain `RuntimeData::Text` and `RuntimeData::Audio` become text and PCM requests. Structured JSON supports `request_id`, `text`, `language` (transcript language), `audio`, `voice_audio`, `voice_id`, `style_language`, `emotion`, `speaking_rate`, `pitch_shift`, `energy_scale`, `style_tags`, `artifacts`, and `options`.

Audio is `{ "samples": [0.0], "sample_rate": 16000, "channels": 1 }`, with interleaved finite f32 samples. Artifacts contain `kind` (the upstream numeric enum 0–8), `id`, byte-array `payload`, and optional string metadata. Request options can be scalar strings/numbers/bools or string lists. Set a model's language option separately in `options` when required by its declared schema.

Offline requests produce audio/text and a metadata result carrying segments, word timestamps, speaker turns, named outputs and artifacts. An offline node keeps its native session between requests (released on `reset`, on error, or when the node is dropped), so per-session caches such as IndexTTS's prepared speaker reference are reused and the session's graph arenas are allocated once; while the node holds it, `unload` of that model is rejected like for an active stream. The streaming callback receives each emission as it arrives. The single-output `process` interface returns all emissions in an `outputs` array when necessary, serialized as `RuntimeData` values; it never drops intermediate outputs.

For streaming audio input, send `operation: start`, then `push` with each audio chunk, then `finish`. A first `push` can implicitly start the stream. `poll` drains queued native events, and `reset` releases the session. Text-only streaming defaults to `generate`, which starts, drains events and finishes in one request. Explicit `start` leaves the stream open until `finish`/`reset`. A model that lacks the requested streaming mode fails explicitly.

Each stream starts with a `stream_policy` metadata result reporting native input/output kinds and preferred chunk sizes. It keeps its initial `request_id`; subsequent chunks stay correlated with that ID. Each node has its own default stream; set `session_id` on structured requests to multiplex isolated streams through one node. This explicit field matters because the pinned loadable ABI does not forward host session IDs to per-frame calls. Finish results may repeat the complete waveform: `audio_already_streamed` prevents duplicate PCM playback while retaining the waveform in final JSON. Named outputs retain native names; generated `chunk_*` names stay in metadata rather than becoming separate playback tracks. There is no audio resampling or invented streaming fallback in the plugin.

The worker queues are bounded (16 commands, 8 result envelopes). One worker serializes native handle access per library/cache pair. Slow inference can therefore delay management calls; downloads themselves run independently. Use separate caches/workers for independent inference concurrency. Dropping a request receiver stops delivery and cleans up its session after the current native call returns; native blocking kernels are not force-interrupted.

## Tests

```powershell
cargo test
cargo build
$env:AUDIOCPP_NATIVE_DIR = (Resolve-Path native/build-cpu/bin/Release).Path
cargo test --test abi -- --ignored --nocapture --test-threads=1
# For the CUDA speech smoke, use the CUDA native directory and:
$env:AUDIOCPP_TEST_BACKEND = 'cuda'
cargo test --test abi native_speech -- --ignored --nocapture
```

The ignored tests deliberately download real models; ordinary tests remain offline. `AUDIOCPP_PLUGIN` can select a different built plugin DLL. See `VALIDATION.md` for actual execution evidence and remaining coverage.
