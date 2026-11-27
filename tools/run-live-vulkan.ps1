param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [string]$BackupDirectory = "C:\GTA6VORTEXRECOMP_BACKUPS"
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
function Check($message) { if ($LASTEXITCODE -ne 0) { throw $message } }
if (!(Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "Executavel nao encontrado" }
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
New-Item -ItemType Directory -Force -Path $BackupDirectory | Out-Null
function Backup($stage) {
    $path = Join-Path $BackupDirectory "Vortex-$stamp-$stage.bundle"
    git bundle create $path --all
    Check "Falha no backup"
    git bundle verify $path
    Check "Backup invalido"
}
Backup "pre"
$branch = git branch --show-current
Check "Falha ao consultar branch"
if ($branch -ne "experimental/amd-vulkan-spirv") { throw "Branch incorreta" }
git fetch origin --tags '+refs/heads/*:refs/remotes/origin/*'
Check "Falha no fetch"
git pull --ff-only origin experimental/amd-vulkan-spirv
Check "Falha no Git"
Backup "post"
cargo build --release --locked -p vortex --features "vulkan,diagnostics"
Check "Falha na compilacao"
$env:RUST_LOG = "info"
$ErrorActionPreference = "Continue"
$log = Join-Path (Get-Location).Path "vulkan-live-$stamp.log"
& ".\target\release\vortex.exe" run $Exe --backend vulkan --execute 2>&1 | Tee-Object -FilePath $log
$result = $LASTEXITCODE
Write-Host "Runtime exit: $result"
Write-Host "Log: $log"
Select-String -Path $log -Pattern 'VULKAN_GUEST_DESCRIPTOR_HEAPS_BOUND|VULKAN_SHADER_OPCODE_GAP|VULKAN_COMPUTE_HARDWARE_INPUTS|VULKAN_OPTIONAL_PSO_UNSUPPORTED|VULKAN_RUNTIME_BLOCKER|FIRST_VULKAN_GUEST_PRESENT_OK'
exit $result
