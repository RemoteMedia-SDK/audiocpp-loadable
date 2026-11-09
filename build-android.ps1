param(
    [string]$NdkDirectory = $env:ANDROID_NDK_HOME,
    [string]$SourceDirectory = '',
    [string]$BuildDirectory = '',
    [string]$DistDirectory = '',
    [string]$ArmArchitecture = 'armv8-a',
    [string]$Models = 'pocket_tts,index_tts2,pulsevad,moonshine_asr',
    [int]$AndroidApi = 28,
    [int]$Jobs = 8,
    [switch]$QwenBenchmark
)
$ErrorActionPreference = 'Stop'

function Invoke-Checked([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    & $Exe @Arguments 2>&1 | ForEach-Object { $_.ToString() }
    if ($LASTEXITCODE -ne 0) { throw "$Exe failed with exit code $LASTEXITCODE" }
}

$cacheRoot = Join-Path $PSScriptRoot '.cache'
if (!$NdkDirectory) { $NdkDirectory = Join-Path $cacheRoot 'android-tools/android-ndk-r30' }
$NdkDirectory = (Resolve-Path -LiteralPath $NdkDirectory).Path
$toolchain = Join-Path $NdkDirectory 'build/cmake/android.toolchain.cmake'
if (!(Test-Path -LiteralPath $toolchain)) { throw "Android NDK toolchain missing: $toolchain" }
if (!$SourceDirectory) {
    $cachedSource = Join-Path $cacheRoot 'audiocpp-source'
    if (Test-Path -LiteralPath "$cachedSource/CMakeLists.txt") { $SourceDirectory = $cachedSource }
}
if (!$BuildDirectory) { $BuildDirectory = Join-Path $PSScriptRoot 'native/build-android-arm64-cpu' }
$BuildDirectory = [IO.Path]::GetFullPath($BuildDirectory)
if (!$DistDirectory) { $DistDirectory = Join-Path $PSScriptRoot 'dist/android-arm64-cpu' }
$distDirectory = [IO.Path]::GetFullPath($DistDirectory)
if ($QwenBenchmark -and 'qwen3_tts' -notin $Models.Split(',')) {
    throw '-QwenBenchmark requires qwen3_tts in -Models'
}
$configure = @(
    '-S', (Join-Path $PSScriptRoot 'native'), '-B', $BuildDirectory, '-G', 'Ninja',
    "-DCMAKE_TOOLCHAIN_FILE=$toolchain", '-DANDROID_ABI=arm64-v8a',
    "-DANDROID_PLATFORM=android-$AndroidApi", '-DANDROID_STL=c++_shared',
    '-DCMAKE_BUILD_TYPE=Release', '-DENGINE_ENABLE_CUDA=OFF', '-DENGINE_ENABLE_VULKAN=OFF',
    '-DENGINE_ENABLE_NATIVE_CPU=OFF', '-DENGINE_ENABLE_OPENMP=OFF', '-DGGML_OPENMP=OFF',
    '-DENGINE_ENABLE_LLAMAFILE=OFF', "-DGGML_CPU_ARM_ARCH=$ArmArchitecture",
    '-DAUDIOCPP_MODEL_SET=custom', "-DAUDIOCPP_MODELS=$Models"
)
if ($SourceDirectory) {
    $SourceDirectory = (Resolve-Path -LiteralPath $SourceDirectory).Path
    $configure += "-DAUDIOCPP_SOURCE_DIR=$SourceDirectory"
}
Invoke-Checked cmake $configure
$targets = @('remotemedia_audiocpp_native', 'audiocpp_cli', 'audiocpp_model_manager', 'audiocpp_c_api_path_test')
if ($QwenBenchmark) { $targets += 'qwen3_android_bench' }
Invoke-Checked cmake (@('--build', $BuildDirectory, '--target') + $targets + @('-j', "$Jobs"))

New-Item -ItemType Directory -Force -Path $distDirectory | Out-Null
$nativeFiles = @('libaudiocpp.so', 'libaudiocpp_manager_bridge.so', 'audiocpp_cli',
    'audiocpp_model_manager', 'audiocpp_c_api_path_test')
if ($QwenBenchmark) { $nativeFiles += 'qwen3_android_bench' }
foreach ($file in $nativeFiles) {
    Copy-Item -LiteralPath (Join-Path $BuildDirectory "bin/$file") -Destination $distDirectory -Force
}
$llvmDirectory = Join-Path $NdkDirectory 'toolchains/llvm/prebuilt/windows-x86_64'
Copy-Item -LiteralPath (Join-Path $llvmDirectory 'sysroot/usr/lib/aarch64-linux-android/libc++_shared.so') -Destination $distDirectory -Force
$nativeFiles += 'libc++_shared.so'
foreach ($file in $nativeFiles) {
    Invoke-Checked (Join-Path $llvmDirectory 'bin/llvm-strip.exe') @('--strip-unneeded', (Join-Path $distDirectory $file))
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'native/AUDIOCPP-LICENSE') -Destination $distDirectory -Force
$metadata = [ordered]@{
    target = 'aarch64-linux-android'; api = $AndroidApi; backend = 'cpu'
    arm_architecture = $ArmArchitecture; models = $Models.Split(',')
    upstream_revision = '06c13737cc39481787a03465697b58b1c954b56f'
    ndk_revision = ([string](Get-Content (Join-Path $NdkDirectory 'source.properties') | Where-Object { $_ -match '^Pkg.Revision' })) -replace '^Pkg.Revision\s*=\s*', ''
    artifacts = @($nativeFiles | ForEach-Object {
        $artifact = Get-Item -LiteralPath (Join-Path $distDirectory $_)
        @{ name = $_; bytes = $artifact.Length; sha256 = (Get-FileHash -LiteralPath $artifact.FullName -Algorithm SHA256).Hash }
    })
}
$metadata | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 (Join-Path $distDirectory 'build-info.json')
Write-Output "Built Android native C API, model manager and CLI: $distDirectory"
