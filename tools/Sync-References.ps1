param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$workspaceRoot = Split-Path -Parent $repositoryRoot
$decompileRoot = Join-Path $workspaceRoot 'Decompile'
$pumpkinRoot = Join-Path $workspaceRoot 'Pumpkin MC'
$roadmapRoot = Join-Path $workspaceRoot 'Roadmap'
$referenceLock = Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot 'references.lock.json') | ConvertFrom-Json

function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

foreach ($directory in @($decompileRoot, $roadmapRoot)) {
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
}
# Do not overwrite locally customized reference discovery settings.
foreach ($entry in @(@('decompile.gitignore', '.gitignore'), @('decompile.codegraph.json', 'codegraph.json'))) {
    $destination = Join-Path $decompileRoot $entry[1]
    if (-not (Test-Path -LiteralPath $destination)) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot "reference-configs/$($entry[0])") -Destination $destination
    }
}

if (-not (Test-Path -LiteralPath (Join-Path $pumpkinRoot '.git'))) {
    Invoke-Checked 'git' @('clone', $referenceLock.pumpkin.url, $pumpkinRoot)
}
$origin = & git -C $pumpkinRoot remote get-url origin
if ($LASTEXITCODE -ne 0 -or $origin -ne $referenceLock.pumpkin.url) {
    throw "Unexpected Pumpkin origin at $pumpkinRoot"
}
$changes = & git -C $pumpkinRoot status --porcelain --untracked-files=no
if ($LASTEXITCODE -ne 0 -or $changes) {
    throw 'Pumpkin has tracked changes; preserve them before syncing the reference'
}
& git -C $pumpkinRoot cat-file -e "$($referenceLock.pumpkin.commit)^{commit}" 2>$null
if ($LASTEXITCODE -ne 0) {
    Invoke-Checked 'git' @('-C', $pumpkinRoot, 'fetch', 'origin', $referenceLock.pumpkin.commit)
}
Invoke-Checked 'git' @('-C', $pumpkinRoot, 'checkout', '--detach', $referenceLock.pumpkin.commit)
Invoke-Checked 'git' @('-C', $pumpkinRoot, 'submodule', 'update', '--init', '--recursive')

$prepareArguments = @((Join-Path $PSScriptRoot 'prepare_minecraft.py'))
$sourceRoot = Join-Path $decompileRoot "sources/$($referenceLock.minecraft.id)"
if (Test-Path -LiteralPath $sourceRoot) {
    $prepareArguments += '--verify-existing'
}
Invoke-Checked 'python' $prepareArguments
foreach ($projectRoot in @($repositoryRoot, $decompileRoot, $pumpkinRoot)) {
    if (Test-Path -LiteralPath (Join-Path $projectRoot '.codegraph')) {
        Invoke-Checked 'codegraph' @('sync', $projectRoot)
    } else {
        Invoke-Checked 'codegraph' @('init', '--yes', $projectRoot)
    }
    Invoke-Checked 'codegraph' @('status', $projectRoot)
}
Invoke-Checked 'python' @((Join-Path $PSScriptRoot 'verify_reference_index.py'))
