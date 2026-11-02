param(
    [ValidateSet('cpu','cuda')][string]$Backend = 'cpu',
    [string]$Method = 'capabilities',
    [string]$ParamsJson = '{}'
)
$ErrorActionPreference = 'Stop'
$distDir = Join-Path $PSScriptRoot "dist/$Backend"
$paramsFile = Join-Path ([IO.Path]::GetTempPath()) ("audiocpp-params-" + [Guid]::NewGuid().ToString('N') + '.json')
try {
    # @file avoids Windows PowerShell 5's native-argument quote stripping.
    [IO.File]::WriteAllText($paramsFile,$ParamsJson,[Text.UTF8Encoding]::new($false))
    & (Join-Path $distDir 'host.exe') (Join-Path $distDir 'audiocpp_loadable.dll') $distDir $Method "@$paramsFile"
    if ($LASTEXITCODE -ne 0) { throw "audio.cpp host failed: $LASTEXITCODE" }
} finally {
    Remove-Item -LiteralPath $paramsFile -ErrorAction SilentlyContinue
}
