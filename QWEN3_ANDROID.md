# Qwen3-TTS on Android

This test runs the same `qwen3_tts_1_7b_base_q8_0` voice-cloning model and four
sentences used by the desktop `compare_tts.py` example. It uses audio.cpp's native
C++ runtime on the phone CPU, with a single reusable model/session. The benchmark
follows the upstream `tests/qwen3_tts/qwen3_tts_warm_bench.cpp` interfaces and WAV
utilities. It does not run the Rust loadable-plugin host or install an APK.

## Reproduce

Build with the cached NDK r30 and pinned audio.cpp source:

```powershell
& build-android.ps1 `
  -Models 'pocket_tts,index_tts2,pulsevad,moonshine_asr,qwen3_tts' `
  -ArmArchitecture 'armv8.2-a+dotprod+fp16' -QwenBenchmark `
  -DistDirectory dist/android-arm64-qwen3

python test-qwen3-android.py `
  --device 'YOUR_ADB_SERIAL_OR_IP:PORT' `
  --model 'D:/audiocpp-cache-compare/Qwen3-TTS-12Hz-1.7B-Base-GGUF/qwen3-tts-12hz-1.7b-base-q8_0_v2.gguf' `
  --reference-wav 'D:/cc-render/avatars/reece_head_arkit.voice.wav' `
  --reference-text 'D:/cc-render/avatars/reece_head_arkit.voice.txt' `
  --repeat-first --name qwen3-smoke-01
```

Python 3.11+ is required; the test harness uses only the standard library. Use the
architecture flag only on devices supporting ARM dot-product and FP16 instructions.
General build prerequisites and NDK overrides are in [ANDROID.md](ANDROID.md).
`-DistDirectory` keeps this model-specific package separate from the previous
Android build. `-BuildDirectory` can also select an independent build cache.

Each run uses a new name and stores evidence under
`dist/android-arm64-qwen3/device-results/<name>/`. Existing names fail instead of
overwriting evidence. `--case-limit 1` selects only the first sentence;
`--repeat-first` appends that sentence to measure another call in the same session.
`--timeout` bounds the entire native process (default 1,200 seconds).

The harness copies the GGUF, reference and binary to
`/data/local/tmp/remotemedia-qwen3`, verifies the transferred model's SHA-256,
then invokes the native benchmark. It preserves the full reference recording and
normalizes transcript whitespace as the desktop example does. No model download
or second local copy of the GGUF is required. Results and logs remain on the phone
and are pulled to the desktop, including partial evidence from failed runs.
The native job runs detached from ADB, writes its own log and exit-status file,
and is polled every ten seconds. A wireless transport interruption does not close
the benchmark's output pipe or terminate synthesis. A missing exit status is
reported as unknown, not as a native engine failure.

## Measurement contract

Configuration: Qwen3-TTS 1.7B Base Q8_0, CPU, four threads, English, seed 42,
offline voice cloning. Sampling and session options retain upstream defaults.
The test adds a 256-new-token bound; the desktop comparison did not specify that
bound. An utterance reaching it must be checked for truncation.

`audio/benchmark.log` contains JSON events with these separately measured phases:

- `model_load`: registry creation and model loading.
- `session_create`: CPU runtime/session construction.
- `prepare`: the explicit preparation hook. In this pinned Qwen session it only
  marks the session prepared; reference encoding occurs during the first `run`.
- `synthesis`: each call to `run`, excluding those explicit setup phases and WAV
  writing. The first call includes reference encoding and graph/cache setup.
- `total`: the whole native sequence from registry creation through output writes.

`audio/engine-timing.log` contains audio.cpp's internal reference-prompt, talker
and speech-decoder timings. These explain first-call versus reused-session costs;
the full request time must not be labeled pure neural generation. ADB copying,
checksum verification and desktop WAV inspection are outside native timings.
`host_observed_seconds` covers invocation, ADB overhead and polling delay, excluding
copying; use native phase timings for performance comparisons.

Each completed request writes a PCM16 WAV immediately and reports duration,
sample rate, channels, float peak/RMS, synthesis real-time factor, and process
peak RSS. RSS is the cumulative high-water mark, not incremental request memory.
Audio must be nonempty, finite and non-silent. Those checks establish successful
native audio output; they do not establish intelligibility or voice similarity.

