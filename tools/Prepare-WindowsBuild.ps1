#requires -Version 7.0
<#
.SYNOPSIS
Prepare the pinned native Windows tools used by vendored OpenSSL.
.DESCRIPTION
Requires x64 Windows and the Visual Studio C++ build tools. With no arguments,
returns the resolved tool paths for a caller to use. With CargoArguments, runs
Cargo with process-local settings and restores them even when Cargo fails.
The archives and extracted tools stay in the sibling Decompile/tools cache.
.EXAMPLE
& ./tools/Prepare-WindowsBuild.ps1 -CargoArguments @('test', '--locked', '--all-targets')
.EXAMPLE
& ./tools/Prepare-WindowsBuild.ps1 -CargoArguments @('clippy', '--locked', '--all-targets', '--', '-D', 'warnings')
.NOTES
Strawberry's published SHA-256: https://strawberryperl.com/releases.json
NASM's SHA-256 was recorded from the HTTPS publisher download below; NASM does
not publish a separate checksum file in that release directory.
Build requirements: https://github.com/openssl/openssl/blob/openssl-3.6.3/NOTES-WINDOWS.md
#>
param([string[]]$CargoArguments = @())

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'This helper supports only x86_64 Windows. Unix uses its native compiler, make and Perl.'
}

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$workspaceRoot = Split-Path -Parent $repositoryRoot
$cacheRoot = [IO.Path]::GetFullPath((Join-Path $workspaceRoot 'Decompile/tools'))

function Assert-CachePath([string]$Path) {
    $absolute = [IO.Path]::GetFullPath($Path)
    if (-not $absolute.StartsWith($cacheRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Tool path leaves the intended cache: $absolute"
    }
    # An existing junction or symlink must not redirect extraction or execution.
    $current = $absolute
    while ($current) {
        if (Test-Path -LiteralPath $current) {
            if ((Get-Item -Force -LiteralPath $current).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Tool cache path contains a reparse point: $current"
            }
        }
        $current = Split-Path -Parent $current
    }
    return $absolute
}

function Get-PinnedTool([string]$Url, [string]$Sha256, [string]$ArchiveName,
    [string]$DirectoryName, [string]$ExecutableEntry) {
    $archivePath = Assert-CachePath (Join-Path $cacheRoot $ArchiveName)
    $directory = Assert-CachePath (Join-Path $cacheRoot $DirectoryName)
    $executable = Assert-CachePath (Join-Path $directory $ExecutableEntry)
    if (-not (Test-Path -LiteralPath $archivePath)) {
        [IO.Directory]::CreateDirectory($cacheRoot) | Out-Null
        $temporary = Assert-CachePath ($archivePath + '.' + [Guid]::NewGuid().ToString('N') + '.download')
        try {
            Write-Host "Downloading $ArchiveName"
            Invoke-WebRequest -Uri $Url -OutFile $temporary
            if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $Sha256) {
                throw "Downloaded archive SHA-256 mismatch: $ArchiveName"
            }
            Move-Item -LiteralPath $temporary -Destination $archivePath
        } finally {
            if (Test-Path -LiteralPath $temporary) {
                Remove-Item -LiteralPath (Assert-CachePath $temporary)
            }
        }
    }
    if ((Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash -ne $Sha256) {
        throw "Cached archive SHA-256 mismatch: $archivePath"
    }
    $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $entry = $archive.GetEntry($ExecutableEntry)
        if ($null -eq $entry) { throw "Archive is missing $ExecutableEntry" }
        if (-not (Test-Path -LiteralPath $executable)) {
            # Check every destination before writing anything. ExtractToDirectory
            # additionally rejects entries outside its destination directory.
            foreach ($member in $archive.Entries) {
                $destination = Assert-CachePath (Join-Path $directory $member.FullName)
                if (-not $destination.StartsWith($directory + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
                    throw "Archive entry leaves its extraction directory: $($member.FullName)"
                }
                if ((($member.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) {
                    throw "Archive contains a symbolic link: $($member.FullName)"
                }
            }
            [IO.Directory]::CreateDirectory($directory) | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToDirectory($archive, $directory, $false)
        }
        # Also authenticate the actual cached executable before running it.
        # The remaining extraction is an ordinary trusted local tool install.
        $stream = $entry.Open()
        try { $expectedExecutableHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream)) }
        finally { $stream.Dispose() }
        if ((Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash -ne $expectedExecutableHash) {
            throw "Cached executable differs from its pinned archive: $executable"
        }
    } finally {
        $archive.Dispose()
    }
    return $executable
}

$perl = Get-PinnedTool `
    -Url 'https://github.com/StrawberryPerl/Perl-Dist-Strawberry/releases/download/SP_54221_64bit/strawberry-perl-5.42.2.1-64bit-portable.zip' `
    -Sha256 '32d83be90cf04b807cfb9477482bc36302cdee6f5b04cf57e81adecbd8f07898' `
    -ArchiveName 'strawberry-perl-5.42.2.1-portable.zip' `
    -DirectoryName 'strawberry-perl-5.42.2.1' -ExecutableEntry 'perl/bin/perl.exe'
$nasm = Get-PinnedTool `
    -Url 'https://www.nasm.us/pub/nasm/releasebuilds/3.02/win64/nasm-3.02-win64.zip' `
    -Sha256 '161d0bfaff53c2f9e9f3e69fd0672323ebabafd1268976a5cec11be92a19aee7' `
    -ArchiveName 'nasm-3.02-win64.zip' `
    -DirectoryName 'nasm-3.02-win64' -ExecutableEntry 'nasm-3.02/nasm.exe'

$perlVersion = & $perl '-MLocale::Maketext::Simple' '-e' 'print "$^V $^O"'
if ($LASTEXITCODE -ne 0 -or $perlVersion -ne 'v5.42.2 MSWin32') {
    throw "Unexpected or incomplete native Perl: $perlVersion"
}
$nasmVersion = & $nasm '-v'
if ($LASTEXITCODE -ne 0 -or $nasmVersion -notmatch '^NASM version 3\.02\b') {
    throw "Unexpected NASM: $nasmVersion"
}
$toolPaths = [PSCustomObject]@{
    Perl = $perl
    PerlDirectory = Split-Path -Parent $perl
    Nasm = $nasm
    NasmDirectory = Split-Path -Parent $nasm
}
if ($CargoArguments.Count -eq 0) {
    return $toolPaths
}

$originalPath = $env:PATH
$originalPerl = $env:OPENSSL_SRC_PERL
$originalNasm = $env:OPENSSL_RUST_USE_NASM
try {
    # Do not add Strawberry's c/bin: its GCC must not replace the MSVC compiler.
    $env:PATH = $toolPaths.NasmDirectory + ';' + $toolPaths.PerlDirectory + ';' + $originalPath
    $env:OPENSSL_SRC_PERL = $toolPaths.Perl
    $env:OPENSSL_RUST_USE_NASM = '1'
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed with exit code $LASTEXITCODE" }
} finally {
    $env:PATH = $originalPath
    $env:OPENSSL_SRC_PERL = $originalPerl
    $env:OPENSSL_RUST_USE_NASM = $originalNasm
}
