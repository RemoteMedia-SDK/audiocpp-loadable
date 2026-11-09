"""Run audio.cpp Qwen3-TTS voice cloning on a paired Android device, retaining evidence."""
import argparse
import hashlib
import json
import pathlib
import re
import shlex
import subprocess
import time
import wave

ROOT = pathlib.Path(__file__).resolve().parent
CASES = [
    "Hold this position until the convoy gets through. Nobody moves without my signal.",
    "I've seen a lot of good soldiers go down in places like this. Don't be one of them.",
    "Alright, listen up. We breach on three, clear the stairs, and regroup on the roof.",
    "You did well out there today. Get some rest, because tomorrow is going to be a lot worse.",
]


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run_native(adb, remote, args, results, model_hash):
    """Keep inference and its logs on-device even if wireless ADB disconnects."""
    destination = f"{remote}/{args.name}"
    adb("shell", "mkdir", destination)
    voice_args = "reference.wav reference.txt"
    mode_arg = ""
    precompute = ""
    if args.voice_mode == "precomputed":
        voice_args = f"{args.name}/voice.json {model_hash}"
        mode_arg = " precomputed"
        if not args.speaker_embedding:
            precompute = (f"timeout 120 ./qwen3_android_bench --export-voice model.gguf reference.wav "
                          f"{destination}/voice.json {model_hash} > {destination}/precompute.log 2>&1\n"
                          f"code=$?\nif [ \"$code\" -ne 0 ]; then printf '%s\\n' \"$code\" > "
                          f"{destination}/exit-code; exit \"$code\"; fi\n")
    elif args.voice_mode == "x-vector":
        mode_arg = " x-vector"
    script = (f"#!/system/bin/sh\ncd {remote} || exit 1\n"
              f"export LD_LIBRARY_PATH={remote} ENGINE_TIMING_ENABLED=1\n"
              f"{precompute}"
              f"timeout {args.timeout} ./qwen3_android_bench model.gguf {voice_args} cases.txt "
              f"{args.name}{mode_arg} > {destination}/benchmark.log 2>&1\n"
              f"code=$?\nprintf '%s\\n' \"$code\" > {destination}/exit-code\n")
    script_path = results / "run.sh"
    script_path.write_text(script, encoding="utf-8", newline="\n")
    adb("push", script_path.resolve(), f"{destination}/run.sh")
    if args.speaker_embedding:
        adb("push", args.speaker_embedding.resolve(), f"{destination}/voice.json")
    started = time.monotonic()
    adb("shell", f"nohup sh {shlex.quote(destination + '/run.sh')} </dev/null >/dev/null 2>&1 &")
    last_progress = ""
    while time.monotonic() - started < args.timeout + 180:
        status = adb("shell", f"cat {destination}/exit-code 2>/dev/null", check=False)
        if status.returncode == 0 and status.stdout.strip().isdigit():
            return int(status.stdout.strip()), time.monotonic() - started
        progress = adb("shell", f"tail -n 1 {destination}/benchmark.log", check=False)
        if progress.returncode == 0 and progress.stdout != last_progress:
            last_progress = progress.stdout
            print(last_progress.strip(), flush=True)
        time.sleep(10)
    # The native timeout still bounds the process if the host lost its connection.
    return None, time.monotonic() - started


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", required=True)
    parser.add_argument("--model", type=pathlib.Path, required=True)
    parser.add_argument("--reference-wav", type=pathlib.Path)
    parser.add_argument("--reference-text", type=pathlib.Path)
    parser.add_argument("--voice-mode", choices=["full-reference", "x-vector", "precomputed"], default="full-reference")
    parser.add_argument("--speaker-embedding", type=pathlib.Path,
                        help="Reuse an exported voice.json; no reference recording is needed")
    parser.add_argument("--adb", type=pathlib.Path, default=ROOT.parents[1] / ".cache/android-tools/platform-tools/adb.exe")
    parser.add_argument("--dist", type=pathlib.Path, default=ROOT / "dist/android-arm64-qwen3")
    parser.add_argument("--name", default=time.strftime("qwen3-%Y%m%d-%H%M%S"))
    parser.add_argument("--case-limit", type=int, choices=range(1, 5), default=4)
    parser.add_argument("--repeat-first", action="store_true")
    parser.add_argument("--timeout", type=int, default=1200)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.name) or not 30 <= args.timeout <= 3600:
        parser.error("Use a simple run name and a timeout between 30 and 3600 seconds")
    if args.speaker_embedding and args.voice_mode != "precomputed":
        parser.error("--speaker-embedding requires --voice-mode precomputed")
    if not args.speaker_embedding and not args.reference_wav:
        parser.error("Supply --reference-wav to compute a voice, or --speaker-embedding to reuse one")
    if args.voice_mode != "precomputed" and not args.reference_text:
        parser.error("The reference-based modes require --reference-text")
    results = args.dist / "device-results" / args.name
    results.mkdir(parents=True, exist_ok=False)
    reference_text = " ".join(args.reference_text.read_text(encoding="utf-8-sig").split()) if args.reference_text else ""
    if args.voice_mode != "precomputed" and not reference_text:
        raise ValueError("Reference transcript is empty")
    cases = CASES[:args.case_limit]
    if args.repeat_first:
        cases = cases + [cases[0]]
    (results / "reference.txt").write_text(reference_text + "\n", encoding="utf-8")
    (results / "cases.txt").write_text("\n".join(cases) + "\n", encoding="utf-8")
    remote = "/data/local/tmp/remotemedia-qwen3"

    def adb(*arguments, check=True):
        result = subprocess.run([str(args.adb), "-s", args.device, *map(str, arguments)],
                                capture_output=True, text=True, encoding="utf-8", errors="replace")
        if check and result.returncode:
            raise RuntimeError(result.stderr or result.stdout)
        return result

    adb("get-state")
    active = adb("shell", "pidof", "qwen3_android_bench", check=False)
    if active.returncode == 0 and active.stdout.strip():
        raise RuntimeError(f"Another Qwen benchmark is still running: {active.stdout.strip()}")
    adb("shell", "mkdir", "-p", remote)
    facts = adb("shell", "getprop ro.product.model; getprop ro.build.version.sdk; head -6 /proc/meminfo; df -h /data")
    (results / "device-before.txt").write_text(facts.stdout, encoding="utf-8")
    sources = [(args.dist / "qwen3_android_bench", "qwen3_android_bench"),
               (args.dist / "libc++_shared.so", "libc++_shared.so"),
               (args.model, "model.gguf"), (results / "cases.txt", "cases.txt")]
    if not args.speaker_embedding:
        sources += [(args.reference_wav, "reference.wav"), (results / "reference.txt", "reference.txt")]
    for source, target in sources:
        print(f"Copying {source.name} ({source.stat().st_size:,} bytes)", flush=True)
        adb("push", "--sync", source.resolve(), f"{remote}/{target}")
    adb("shell", "chmod", "755", f"{remote}/qwen3_android_bench")
    model_hash = sha256(args.model)
    remote_hash = adb("shell", "sha256sum", f"{remote}/model.gguf").stdout.split()[0]
    if remote_hash != model_hash:
        raise RuntimeError("Transferred GGUF checksum does not match")
    if args.speaker_embedding:
        artifact = json.loads(args.speaker_embedding.read_text(encoding="utf-8"))
        if artifact.get("model_sha256") != model_hash:
            raise RuntimeError("Speaker embedding was exported for a different model")
    manifest = dict(model_sha256=model_hash,
                    reference_sha256=sha256(args.reference_wav) if args.reference_wav else None,
                    voice_mode=args.voice_mode,
                    reused_embedding_sha256=sha256(args.speaker_embedding) if args.speaker_embedding else None,
                    cases=cases, seed=42, max_new_tokens=256, backend="cpu", threads=4,
                    build=json.loads((args.dist / "build-info.json").read_text(encoding="utf-8-sig")))
    (results / "inputs.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Running {len(cases)} requests; native logs persist under {remote}/{args.name}", flush=True)
    exit_code, wall = run_native(adb, remote, args, results, model_hash)
    pull = adb("pull", f"{remote}/{args.name}", results / "audio", check=False)
    (results / "pull.log").write_text(pull.stdout + pull.stderr, encoding="utf-8")
    events = []
    benchmark_log = results / "audio/benchmark.log"
    log_text = benchmark_log.read_text(encoding="utf-8", errors="replace") if benchmark_log.exists() else ""
    for line in log_text.splitlines():
        if line.startswith('{"phase":'):
            events.append(json.loads(line))
    clips = []
    for path in sorted((results / "audio").glob("*.wav")):
        with wave.open(str(path)) as clip:
            clips.append(dict(file=path.name, seconds=clip.getnframes() / clip.getframerate(),
                              sample_rate=clip.getframerate(), channels=clip.getnchannels(), sha256=sha256(path)))
    report = dict(exit_code=exit_code, native_exit_confirmed=exit_code is not None,
                  host_observed_seconds=wall, polling_interval_seconds=10,
                  events=events, clips=clips)
    precompute_log = results / "audio/precompute.log"
    report["precompute_events"] = []
    if precompute_log.exists():
        for line in precompute_log.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith('{"phase":'):
                report["precompute_events"].append(json.loads(line))
    embedding_path = results / "audio/voice.json"
    if embedding_path.exists():
        report["speaker_embedding"] = dict(bytes=embedding_path.stat().st_size, sha256=sha256(embedding_path))
    (results / "summary.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2), flush=True)
    if exit_code != 0 or pull.returncode or len(clips) != len(cases):
        raise RuntimeError(f"Incomplete benchmark; retained evidence in {results}")
    print(f"Evidence: {results}")


if __name__ == "__main__":
    main()
