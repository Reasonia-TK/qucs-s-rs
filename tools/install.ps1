<#
.SYNOPSIS
  Install Qucs-S RS for the current user (no administrator rights needed).

.DESCRIPTION
  Copies qucs-s-rs.exe to %LOCALAPPDATA%\Programs\qucs-s-rs, adds a Start menu
  shortcut and (with -Associate) opens .sch files with Qucs-S RS.
  Run it from the unpacked release folder or from the repository after
  `cargo build --release`.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File install.ps1
  powershell -ExecutionPolicy Bypass -File install.ps1 -Associate
#>
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA "Programs\qucs-s-rs"),
    [switch]$Associate,
    [switch]$NoShortcut,
    [switch]$AddToPath
)
$ErrorActionPreference = "Stop"

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$candidates = @(
    (Join-Path $here "qucs-s-rs.exe"),
    (Join-Path $here "..\qucs-s-rs.exe"),
    (Join-Path $here "..\target\release\qucs-s-rs.exe")
)
$exe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $exe) { throw "qucs-s-rs.exe not found next to this script (build with: cargo build --release)" }

New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
Copy-Item $exe (Join-Path $Prefix "qucs-s-rs.exe") -Force
foreach ($f in @("README.md", "README.en.md", "LICENSE", "CHANGELOG.md")) {
    $src = @((Join-Path $here $f), (Join-Path $here "..\$f")) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($src) { Copy-Item $src $Prefix -Force }
}
$target = Join-Path $Prefix "qucs-s-rs.exe"
Write-Host "installed: $target"

if (-not $NoShortcut) {
    $menu = Join-Path ([Environment]::GetFolderPath("Programs")) "Qucs-S RS.lnk"
    $sh = New-Object -ComObject WScript.Shell
    $lnk = $sh.CreateShortcut($menu)
    $lnk.TargetPath = $target
    $lnk.Arguments = "gui"
    $lnk.IconLocation = "$target,0"
    $lnk.Description = "Qucs-S RS schematic editor"
    $lnk.Save()
    Write-Host "Start menu: $menu"
}

if ($Associate) {
    # per-user file association (HKCU), Qucs-S keeps working via "Open with"
    $cls = "HKCU:\Software\Classes"
    New-Item -Force "$cls\QucsSRS.Schematic\shell\open\command" | Out-Null
    Set-ItemProperty "$cls\QucsSRS.Schematic" -Name "(default)" -Value "Qucs schematic"
    New-Item -Force "$cls\QucsSRS.Schematic\DefaultIcon" | Out-Null
    Set-ItemProperty "$cls\QucsSRS.Schematic\DefaultIcon" -Name "(default)" -Value "`"$target`",0"
    Set-ItemProperty "$cls\QucsSRS.Schematic\shell\open\command" -Name "(default)" -Value "`"$target`" gui `"%1`""
    New-Item -Force "$cls\.sch\OpenWithProgids" | Out-Null
    New-ItemProperty -Force -Path "$cls\.sch\OpenWithProgids" -Name "QucsSRS.Schematic" -Value "" -PropertyType String | Out-Null
    Write-Host ".sch files: 'Open with' -> Qucs-S RS (choose it once in Explorer to make it the default)"
}

if ($AddToPath) {
    $p = [Environment]::GetEnvironmentVariable("Path", "User")
    if (($p -split ";") -notcontains $Prefix) {
        [Environment]::SetEnvironmentVariable("Path", ($p.TrimEnd(";") + ";" + $Prefix), "User")
        Write-Host "added to the user PATH (new terminals): $Prefix"
    }
}

Write-Host ""
Write-Host "MCP (Claude Code):  claude mcp add -s user qucs-s-rs -- `"$target`" mcp"
