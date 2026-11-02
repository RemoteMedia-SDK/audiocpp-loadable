param(
    [ValidateSet('cpu','cuda')][string]$Backend = 'cpu',
    [string]$SourceDir = '',
    [string]$Generator = '',
    [string]$CudaRoot = '',
    [string]$CudaArchitectures = '89',
    [string]$BuildDirectory = '',
    [int]$Jobs = 8
)
$ErrorActionPreference = 'Stop'
function Invoke-Checked([string]$Exe, [string[]]$Arguments) {
    # PowerShell 5 can classify native stderr warnings as errors when redirected.
    # The process exit code is the build result.
    $ErrorActionPreference = 'Continue'
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Exe failed with exit code $LASTEXITCODE" }
}
if (!$Generator) { $Generator = @{cpu='Visual Studio 17 2022';cuda='Ninja'}[$Backend] }
if ($Generator -eq 'Ninja') {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $vs = & $vswhere -latest -products '*' -version '[17,18)' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (!$vs) { throw 'Visual Studio 2022 C++ build tools are required.' }
    $vcvars = Join-Path $vs 'VC/Auxiliary/Build/vcvars64.bat'
    # A long inherited PATH can exceed cmd.exe limits inside vcvarsall.
    $toolDirs = @('cmake','ninja','cargo','git','python') | ForEach-Object { Split-Path (Get-Command $_ -ErrorAction Stop).Source }
    $env:PATH = ((@("$env:SystemRoot/System32",$env:SystemRoot) + $toolDirs) | Select-Object -Unique) -join ';'
    $environment = & cmd /d /s /c "`"$vcvars`" >nul && set"
    if ($LASTEXITCODE -ne 0) { throw 'MSVC environment initialization failed.' }
    foreach ($entry in $environment) {
        if ($entry -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1],$Matches[2],'Process') }
    }
}
$buildDir = if ($BuildDirectory) { $BuildDirectory } else { Join-Path $PSScriptRoot "native/build-$Backend-$($Generator -replace '[^A-Za-z0-9]','')" }
$distDir = Join-Path $PSScriptRoot "dist/$Backend"
$configure = @('-S', (Join-Path $PSScriptRoot 'native'), '-B', $buildDir, '-G', $Generator)
if ($Generator -like 'Visual Studio*') { $configure += @('-A','x64') }
$configure += "-DENGINE_ENABLE_CUDA=$(@{cpu='OFF';cuda='ON'}[$Backend])"
$configure += '-DCMAKE_BUILD_TYPE=Release'
if ($SourceDir) { $configure += "-DAUDIOCPP_SOURCE_DIR=$((Resolve-Path -LiteralPath $SourceDir).Path)" }
if ($Backend -eq 'cuda') {
    if (!$CudaRoot) {
        $CudaRoot = Get-ChildItem -LiteralPath "$env:ProgramFiles/NVIDIA GPU Computing Toolkit/CUDA" -Directory |
            Sort-Object { [Version]($_.Name -replace '^v','') } -Descending | Select-Object -First 1 -ExpandProperty FullName
    }
    if (!$CudaRoot) { throw 'Set -CudaRoot to an installed CUDA toolkit.' }
    $configure += @("-DCUDAToolkit_ROOT=$CudaRoot", "-DCMAKE_CUDA_COMPILER=$CudaRoot/bin/nvcc.exe", "-DCMAKE_CUDA_ARCHITECTURES=$CudaArchitectures")
    if ($Generator -eq 'Ninja') { $configure += '-DCMAKE_CUDA_FLAGS=--use-local-env' }
}
Invoke-Checked cmake $configure
Invoke-Checked cmake @('--build',$buildDir,'--config','Release','--target','remotemedia_audiocpp_native','-j',"$Jobs")
Invoke-Checked cargo @('build','--locked','--release','--manifest-path',(Join-Path $PSScriptRoot 'Cargo.toml'))
New-Item -ItemType Directory -Force -Path $distDir | Out-Null
Get-ChildItem -LiteralPath (Join-Path $buildDir 'bin') -Recurse -File -Filter '*.dll' | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination $distDir -Force
}
if ($Backend -eq 'cuda') {
    foreach ($pattern in @('cublas64_*.dll','cublasLt64_*.dll','cufft64_*.dll','cudart64_*.dll')) {
        Get-ChildItem -LiteralPath (Join-Path $CudaRoot 'bin') -Recurse -File -Filter $pattern | ForEach-Object {
            Copy-Item -LiteralPath $_.FullName -Destination $distDir -Force
        }
    }
    $cudaLicense = Join-Path $CudaRoot 'EULA.txt'
    if (Test-Path -LiteralPath $cudaLicense) { Copy-Item -LiteralPath $cudaLicense -Destination (Join-Path $distDir 'CUDA-EULA.txt') -Force }
}
foreach ($file in @('audiocpp_loadable.dll','host.exe')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "target/release/$file") -Destination $distDir -Force
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'plugin.toml') -Destination $distDir -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'native/AUDIOCPP-LICENSE') -Destination $distDir -Force
Invoke-Checked (Join-Path $distDir 'host.exe') @((Join-Path $distDir 'audiocpp_loadable.dll'),$distDir,'capabilities')
Write-Output "Built and smoke-tested $distDir"
