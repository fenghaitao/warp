#!/usr/bin/env powershell
#
# Windows entrypoint for running a local Warp build.
#
# Mirrors the logic of `script/run` for native PowerShell on Windows.
# Invokes `cargo run` directly after setting up channel config and features.

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $repoRoot

# Ensure protoc is on PATH (installed by bootstrap.ps1 to LOCALAPPDATA\protoc).
$protocBinDir = "$env:LOCALAPPDATA\protoc\bin"
if ((Test-Path $protocBinDir) -and ($env:PATH -notlike "*$protocBinDir*")) {
    $env:PATH += ";$protocBinDir"
}

# Git for Windows can be installed system-wide (Program Files) or per-user (LOCALAPPDATA\Programs\Git).
$gitBinCandidates = @(
    "$env:PROGRAMFILES\Git\bin",
    "$env:LOCALAPPDATA\Programs\Git\bin"
)
$gitBinDir = $gitBinCandidates | Where-Object { Test-Path -PathType Container $_ } | Select-Object -First 1

# Try to install/update the internal channel config binary via bash.
if ($gitBinDir) {
    & "$gitBinDir\bash.exe" "$repoRoot\script\install_channel_config"
    if ($LASTEXITCODE -ne 0) {
        Write-Output 'Skipping internal channel config installation (no repo access).'
    }
} else {
    Write-Output 'Skipping internal channel config installation (Git for Windows not found).'
}

# If warp-channel-config is on PATH, build the Local channel binary; otherwise build the OSS channel.
if (Get-Command -Name 'warp-channel-config' -Type Application -ErrorAction SilentlyContinue) {
    $warpBinName = 'warp'
    $env:WARP_CHANNEL = 'local'
} else {
    $warpBinName = 'warp-oss'
    $env:WARP_CHANNEL = 'oss'
}

$features = 'gui'
$cargoParams = @()
$warpArgs = @()

# Parse arguments.
$i = 0
while ($i -lt $args.Count) {
    switch ($args[$i]) {
        '--features' {
            if (($i + 1) -lt $args.Count) {
                $features = "$features,$($args[$i + 1])"
                $i += 2
            } else {
                Write-Error "Error: Argument for --features is missing"
                exit 1
            }
        }
        '--host-id' {
            if (($i + 1) -lt $args.Count) {
                $env:WARP_CLOUD_MODE_DEFAULT_HOST = $args[$i + 1]
                $i += 2
            } else {
                Write-Error "Error: Argument for --host-id is missing"
                exit 1
            }
        }
        '--release' {
            $cargoParams += '--release'
            $i++
        }
        '--profile' {
            if (($i + 1) -lt $args.Count) {
                $cargoParams += '--profile'
                $cargoParams += $args[$i + 1]
                $i += 2
            } else {
                Write-Error "Error: Argument for --profile is missing"
                exit 1
            }
        }
        '--' {
            $warpArgs = $args[($i + 1)..($args.Count - 1)]
            $i = $args.Count
        }
        default {
            # Unknown flags are silently ignored (e.g. macOS-only --dont-open).
            $i++
        }
    }
}

# These cargo features were removed and replaced by environment variables read
# by warp-channel-config. Intercept them here so that existing --features
# invocations keep working.
$featureMappings = @{
    'with_local_server'                  = 'WITH_LOCAL_SERVER'
    'with_local_session_sharing_server'  = 'WITH_LOCAL_SESSION_SHARING_SERVER'
    'with_sandbox_telemetry'             = 'WITH_SANDBOX_TELEMETRY'
}

foreach ($feature in $featureMappings.Keys) {
    $featureList = $features -split ','
    if ($featureList -contains $feature) {
        $envVar = $featureMappings[$feature]
        Set-Item -Path "env:$envVar" -Value '1'
        $featureList = $featureList | Where-Object { $_ -ne $feature }
        $features = $featureList -join ','
        Write-Output "Note: '$feature' is no longer a cargo feature; setting ${envVar}=1 instead."
    }
}

$env:FEATURES = $features
$env:WARP_BIN_NAME = $warpBinName

Write-Output "Running cargo run --bin $warpBinName --features `"$features`" $($cargoParams -join ' ')"

if ($warpArgs.Count -gt 0) {
    cargo run --bin $warpBinName --features $features @cargoParams -- @warpArgs
} else {
    cargo run --bin $warpBinName --features $features @cargoParams
}

exit $LASTEXITCODE
