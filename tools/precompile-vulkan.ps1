param(
    [Parameter(Mandatory=$true)][string]$GameDir,
    [string]$Exe,
    [ValidateRange(1,8)][int]$Workers = 6,
    [string]$BackupDirectory = "C:\GTA6VORTEXRECOMP_BACKUPS",
    [switch]$Refresh,
    [switch]$Launch
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
function Check($message) { if ($LASTEXITCODE -ne 0) { throw $message } }
$GameDir = (Resolve-Path -LiteralPath $GameDir).Path
if (!(Test-Path -LiteralPath $GameDir -PathType Container)) { throw "Pasta invalida" }
if ($Launch) {
    if (!$Exe) { throw "Informe -Exe para usar -Launch" }
    $Exe = (Resolve-Path -LiteralPath $Exe).Path
    if (!(Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "Executavel invalido" }
}
$stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
New-Item -ItemType Directory -Force -Path $BackupDirectory | Out-Null
function Backup($stage) {
    $p = Join-Path $BackupDirectory "Vortex-$stamp-$stage.bundle"
    git bundle create $p --all
    Check "Falha no backup"
    git bundle verify $p
    Check "Backup invalido"
}
Backup "precompile-pre"
$branch = git branch --show-current
Check "Falha ao consultar branch"
if ($branch -ne "experimental/amd-vulkan-spirv") { throw "Branch incorreta" }
git pull --ff-only origin experimental/amd-vulkan-spirv
Check "Falha no Git"
Backup "precompile-post"
cargo build --release --locked -p vortex --features "vulkan,diagnostics"
Check "Falha na compilacao"
$binary = (Resolve-Path ".\target\release\vortex.exe").Path
$scanArgs = @("vulkan-precompile", "--game-dir", $GameDir, "--workers", "$Workers")
if (Get-Command spirv-val -ErrorAction SilentlyContinue) {
    $scanArgs += "--validate"
} else {
    Write-Warning "spirv-val nao encontrado; analise sem validacao SPIR-V"
}
if ($Refresh) { $scanArgs += "--refresh" }
$env:RUST_LOG = "warn"
& $binary @scanArgs
Check "Falha na analise previa"
if ($Launch) {
    $env:RUST_LOG = "info"
    Write-Host "A analise nao corrige automaticamente os pipelines. Iniciando runtime experimental Vulkan."
    & $binary run $Exe --backend vulkan --execute
    exit $LASTEXITCODE
}
