# Native Android audio.cpp

The native C API and CLI run on a Pixel 6 Pro (Google Tensor, ARM64, Android 16 / API 36).
This is an on-device native execution test, not an APK or remote-runner integration.

## Build

The Windows build needs CMake, Ninja and the Android NDK. The tested NDK is
Google's r30, revision `30.0.16248370`. Obtain it from the
[official NDK downloads](https://developer.android.com/ndk/downloads).

```powershell
./build-android.ps1 -NdkDirectory D:/path/to/android-ndk-r30 `
  -SourceDirectory D:/path/to/audio.cpp `
  -ArmArchitecture 'armv8.2-a+dotprod+fp16' -Jobs 12
```

`-SourceDirectory` is optional: the script uses `.cache/audiocpp-source` in this repository when present;
otherwise the native CMake project fetches the pinned source. The current pin is
`06c13737cc39481787a03465697b58b1c954b56f`. The vendored C header must match.
`-NdkDirectory` defaults to `ANDROID_NDK_HOME`, then `.cache/android-tools/android-ndk-r30` in this repository.

The default architecture is portable `armv8-a`. The example above explicitly
enables dot-product and FP16 instructions confirmed in the Pixel's CPU features.
Do not select that architecture for devices which lack those instructions.

The default model selection is PocketTTS, IndexTTS2/2.5, PulseVAD and Moonshine;
the upstream core also includes Silero VAD. `-Models` accepts the upstream
comma-separated model target names. Compiling a model does not certify its
execution. Android API 28 is the default minimum native API.

Output in `dist/android-arm64-cpu`:

- `libaudiocpp.so`: native inference C API.
- `libaudiocpp_manager_bridge.so`: native model-manager bridge.
- `audiocpp_cli`, `audiocpp_model_manager`: native command-line tools.
- `audiocpp_c_api_path_test`: upstream C ABI inference/ownership test.
- `libc++_shared.so`: required Android C++ runtime.
- `build-info.json`: target, source revision, NDK, model selection and artifact hashes.

These are Android ELF binaries. The build adds `liblog` to the runtime's link
dependencies because SentencePiece's bundled protobuf logger uses Android's
logging API. Both engine and ggml OpenMP are disabled, so the package does not
require an additional `libomp.so`. ggml still uses its own CPU worker threads.

## Device smoke test

Enable USB or wireless debugging and authorize/pair ADB. Use the exact device
identifier shown by `adb devices -l`; the script requires explicit selection.
Wireless pairing and connection use separate ports.

```powershell
./test-android.ps1 -Device 'PHONE_IP:DEBUG_PORT'
```

Supply `-Adb`, `-SourceDirectory` or `-DistDirectory` to override the defaults
(this repository's `.cache/` and `dist/android-arm64-cpu`). The test installs files only under
`/data/local/tmp/remotemedia-audiocpp`. It does not install an APK, alter system
settings, or remove existing application data. It leaves its test files/models
there for reuse. Logs and exit codes are saved under
`dist/android-arm64-cpu/device-results`.

The test runs the C API path test with the bundled Silero model, installs the
small PulseVAD GGUF through the native HTTPS model manager, and runs inference
using the downloaded model. A native nonzero exit status fails the script.

Android's trusted CA files are exported into a PEM bundle inside the test
directory and selected through `SSL_CERT_FILE`. The first attempt without this
configuration failed certificate verification because BoringSSL's desktop
defaults did not find the Android trust store. Certificate and hostname
verification remain enabled. An APK integration must supply an Android trust
store to its native download client too; this shell export is a test setup.

For the avatar's IndexTTS model, additionally provide its local GGUF and a short
PCM voice-reference WAV. This step also needs Python 3 for output validation:

```powershell
./test-android.ps1 -Device 'PHONE_IP:DEBUG_PORT' `
  -IndexTtsModel 'C:/path/to/index-tts2_5-q8_0.gguf' `
  -VoiceReference 'C:/path/to/three-second-reference.wav'
```

The model is copied with ADB's `--sync`; no second local copy is made. The test
uses `clon`, English, four CPU threads, one beam, seed 42, 128 maximum tokens,
and `index_tts2.mem_saver=true`. The prompt is "Hello from Android." A 240-second
timeout bounds the process (`-TimeoutSeconds` overrides it). Generated audio is
pulled back and checked for nonempty, finite, non-silent PCM16. Its duration,
level and elapsed-time ratio are saved beside the WAV. This is not a speech
quality or voice-similarity certification.

## Observed results, 2026-11-02

| Check | Result |
| --- | --- |
| NDK cross-compilation | Passed: native ARM64 libraries and executables |
| Native C ABI path test | Passed: Silero speech detection, streaming events, session reuse and ABI ownership checks |
| PulseVAD GGUF inference | Passed: speech segments from a real WAV |
| Native model download | Passed with the phone's trusted CA bundle; install marker created |
| IndexTTS 2.5 Q8_0 | Passed: generated an audio file on the phone CPU |

The reproducible scripted IndexTTS run used the cached 3,502,960,096-byte GGUF and a
three-second reference trimmed from upstream `sample_16k.wav`. It generated
50,944 mono samples at 22,050 Hz (2.310 seconds), with normalized peak 0.99997 and
RMS 0.1432. The script's wall-clock measurement was **95.38 seconds**, including
ADB overhead, model load and reference preparation: approximately **41.28 times
the generated audio duration**. This is a fresh-process measurement, not a steady-state
benchmark, and is unsuitable for interactive avatar speech on this CPU path.

The device had about 6 GB available RAM and 39 GB free storage before the run.
Peak process memory and sustained thermal behavior have not been measured.

## Remaining integration

- Vulkan/GPU execution and performance on Tensor/Mali are untested.
- PocketTTS and Moonshine are compiled but were not exercised on this device.
- The Rust loadable plugin ABI and typed management RPC are not exercised by
  this native C ABI test. Their Android rebuild/integration remains separate.
- No Android application service, APK packaging, desktop offload, or scaling
  runner is implemented by these scripts.
- Android linker namespaces, app lifecycle and app-owned model storage must be
  validated inside the eventual APK; an ADB-shell run does not establish them.
