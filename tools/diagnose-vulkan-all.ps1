# SPDX-License-Identifier: GPL-3.0-or-later
param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [ValidateRange(1,8)][int]$Workers=6,
    [ValidateRange(15,3600)][int]$GameSeconds=180,
    [switch]$CodeOnly
)
$ErrorActionPreference="Stop"
Set-Location (Join-Path $PSScriptRoot "..")
$Exe=(Resolve-Path -LiteralPath $Exe).Path
if (!(Test-Path -LiteralPath $Exe -PathType Leaf)) { throw "Executavel invalido" }
$GameDir=Split-Path -Parent $Exe
$root=Join-Path (Get-Location).Path ("diagnostics\vulkan-"+(Get-Date -Format "yyyyMMdd-HHmmss-fff"))
New-Item -ItemType Directory -Path $root -Force | Out-Null
$status=New-Object System.Collections.Generic.List[object]
$env:RUST_LOG="info"
$env:NO_COLOR="1"
$env:CARGO_TERM_COLOR="never"
$env:RUST_BACKTRACE="1"
$env:VORTEX_BATCH_DIAGNOSTICS="1"
function Run-Step([string]$Name,[string]$Program,[string[]]$Arguments,[int]$Timeout=0) {
    $out=Join-Path $root ($Name+".stdout.log")
    $err=Join-Path $root ($Name+".stderr.log")
    $exitCode=$null
    $timedOut=$false
    $failure=$null
    $clock=[Diagnostics.Stopwatch]::StartNew()
    Write-Host "=== $Name ==="
    try {
        $app=(Get-Command $Program -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        # Windows paths cannot contain double quotes; reject ambiguous inputs.
        if ($Arguments | Where-Object { $_ -match '"' -or $_ -match '\r|\n' }) { throw "Argumento invalido" }
        $quoted=($Arguments | ForEach-Object { '"' + ($_ -replace '(\\+)$','$1$1') + '"' }) -join ' '
        $p=Start-Process -FilePath $app -ArgumentList $quoted -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
        if ($Timeout -gt 0) {
            if (!$p.WaitForExit($Timeout*1000)) {
                $timedOut=$true
                Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
                $p.WaitForExit()
            }
        } else { $p.WaitForExit() }
        $exitCode=$p.ExitCode
    } catch {
        $failure=$_.Exception.Message
        $failure | Out-File -LiteralPath (Join-Path $root ($Name+".error.txt")) -Encoding utf8
    }
    $clock.Stop()
    $status.Add([pscustomobject]@{Step=$Name;ExitCode=$exitCode;Timeout=$timedOut;Error=$failure;Seconds=[math]::Round($clock.Elapsed.TotalSeconds,2)})
    Write-Host "Exit=$exitCode Timeout=$timedOut"
    return (!$timedOut -and !$failure -and $null -ne $exitCode -and $exitCode -eq 0)
}
git rev-parse HEAD | Out-File (Join-Path $root "revision.txt") -Encoding utf8
git status --short | Out-File (Join-Path $root "git-status.txt") -Encoding utf8
$built=Run-Step "build" "cargo" @("build","--release","--locked","-p","vortex","--features","vulkan,diagnostics")
# Independent crates: a failure must not prevent collecting the other results.
$null=Run-Step "shader-tests" "cargo" @("test","--release","--locked","-p","vortex-shader","--no-fail-fast","--","--nocapture")
$null=Run-Step "gpu-tests" "cargo" @("test","--release","--locked","-p","vortex-gpu","--features","vulkan,diagnostics","--no-fail-fast","--","--nocapture")
$null=Run-Step "vulkan-tests" "cargo" @("test","--release","--locked","-p","vortex-vulkan","--no-fail-fast","--","--nocapture")
if ($built) {
    $bin=(Resolve-Path ".\target\release\vortex.exe").Path
    $null=Run-Step "config" $bin @("config")
    $null=Run-Step "imports" $bin @("census",$GameDir,"--missing")
    $null=Run-Step "gpu-check" $bin @("vulkan-check","--async-check")
    $null=Run-Step "gpu-commands" $bin @("vulkan-commands")
    $null=Run-Step "native-compute" $bin @("vulkan-native")
    $null=Run-Step "advanced-compute" $bin @("vulkan-advanced")
    $null=Run-Step "native-graphics" $bin @("vulkan-native-graphics")
    $null=Run-Step "game" $bin @("run",$Exe,"--backend","vulkan","--execute") $GameSeconds
    $scan=@("vulkan-precompile","--game-dir",$GameDir,"--workers","$Workers","--refresh")
    if ($CodeOnly) { $scan+="--code-only" }
    if (Get-Command spirv-val -ErrorAction SilentlyContinue) { $scan+="--validate" }
    $null=Run-Step "shader-scan" $bin $scan
    $scanLog=Join-Path $root "shader-scan.stdout.log"
    if (Test-Path $scanLog) {
        $last=Select-String -LiteralPath $scanLog -Pattern 'VULKAN_PRECOMPILE_DONE.* report=(.+)$' | Select-Object -Last 1
        if ($last) {
            $report=$last.Matches[0].Groups[1].Value.Trim()
            if (Test-Path -LiteralPath $report -PathType Container) {
                $dest=Join-Path $root "shader-report"
                New-Item -ItemType Directory -Path $dest -Force | Out-Null
                foreach ($name in @("report.txt","analysis.toml","files.tsv","containers.tsv","modules.txt")) {
                    $file=Join-Path $report $name
                    if (Test-Path -LiteralPath $file -PathType Leaf) { Copy-Item -LiteralPath $file -Destination $dest }
                }
            }
        }
    }
}
$status | Export-Csv -LiteralPath (Join-Path $root "steps.csv") -NoTypeInformation -Encoding UTF8
$pattern='VULKAN_.*(?:GAP|BLOCKER|UNSUPPORTED)|error(?:\[[A-Z]\d+\])?:|FAILED|panicked at|missing='
Get-ChildItem -LiteralPath $root -Filter "*.log" -File | ForEach-Object {
    Select-String -LiteralPath $_.FullName -Pattern $pattern | ForEach-Object {
        "{0}:{1}: {2}" -f (Split-Path -Leaf $_.Path),$_.LineNumber,$_.Line.Trim()
    }
} | Out-File -LiteralPath (Join-Path $root "gaps.txt") -Encoding utf8
$zip=$root+".zip"
Compress-Archive -Path (Join-Path $root "*") -DestinationPath $zip
Write-Host "DIAGNOSTICO: $zip"
Write-Host "Envie somente o ZIP. Nenhum arquivo do jogo foi incluido."
