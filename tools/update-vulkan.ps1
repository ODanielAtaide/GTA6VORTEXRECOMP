param(
    [string]$BackupDirectory = "C:\GTA6VORTEXRECOMP_BACKUPS",
    [switch]$SkipTests
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
function Check-Git {
    param([string]$Message)
    if ($LASTEXITCODE -ne 0) { throw $Message }
}
function Save-Repository {
    param([string]$Stage)
    New-Item -ItemType Directory -Force -Path $BackupDirectory | Out-Null
    $sha = git rev-parse --short HEAD
    Check-Git "Falha ao ler HEAD"
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
    $bundle = Join-Path $BackupDirectory "Vortex-$stamp-$sha-$Stage.bundle"
    git bundle create $bundle --all
    Check-Git "Falha ao criar backup Git"
    git bundle verify $bundle
    Check-Git "Falha ao verificar backup Git"
    Write-Host "BACKUP VERIFICADO: $bundle" -ForegroundColor Green
}
# Save even if the remote is inaccessible. Bundles contain committed refs,
# not uncommitted working-tree files, ignored files, or external LFS objects.
Save-Repository "pre"
$branch = git branch --show-current
Check-Git "Falha ao identificar branch"
if ($branch -ne "experimental/amd-vulkan-spirv") { throw "Branch incorreta; backup anterior preservado" }
git fetch origin --tags '+refs/heads/*:refs/remotes/origin/*'
Check-Git "Falha no fetch; backup anterior preservado"
git pull --ff-only origin experimental/amd-vulkan-spirv
Check-Git "Falha no Git; backup anterior preservado"
Save-Repository "post"
if ($SkipTests) { return }

cargo test --release --locked -p vortex-shader --lib
Check-Git "Falha nos testes de shader"
cargo test --release --locked -p vortex-vulkan --lib
Check-Git "Falha nos testes Vulkan"
cargo build --release --locked -p vortex --features vulkan
Check-Git "Falha na compilacao"

$exe = ".\target\release\vortex.exe"
$stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
$compute = ".\target\vulkan-compute-$stamp"
$graphics = ".\target\vulkan-graphics-$stamp"
& $exe vulkan-advanced --emit-only $compute
Check-Git "Falha na traducao Compute"
& $exe vulkan-native-graphics --emit-only $graphics
Check-Git "Falha na traducao grafica"
foreach ($dir in @($compute,$graphics)) {
    Get-ChildItem "$dir\*.spv" | ForEach-Object {
        spirv-val --target-env vulkan1.2 $_.FullName
        Check-Git "SPIR-V invalido: $($_.Name)"
        Write-Host "VALIDADO: $($_.Name)" -ForegroundColor Green
    }
}
New-Item -ItemType Directory -Force -Path ".\target\vulkan-cache" | Out-Null
$env:AMD_VK_PIPELINE_CACHE = Join-Path (Get-Location).Path "target\vulkan-cache\session.bin"
foreach ($test in @("vulkan-native","vulkan-advanced")) {
    & $exe $test
    Check-Git "Falha em $test"
}
try {
    foreach ($mode in @("0","1")) {
        $env:VORTEX_VK_BATCH_DRAWS = $mode
        Write-Host "Draw batching: $mode" -ForegroundColor Cyan
        & $exe vulkan-native-graphics
        Check-Git "Falha no teste grafico (batching=$mode)"
    }
} finally {
    Remove-Item Env:VORTEX_VK_BATCH_DRAWS -ErrorAction SilentlyContinue
}
Write-Host "TODOS OS TESTES APROVADOS" -ForegroundColor Green
