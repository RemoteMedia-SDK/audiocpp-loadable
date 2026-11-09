param(
    [Parameter(Mandatory = $true)][string]$Device,
    [string]$Adb = '',
    [string]$SourceDirectory = '',
    [string]$DistDirectory = '',
    [string]$DeviceDirectory = '/data/local/tmp/remotemedia-audiocpp',
    [string]$IndexTtsModel = '',
    [string]$VoiceReference = '',
    [ValidateRange(30, 600)][int]$TimeoutSeconds = 240
)
$ErrorActionPreference = 'Stop'
if ($DeviceDirectory -notmatch '^/data/local/tmp/remotemedia-audiocpp(?:-[a-zA-Z0-9_-]+)?$') {
    throw 'DeviceDirectory must be /data/local/tmp/remotemedia-audiocpp with an optional simple suffix.'
}
$cacheRoot = Join-Path $PSScriptRoot '.cache'
if (!$Adb) { $Adb = Join-Path $cacheRoot 'android-tools/platform-tools/adb.exe' }
if (!$SourceDirectory) { $SourceDirectory = Join-Path $cacheRoot 'audiocpp-source' }
if (!$DistDirectory) { $DistDirectory = Join-Path $PSScriptRoot 'dist/android-arm64-cpu' }
$SourceDirectory = (Resolve-Path -LiteralPath $SourceDirectory).Path
$DistDirectory = (Resolve-Path -LiteralPath $DistDirectory).Path
$resultsDirectory = Join-Path $DistDirectory 'device-results'
New-Item -ItemType Directory -Force -Path $resultsDirectory | Out-Null

function Invoke-Adb([string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    & $Adb -s $Device @Arguments 2>&1 | ForEach-Object { $_.ToString() }
    if ($LASTEXITCODE -ne 0) { throw "ADB failed with exit code $LASTEXITCODE" }
}

function Invoke-DeviceTest([string]$Name, [string]$Command) {
    $ErrorActionPreference = 'Continue'
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $output = & $Adb -s $Device shell "cd $DeviceDirectory && export LD_LIBRARY_PATH=$DeviceDirectory && $Command" 2>&1
    $exitCode = $LASTEXITCODE
    $timer.Stop()
    $output | ForEach-Object { $_.ToString() } | Set-Content -Encoding UTF8 (Join-Path $resultsDirectory "$Name.log")
    [ordered]@{ test = $Name; exit_code = $exitCode; adb_wall_seconds = $timer.Elapsed.TotalSeconds } |
        ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $resultsDirectory "$Name.json")
    if ($exitCode -ne 0) { throw "$Name failed ($exitCode); see $resultsDirectory/$Name.log" }
    Write-Host "$Name passed ($([math]::Round($timer.Elapsed.TotalSeconds, 2)) seconds including ADB)"
    return $timer.Elapsed.TotalSeconds
}

Invoke-Adb @('get-state')
Invoke-Adb @('shell', "mkdir -p $DeviceDirectory/models")
foreach ($file in @('libaudiocpp.so', 'libaudiocpp_manager_bridge.so', 'libc++_shared.so',
        'audiocpp_cli', 'audiocpp_model_manager', 'audiocpp_c_api_path_test')) {
    Invoke-Adb @('push', '--sync', (Join-Path $DistDirectory $file), "$DeviceDirectory/$file")
}
Invoke-Adb @('push', '--sync', (Join-Path $SourceDirectory 'assets/framework/models/silero_vad'), "$DeviceDirectory/models/")
Invoke-Adb @('push', '--sync', (Join-Path $SourceDirectory 'assets/resources/sample_16k.wav'), "$DeviceDirectory/sample_16k.wav")
Invoke-Adb @('shell', "chmod 755 $DeviceDirectory/audiocpp_cli $DeviceDirectory/audiocpp_model_manager $DeviceDirectory/audiocpp_c_api_path_test")
$null = Invoke-DeviceTest 'device' 'getprop ro.product.model; getprop ro.build.version.release; getprop ro.product.cpu.abilist; getconf PAGESIZE; head -5 /proc/meminfo'
$null = Invoke-DeviceTest 'c-api' 'timeout 120 ./audiocpp_c_api_path_test models/silero_vad sample_16k.wav'
# BoringSSL's desktop trust-store defaults do not locate Android's CA store.
# Use the device's trusted roots; certificate and hostname verification stay on.
$certificates = 'if [ -d /apex/com.android.conscrypt/cacerts ]; then cat /apex/com.android.conscrypt/cacerts/* > android-ca-bundle.pem; else cat /system/etc/security/cacerts/* > android-ca-bundle.pem; fi'
$null = Invoke-DeviceTest 'trust-store' $certificates
$null = Invoke-DeviceTest 'model-install' "SSL_CERT_FILE=$DeviceDirectory/android-ca-bundle.pem timeout 90 ./audiocpp_model_manager install pulsevad_2_1k_f32 --models-dir download-test"
$null = Invoke-DeviceTest 'gguf-vad' 'timeout 60 ./audiocpp_cli --task vad --family pulsevad --model download-test/PulseVAD-GGUF/pulsevad-2.1k-f32.gguf --backend cpu --threads 4 --audio sample_16k.wav'

if ($IndexTtsModel) {
    if (!$VoiceReference) { throw 'Supply -VoiceReference with -IndexTtsModel (a short PCM WAV).' }
    Invoke-Adb @('push', '--sync', (Resolve-Path -LiteralPath $IndexTtsModel).Path, "$DeviceDirectory/models/index-tts2_5-q8_0.gguf")
    Invoke-Adb @('push', (Resolve-Path -LiteralPath $VoiceReference).Path, "$DeviceDirectory/reference.wav")
    $seconds = Invoke-DeviceTest 'indextts' "timeout $TimeoutSeconds ./audiocpp_cli --task clon --family index_tts2 --model models/index-tts2_5-q8_0.gguf --backend cpu --threads 4 --language en --text 'Hello from Android.' --voice-ref reference.wav --num-beams 1 --max-tokens 128 --seed 42 --session-option index_tts2.mem_saver=true --out indextts-android.wav"
    $wav = Join-Path $resultsDirectory 'indextts-android.wav'
    Invoke-Adb @('pull', "$DeviceDirectory/indextts-android.wav", $wav)
    @'
import json, math, struct, sys, wave
from pathlib import Path
path = Path(sys.argv[1])
with wave.open(str(path), 'rb') as audio:
    frames, rate, channels = audio.getnframes(), audio.getframerate(), audio.getnchannels()
    assert frames > 0 and audio.getsampwidth() == 2, 'Expected nonempty PCM16 audio'
    samples = struct.unpack('<' + 'h' * (frames * channels), audio.readframes(frames))
peak = max(abs(x) for x in samples) / 32768
rms = math.sqrt(sum((x / 32768) ** 2 for x in samples) / len(samples))
assert peak > 0 and math.isfinite(rms), 'Audio must be finite and non-silent'
seconds = float(sys.argv[2])
metrics = dict(frames=frames, sample_rate=rate, channels=channels, seconds=frames / rate,
               peak=peak, rms=rms, adb_wall_seconds=seconds, real_time_factor=seconds / (frames / rate))
path.with_suffix('.metrics.json').write_text(json.dumps(metrics, indent=2) + '\n')
print(json.dumps(metrics, indent=2))
'@ | python - $wav $seconds.ToString([Globalization.CultureInfo]::InvariantCulture)
    if ($LASTEXITCODE -ne 0) { throw 'Generated WAV validation failed' }
}
Write-Output "Device evidence: $resultsDirectory"
