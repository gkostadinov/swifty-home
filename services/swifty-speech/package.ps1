#Requires -Version 7.0
<#
.SYNOPSIS
    Builds swifty-speech and creates dist\swifty-speech and dist\swifty-speech.zip.

.PARAMETER ModelDir
    Folder holding encoder.int8.onnx, decoder.int8.onnx, joiner.int8.onnx and
    tokens.txt. Defaults to the SWIFTY_MODEL_DIR environment variable.
    Only those four files are copied, so test_wavs and the README are left out.
    Each file must match the checksum recorded below.

.PARAMETER SherpaArchiveDir
    Folder holding the official sherpa-onnx static library archive, for building
    offline. If the archive is not already in the build cache and this is not
    given, it is downloaded from the official release. Either way it must match
    the checksum recorded below.

.NOTES
    Set CARGO_TARGET_DIR to a short path to avoid long-path problems; without
    it, %TEMP%\ss-target is used. rust-toolchain.toml pins the compiler.
#>
[CmdletBinding()]
param(
    [string]$ModelDir = $env:SWIFTY_MODEL_DIR,
    [string]$SherpaArchiveDir
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Add-Type -AssemblyName System.IO.Compression.FileSystem

# NVIDIA Parakeet TDT 0.6B v3 int8, as published by sherpa-onnx.
$ModelFiles = [ordered]@{
    'encoder.int8.onnx' = '1afbe69c1ffdabee94755f42e4daf1d93549131a3da11356bda84fcef0f2c479'
    'decoder.int8.onnx' = '0222a86147299d4dee90edf95b6d13827edb42ad8b7c9cf0b2582f030937c0b5'
    'joiner.int8.onnx'  = 'cbd02f3696e9944970111fb439d9c3fd365a80374565f13f2b9c7007d253fc98'
    'tokens.txt'        = 'd58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d'
}
$Target = 'x86_64-pc-windows-msvc'

$SherpaVersion = '2.0.1'
$SherpaStem = "sherpa-onnx-v$SherpaVersion-win-x64-static-MT-Release-lib"
$SherpaArchive = "$SherpaStem.tar.bz2"
$SherpaArchiveUrl = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$SherpaVersion/$SherpaArchive"
$SherpaArchiveSha256 = '23e7b986b197eda0bbc47ff3d7d5c87afd6ef2b9077214ba26b0f1ec52ef0e66'

# Every entry in the zip gets this time, so that the zip is reproducible.
$ZipEntryTime = [DateTimeOffset]::new(2026, 1, 1, 0, 0, 0, [TimeSpan]::Zero)

# DLLs that are part of every supported Windows. Anything else fails the build.
$SystemDlls = @(
    'advapi32.dll', 'bcrypt.dll', 'bcryptprimitives.dll', 'dbghelp.dll', 'dxgi.dll', 'kernel32.dll',
    'ntdll.dll', 'ole32.dll', 'oleaut32.dll', 'setupapi.dll', 'shell32.dll', 'shlwapi.dll',
    'user32.dll', 'userenv.dll', 'ws2_32.dll'
)

function Assert-Sha256([string]$Path, [string]$Expected) {
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($actual -ne $Expected) {
        throw "Checksum mismatch for ${Path}: expected $Expected, got $actual"
    }
}

if (-not $ModelDir) {
    throw 'Pass -ModelDir or set SWIFTY_MODEL_DIR.'
}
foreach ($name in $ModelFiles.Keys) {
    $path = Join-Path $ModelDir $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Model file missing: $path"
    }
    Assert-Sha256 $path $ModelFiles[$name]
}

if (-not $env:CARGO_TARGET_DIR) {
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'ss-target'
}

$root = $PSScriptRoot
$dist = Join-Path $root 'dist'
$package = Join-Path $dist 'swifty-speech'
$zip = Join-Path $dist 'swifty-speech.zip'