The first line is repeated with the same seed when requested. Identical WAV hashes
would establish repeatability for that device/build/configuration only. CPU and
CUDA outputs need not be bit-identical.

## Results

Validated on the Pixel 6 Pro / Google Tensor / Android 16 (API 36), 2026-11-03 UTC.
All four comparison sentences and the repeated first sentence completed with native
exit code 0. The model checksum matched after transfer. Each result is nonempty,
finite, non-silent, mono PCM16 at 24 kHz. The full 14.3747-second, 48 kHz Reece
reference was used without trimming.

Model loading took **0.276 seconds**, session creation **1.857 seconds**: **2.133
seconds total setup**. Per-request explicit preparation was 0.85-2.94 ms. These
are fresh-process timings with cached local model storage, not cold-filesystem
measurements.

| Request | Output duration | Synthesis, excluding setup | Real-time factor |
| --- | ---: | ---: | ---: |
| Hold this position... | 4.32 s | 69.79 s | 16.16 |
| I've seen a lot... | 4.08 s | 69.49 s | 17.03 |
| Alright, listen up... | 3.68 s | 73.77 s | 20.05 |
| You did well... | 4.56 s | 86.36 s | 18.94 |
| Repeat of first line | 4.32 s | 86.80 s | 20.09 |

Peak process RSS was **5.52 GiB**. The complete native sequence took 388.384
seconds. Reference-prompt work fell from 5.468 seconds on the first call to
4-9 ms on subsequent calls, confirming reuse. Speech decoding took 38.13-58.01
seconds per call and dominated much of the runtime. The later repeated line was
slower despite producing an **identical WAV SHA-256**. Only a post-run thermal
snapshot was collected; the cause of that slowdown has not been established.

The Android on-device STT runner transcribed the four distinct generated WAVs
after the timed benchmark finished. Lines 1 and 4 matched after punctuation/case
normalization. Line 2 had an extra trailing "of"; line 3 used "All right" and
recognized "breach" as "preach". These are ASR observations, not proof of which
component introduced a difference. Voice similarity has not been scored, and no
human listening assessment is claimed.

An earlier attached-ADB attempt lost its connection, but its first WAV was recovered
from the phone afterward (71.14 seconds synthesis). Its incomplete evidence is kept
under `reece-full-reference`; the successful detached run is `reece-detached`.
The harness now keeps execution/logging on-device and rejects concurrent runs before
overwriting active inputs; that rejection was tested while the benchmark was running.

The exact build/model hashes, phase timings and recognized text are retained in
[qwen3-android-validation.json](qwen3-android-validation.json). Full logs and WAVs
are in `dist/android-arm64-qwen3/device-results/reece-detached/`.

Qwen3-TTS works natively on this Android CPU. At **16-20 times the audio duration**,
this configuration is unsuitable for interactive avatar speech. Vulkan, smaller
models and alternative quantizations were not tested in this run.

## Saved speaker embeddings (1.7B)

The benchmark also supports exporting Reece's speaker embedding once and loading
it in a fresh process. The model remains **1.7B Base Q8_0**, with the same CPU,
four threads and seed 42. This changes voice conditioning: speaker-only mode omits
the reference transcript and speech codes, including their repeated decoding cost.
It may change voice similarity, cadence and generated duration.

Build into a separate output folder using the command above, adding
`-DistDirectory dist/android-arm64-qwen3-precomputed`.
Then export the voice on Android and benchmark it in a separate process:

```powershell
python test-qwen3-android.py `
  --device '192.168.18.60:39563' `
  --model 'D:/audiocpp-cache-compare/Qwen3-TTS-12Hz-1.7B-Base-GGUF/qwen3-tts-12hz-1.7b-base-q8_0_v2.gguf' `
  --reference-wav 'D:/cc-render/avatars/reece_head_arkit.voice.wav' `
  --voice-mode precomputed --case-limit 4 --repeat-first `
  --dist dist/android-arm64-qwen3-precomputed `
  --name reece-precomputed
```

To reuse the saved file, use a new run name and replace `--reference-wav` with:

