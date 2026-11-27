param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [string]$BackupDirectory = "C:\GTA6VORTEXRECOMP_BACKUPS",
    [switch]$NoFetch
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
if (!(Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "Executavel nao encontrado: $Exe" }
$Exe = (Resolve-Path -LiteralPath $Exe).Path
function Check($Message) { if ($LASTEXITCODE -ne 0) { throw $Message } }
function Backup($Stage) {
    New-Item -ItemType Directory -Force -Path $BackupDirectory | Out-Null
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
    $path = Join-Path $BackupDirectory "Vortex-first-frame-$stamp-$Stage.bundle"
    git bundle create $path --all
    Check "Falha ao criar backup"
    git bundle verify $path
    Check "Backup invalido"
    Write-Host "BACKUP VERIFICADO: $path" -ForegroundColor Green
}
Backup "pre"
if (!$NoFetch) {
    $branch = git branch --show-current
    Check "Falha ao consultar branch"
    if ($branch -ne "experimental/amd-vulkan-spirv") { throw "Branch incorreta" }
    git pull --ff-only origin experimental/amd-vulkan-spirv
    Check "Falha na atualizacao"
}
Backup "post"
# Intentionally build only the established title runtime. The experimental
# Vulkan compiler is not linked into the live title GPU command path yet.
cargo build --release --locked -p vortex --features diagnostics
Check "Falha na compilacao"
$stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
$runDir = Join-Path (Get-Location).Path "target\first-frame-$stamp"
New-Item -ItemType Directory -Force -Path $runDir | Out-Null
$env:VORTEX_CAPTURE = $runDir
$env:VORTEX_CAPTURE_FRAMES = "1,2,3,4,5,6,7,8"
$env:RUST_LOG = "info"
Write-Host "Backend D3D12/DXGI (nao Vulkan). Log e imagens: $runDir" -ForegroundColor Cyan
# Run the user's own local authorized executable; do not download or modify it.
& ".\target\release\vortex.exe" run $Exe --execute --backend d3d12 2>&1 |
    Tee-Object -FilePath (Join-Path $runDir "run.log")
$code=$LASTEXITCODE
Write-Host "Log: $runDir\run.log"
Write-Host "Capturas: $runDir\frame-*.png"
if ($code -ne 0) { throw "Runtime saiu com codigo $code" }