# Make sure the build links the checked sherpa-onnx libraries and nothing else.
function Use-VerifiedSherpaArchive {
    $cache = Join-Path $env:CARGO_TARGET_DIR 'sherpa-onnx-prebuilt'
    $archive = Join-Path $cache $SherpaArchive
    New-Item -ItemType Directory -Path $cache -Force | Out-Null

    if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
        if ($SherpaArchiveDir) {
            Copy-Item -LiteralPath (Join-Path $SherpaArchiveDir $SherpaArchive) -Destination $archive
        } else {
            Write-Host "Downloading $SherpaArchiveUrl"
            Invoke-WebRequest -Uri $SherpaArchiveUrl -OutFile "$archive.part"
            Move-Item -LiteralPath "$archive.part" -Destination $archive
        }
    }
    Assert-Sha256 $archive $SherpaArchiveSha256

    # With this set, the crate's build script uses the archive just checked and
    # fails rather than downloading another one of its own.
    $env:SHERPA_ONNX_ARCHIVE_DIR = $cache

    # The build script reuses an extracted folder without looking at the archive,
    # so one that was not extracted from the checked archive is discarded.
    $extracted = Join-Path $cache $SherpaStem
    $marker = Join-Path $cache "$SherpaStem.sha256"
    $current = (Test-Path -LiteralPath $marker) -and ((Get-Content -LiteralPath $marker -Raw).Trim() -eq $SherpaArchiveSha256)
    if (-not $current) {
        if (Test-Path -LiteralPath $extracted) { Remove-Item -LiteralPath $extracted -Recurse -Force }
        cargo clean --release -p sherpa-onnx-sys
        Set-Content -LiteralPath $marker -Value $SherpaArchiveSha256 -Encoding utf8NoBOM
    }
}

function Read-LicenceText([string]$Path) {
    return (Get-Content -LiteralPath $Path -Raw -Encoding utf8).Replace("`r`n", "`n").Trim("`n", ' ')
}

function Get-CrateNotices {
    $metadata = cargo metadata --format-version 1 --locked --filter-platform $Target | ConvertFrom-Json -Depth 64
    $packages = @{}
    foreach ($item in $metadata.packages) { $packages[$item.id] = $item }
    $nodes = @{}
    foreach ($item in $metadata.resolve.nodes) { $nodes[$item.id] = $item }

    # Follow normal dependencies only, and stop at compile-time macros: build
    # tools and macros, and what only they depend on, are not in the executable.
    $seen = [System.Collections.Generic.HashSet[string]]::new()
    $pending = [System.Collections.Generic.Stack[string]]::new()
    $pending.Push($metadata.resolve.root)
    while ($pending.Count -gt 0) {
        foreach ($dependency in $nodes[$pending.Pop()].deps) {
            $isNormal = @($dependency.dep_kinds | Where-Object { $null -eq $_.kind }).Count -gt 0
            $isMacro = @($packages[$dependency.pkg].targets | Where-Object { $_.kind -contains 'proc-macro' }).Count -gt 0
            if ($isNormal -and -not $isMacro -and $seen.Add($dependency.pkg)) { $pending.Push($dependency.pkg) }
        }
    }
    $crates = @($seen | ForEach-Object { $packages[$_] } | Sort-Object name, version)

    $lines = @(
        '## Rust crates compiled into the executable', '',
        "Generated from the build's dependency graph.", '',
        '| Crate | Version | Licence | Source |', '| --- | --- | --- | --- |'
    )
    foreach ($crate in $crates) {
        $licence = if ($crate.license) { $crate.license } else { 'see below' }
        $lines += "| $($crate.name) | $($crate.version) | $licence | $($crate.repository) |"
    }

    $sherpa = $crates | Where-Object name -eq 'sherpa-onnx'
    $apache = Read-LicenceText (Join-Path (Split-Path $sherpa.manifest_path -Parent) 'LICENSE')
    if (-not $apache.Contains('TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION')) {
        throw 'The sherpa-onnx crate no longer ships the Apache License 2.0 as LICENSE.'
    }

    # One section per distinct text, in the order of the sorted crates.
    $texts = [ordered]@{}
    foreach ($crate in $crates) {
        $folder = Split-Path $crate.manifest_path -Parent
        $files = @(Get-ChildItem -LiteralPath $folder -File |
            Where-Object { $_.Name -match '^(licen[cs]e|copying|notice|unlicense)' } |
            Sort-Object Name)
        if ($files.Count -eq 0) { throw "Crate $($crate.name) $($crate.version) ships no licence file." }
        foreach ($file in $files) {
            $text = Read-LicenceText $file.FullName
            if (-not $texts.Contains($text)) { $texts[$text] = [System.Collections.Generic.List[string]]::new() }
            $label = "$($crate.name) $($crate.version)"
            if (-not $texts[$text].Contains($label)) { $texts[$text].Add($label) }
        }
    }

    $lines += '', '## Licence texts of those crates', ''
    $number = 0
    foreach ($text in $texts.Keys) {
        if ($text -eq $apache) { continue }
        $number += 1
        $lines += "### Licence text $number", '', "Applies to: $($texts[$text] -join ', ')", '', '~~~text'
        $lines += $text -split "`n"
        $lines += '~~~', ''
    }

    $lines += '## Apache License 2.0', '', 'The full text, as shipped with sherpa-onnx.', ''
    if ($texts.Contains($apache)) { $lines += "Also applies to: $($texts[$apache] -join ', ')", '' }
    $lines += '~~~text'
    $lines += $apache -split "`n"
    $lines += '~~~'
    return $lines
}