```powershell
--speaker-embedding dist/android-arm64-qwen3-precomputed/device-results/reece-precomputed/audio/voice.json
```

No reference WAV or transcript is read or sent by the reuse path. Model weights
must still be loaded. `precompute.log` records model loading, speaker encoder
creation, embedding computation and total export time. `benchmark.log` separately
records model loading, saved-embedding loading, runtime creation and synthesis.
The float32 embedding survives JSON serialization exactly; export verifies the
round trip. The host verifies the GGUF SHA-256 on both machines, and the native
loader checks the supplied model identity, upstream revision, dimensions and
finite values. This is a local benchmark artifact format, not a portable voice
format shared with other Qwen implementations.

The pinned audio.cpp session has no public saved-embedding input. The experimental
adapter in `native/qwen3_precomputed_voice.h` therefore uses its existing C++
speaker encoder, tokenizer, talker and speech decoder directly, matching default
session performance settings. It does not instantiate reference encoders during
synthesis. No audio.cpp source patch or plugin ABI change is required. Use
`--voice-mode x-vector` with the reference WAV/transcript to cross-check the
upstream session's `x_vector_only_mode` against the saved-embedding adapter.

The earlier full-reference run requested a 256-token cap using `max_new_tokens`,
but this pinned session reads `max_tokens`. That cap was not applied in the
historical run. The harness now uses the correct key; the precomputed adapter
sets its generation structure's 256-token cap directly.

### Saved-embedding results

On the same Pixel 6 Pro, exporting the 2,048-dimensional Reece embedding took
**2.147 seconds total**, including **2.014 seconds of encoder computation**.
The JSON file is **27,262 bytes** (8,192 bytes of float32 values before encoding).
Loading it took **0.673 ms**. Model loading and runtime creation were separately
measured at **0.074 s** and **1.796 s**, respectively.

| Request | Output duration | Synthesis, excluding setup | Real-time factor |
| --- | ---: | ---: | ---: |
| Hold this position... | 5.52 s | 39.07 s | 7.08 |
| I've seen a lot... | 5.12 s | 34.97 s | 6.83 |
| Alright, listen up... | 5.28 s | 38.00 s | 7.20 |
| You did well... | 5.76 s | 41.23 s | 7.16 |
| Repeat of first line | 5.52 s | 39.69 s | 7.19 |

All five outputs were finite, non-silent mono PCM16 at 24 kHz. Peak process RSS
was **3.35 GiB**, compared with **5.52 GiB** for the historical full-reference run.
The talker took **25.12-29.68 s** per request and the decoder **9.84-11.55 s**.
The saved-voice run completed with native exit code 0.

The first sentence, its warm repeat, a new process reusing the saved file without
any reference inputs, and the upstream session's embedding-only control all
produced SHA-256 `d3ae17dc2f1bd49e7be05204c1a16b1b42d162c8f567a08a8eeec39f8db279a5`.
The reload took **39.91 s**, with **0.775 ms** to load the embedding. The upstream
control took **44.89 s**, including fresh reference encoding. This establishes
serialization/reuse equivalence for this device, model and seed; it does not
establish equivalent voice quality to full reference conditioning.

Native checks rejected wrong-model, wrong-runtime, wrong-dimension and nonfinite
artifacts before synthesis. The model itself remains the original 1.7B Q8 GGUF.
The improvement combines speaker-only conditioning and saved-embedding reuse;
it is not a measurement of caching alone. The previous session already cached
its reference prompt in RAM. Runs were sequential, without controlled thermal
conditions. This configuration remains roughly seven times slower than real time.

Full evidence is under `dist/android-arm64-qwen3-precomputed/device-results/`:
`reece-precomputed`, `reece-precomputed-reload` and `reece-xvector-control`.
The first directory contains the reusable `audio/voice.json` and all five WAVs.
Build hashes, per-stage timings, artifact rejection checks and recognized text
are retained in [qwen3-precomputed-validation.json](qwen3-precomputed-validation.json).
Android's on-device STT recovered all four sentences, ignoring punctuation/case
and the spelling difference `Alright` versus `All right`. This is an automated
intelligibility check; no human listening or voice-similarity assessment is claimed.
