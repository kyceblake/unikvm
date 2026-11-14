[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Install', 'Run', 'Report', 'Remove')]
    [string] $Action,
    [ValidateRange(0, 60)]
    [int] $DelaySeconds = 0
)

$ErrorActionPreference = 'Stop'
$serviceName = 'UnikvmSecureDesktopProbe'
$installDirectory = Join-Path $env:ProgramFiles $serviceName
$installedBinary = Join-Path $installDirectory 'secure_desktop_probe.exe'
$repository = Split-Path -Parent $PSScriptRoot
$builtBinary = Join-Path $repository 'target\debug\examples\secure_desktop_probe.exe'

function Assert-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this action from an Administrator PowerShell window.'
    }
}

function Show-ProbeReport {
    $events = Get-WinEvent -FilterHashtable @{
        LogName = 'Application'
        ProviderName = $serviceName
    } -MaxEvents 5 -ErrorAction SilentlyContinue
    if (-not $events) {
        Write-Output 'No probe report yet. Run the service, then inspect this report again.'
        return
    }
    foreach ($event in $events) {
        Write-Output ("{0:o} event_id={1}" -f $event.TimeCreated, $event.Id)
        foreach ($property in $event.Properties) {
            Write-Output $property.Value
        }
    }
}

switch ($Action) {
    'Install' {
        Assert-Administrator
        if (-not (Test-Path -LiteralPath $builtBinary -PathType Leaf)) {
            throw "Build first: cargo build --locked -p unikvm-platform --example secure_desktop_probe. Missing $builtBinary"
        }
        if (Get-Service -Name $serviceName -ErrorAction SilentlyContinue) {
            throw 'Probe service already exists. Use Run/Report, or explicitly Remove before replacing it.'
        }
        if (Test-Path -LiteralPath $installDirectory) {
            throw "Installation directory already exists; inspect it before proceeding: $installDirectory"
        }
        New-Item -ItemType Directory -Path $installDirectory | Out-Null
        # Only SYSTEM/Administrators may replace the installed executable. Users
        # get read/execute access; pairing data and the network agent are untouched.
        & icacls.exe $installDirectory /inheritance:r /grant:r `
            '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Could not protect the probe installation directory.' }
        Copy-Item -LiteralPath $builtBinary -Destination $installedBinary
        if (-not [Diagnostics.EventLog]::SourceExists($serviceName)) {
            [Diagnostics.EventLog]::CreateEventSource($serviceName, 'Application')
        }
        $command = '"{0}" --service' -f $installedBinary
        # New-Service uses LocalSystem unless credentials are explicitly supplied.
        New-Service -Name $serviceName -DisplayName 'unikvm secure desktop feasibility probe' `
            -Description 'Capability-only Winlogon worker probe. Never injects input.' `
            -BinaryPathName $command -StartupType Manual | Out-Null
        $service = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
        if ($service.StartName -ne 'LocalSystem') {
            throw 'Unexpected service identity; do not start the probe.'
        }
        Write-Output 'Installed manual LocalSystem capability probe. No input is injected.'
        Write-Output 'Next: -Action Run -DelaySeconds 15, press Win+L during the countdown, then unlock physically and use -Action Report.'
    }
    'Run' {
        Assert-Administrator
        if ($DelaySeconds -gt 0) {
            Write-Output "Starting probe in $DelaySeconds seconds. You may now lock Windows with Win+L."
            Start-Sleep -Seconds $DelaySeconds
        }
        Start-Service -Name $serviceName
        $deadline = [DateTime]::UtcNow.AddSeconds(25)
        do {
            Start-Sleep -Milliseconds 200
            $service = Get-Service -Name $serviceName
            $state = $service.Status
            $service.Dispose()
            if ([DateTime]::UtcNow -gt $deadline) {
                throw 'Probe has not stopped. Inspect the service and Application event log.'
            }
        } while ($state -ne 'Stopped')
        Show-ProbeReport
    }
    'Report' {
        Show-ProbeReport
    }
    'Remove' {
        Assert-Administrator
        $service = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
        if ($service) {
            if ($service.Status -ne 'Stopped') {
                Stop-Service -Name $serviceName
                $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20))
            }
            $service.Dispose()
            & sc.exe delete $serviceName | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Could not remove the probe service.' }
        }
        if (Test-Path -LiteralPath $installedBinary -PathType Leaf) {
            Remove-Item -LiteralPath $installedBinary
        }
        if ((Test-Path -LiteralPath $installDirectory -PathType Container) -and
            -not (Get-ChildItem -LiteralPath $installDirectory -Force)) {
            Remove-Item -LiteralPath $installDirectory
        }
        Write-Output 'Probe service and installed executable removed. Event history, network agent, and pairing data preserved.'
    }
}