function Find-Dumpbin {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { return $null }
    $installs = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    foreach ($install in $installs) {
        $found = Get-ChildItem -LiteralPath (Join-Path $install 'VC\Tools\MSVC') -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending |
            ForEach-Object { Join-Path $_.FullName 'bin\Hostx64\x64\dumpbin.exe' } |
            Where-Object { Test-Path -LiteralPath $_ } |
            Select-Object -First 1
        if ($found) { return $found }
    }
    return $null
}

function New-ReproducibleZip([string]$Folder, [string]$Destination) {
    $parent = Split-Path $Folder -Parent
    $names = [string[]]@(Get-ChildItem -LiteralPath $Folder -Recurse -File |
        ForEach-Object { [System.IO.Path]::GetRelativePath($parent, $_.FullName) })
    [Array]::Sort($names, [StringComparer]::Ordinal)

    $archive = [System.IO.Compression.ZipFile]::Open($Destination, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in $names) {
            $entry = $archive.CreateEntry($name.Replace('\', '/'), [System.IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = $ZipEntryTime
            $source = [System.IO.File]::OpenRead((Join-Path $parent $name))
            $target = $entry.Open()
            try { $source.CopyTo($target) } finally { $target.Dispose(); $source.Dispose() }
        }
    }
    finally {
        $archive.Dispose()
    }
}

$previousRustFlags = $env:CARGO_ENCODED_RUSTFLAGS
$previousArchiveDir = $env:SHERPA_ONNX_ARCHIVE_DIR
Push-Location $root
try {
    # This variable replaces the rustflags in .cargo\config.toml, so it has to
    # repeat crt-static. The other two keep this machine's paths out of the
    # executable and make its link timestamps depend only on its contents.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
    $registrySources = Join-Path $cargoHome 'registry\src'
    $env:CARGO_ENCODED_RUSTFLAGS = @(
        '-Ctarget-feature=+crt-static',
        "--remap-path-prefix=$registrySources=cargo",
        '-Clink-arg=/Brepro'
    ) -join [char]0x1F

    Use-VerifiedSherpaArchive

    Write-Host "Building (CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR)"
    cargo build --release --locked
    $exe = Join-Path $env:CARGO_TARGET_DIR 'release\swifty-speech.exe'

    $dumpbin = Find-Dumpbin
    if (-not $dumpbin) {
        throw 'dumpbin was not found, so the DLL list cannot be checked. Install the MSVC build tools.'
    }
    $imports = & $dumpbin /nologo /dependents $exe | ForEach-Object { $_.Trim() } | Where-Object { $_ -like '*.dll' }
    $unexpected = @($imports | Where-Object { $_ -notin $SystemDlls -and $_ -notlike 'api-ms-win-*' })
    if ($unexpected.Count -gt 0) {
        throw "The executable depends on non-system DLLs: $($unexpected -join ', ')"
    }
    Write-Host "DLL imports: $($imports -join ', ')"

    if (Test-Path -LiteralPath $package) { Remove-Item -LiteralPath $package -Recurse -Force }
    if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
    New-Item -ItemType Directory -Path (Join-Path $package 'model') -Force | Out-Null

    Copy-Item -LiteralPath $exe -Destination $package
    foreach ($name in $ModelFiles.Keys) {
        Copy-Item -LiteralPath (Join-Path $ModelDir $name) -Destination (Join-Path $package 'model')
    }
    Copy-Item -LiteralPath (Join-Path $root 'packaging\README.md') -Destination $package

    $notices = @(Get-Content -LiteralPath (Join-Path $root 'packaging\LICENSE-NOTICES.header.md'))
    $notices += Get-CrateNotices
    Set-Content -LiteralPath (Join-Path $package 'LICENSE-NOTICES.md') -Value $notices -Encoding utf8NoBOM

    Write-Host 'Compressing'
    New-ReproducibleZip $package $zip

    Write-Host "Package: $package"
    Write-Host "Zip:     $zip ($([math]::Round((Get-Item -LiteralPath $zip).Length / 1MB)) MiB)"
    Write-Host "SHA-256: exe $((Get-FileHash -LiteralPath (Join-Path $package 'swifty-speech.exe') -Algorithm SHA256).Hash)"
    Write-Host "SHA-256: zip $((Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash)"
}
finally {
    Pop-Location
    $env:CARGO_ENCODED_RUSTFLAGS = $previousRustFlags
    $env:SHERPA_ONNX_ARCHIVE_DIR = $previousArchiveDir
}
