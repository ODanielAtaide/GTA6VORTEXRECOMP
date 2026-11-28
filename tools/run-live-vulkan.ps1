param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$ShaderPack,
    [ValidateSet("series-s", "series-x")][string]$Console = "series-s"
)

$ErrorActionPreference = "Stop"
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$ShaderPack = (Resolve-Path -LiteralPath $ShaderPack).Path
Push-Location (Join-Path $PSScriptRoot "..")
try {
    cargo build --release --locked -p vortex --features diagnostics
    if ($LASTEXITCODE -ne 0) { throw "Falha na compilação." }
    & ".\target\release\vortex.exe" run $Exe --console $Console --execute --shader-pack $ShaderPack
    if ($LASTEXITCODE -ne 0) { throw "O runtime saiu com código $LASTEXITCODE." }
} finally {
    Pop-Location
}
