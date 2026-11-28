[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$LeonidaSource,
    [Parameter(Mandatory)][string]$GameDirectory,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$WorkDirectory,
    [string]$Python = 'python.exe',
    [string]$CMake = 'cmake.exe',
    [ValidateRange(1, 256)][int]$Jobs = [Math]::Max(1, [int]([Environment]::ProcessorCount / 2))
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $LeonidaSource).Path
$game = (Resolve-Path -LiteralPath $GameDirectory).Path
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$work = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($WorkDirectory)
foreach ($directory in @($output, $work)) {
    $ancestor = [IO.DirectoryInfo]::new($directory)
    while ($null -ne $ancestor) {
        if ((Test-Path -LiteralPath (Join-Path $ancestor.FullName '.vortex-root')) -or
            $ancestor.FullName -eq $source -or $ancestor.FullName -eq $game) {
            throw 'Choose runtime and work directories outside the source and the game package.'
        }
        $ancestor = $ancestor.Parent
    }
}

$revision = & git -C $source rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $revision -ne '39f52f39c435e04273bc822f258f2c4bb1b7fec0') {
    throw 'Use ProjectLeonida/mandy at revision 39f52f39c435e04273bc822f258f2c4bb1b7fec0.'
}
foreach ($name in @('leonida-vulkan-only.patch', 'leonida-cache-writeback.patch')) {
    $patch = Join-Path $PSScriptRoot $name
    & git -C $source apply --reverse --check $patch 2>$null
    if ($LASTEXITCODE -ne 0) {
        & git -C $source apply $patch
        if ($LASTEXITCODE -ne 0) { throw "Could not apply $name to Leonida." }
    }
}

$sourceRuntime = Join-Path $source 'runtime'
$build = Join-Path $sourceRuntime 'build'
& $CMake -S $sourceRuntime -B $build -G 'Visual Studio 17 2022' -A x64 `
    -DLEONIDA_REQUIRE_VULKAN=ON -DLEONIDA_SDL3=ON -DLEONIDA_SDL3_FETCH=ON
if ($LASTEXITCODE -ne 0) { throw 'Could not configure the Vulkan runtime.' }
& $CMake --build $build --config RelWithDebInfo --parallel $Jobs --target gta6rt gta6rt_tls_host bcpack_history_train
if ($LASTEXITCODE -ne 0) { throw 'Could not compile the Vulkan runtime.' }

$binaries = Join-Path $build 'RelWithDebInfo'
$capability = & (Join-Path $binaries 'gta6rt.exe') --vortex-vulkan-runtime
if ($LASTEXITCODE -ne 0 -or $capability -ne 'vortex-vulkan-runtime=1') {
    throw 'The compiled executable does not expose the Vortex Vulkan-only runtime.'
}
& $Python -I (Join-Path $source 'make_game.py') --game $game --out $output --work $work `
    --stages check,shaders,runtime --runtime-build $binaries --jobs $Jobs
if ($LASTEXITCODE -ne 0) { throw 'Could not prepare the local game and shader library.' }
