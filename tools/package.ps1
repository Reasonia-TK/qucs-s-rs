<#
.SYNOPSIS
  Build the release binary and pack a distributable zip into dist\.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\package.ps1
#>
param([switch]$NoBuild)
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) "..")
Set-Location $root

if (-not $NoBuild) {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}
$ver = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$name = "qucs-s-rs-$ver-windows-x64"
$stage = Join-Path $root "dist\$name"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage, "$stage\examples", "$stage\docs" | Out-Null

Copy-Item target\release\qucs-s-rs.exe $stage
Copy-Item README.md, README.en.md, LICENSE, CHANGELOG.md $stage
Copy-Item tools\install.ps1, tools\uninstall.ps1 $stage
Copy-Item examples\*.sch "$stage\examples"
Copy-Item docs\*.md, docs\*.png "$stage\docs"

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "packed $zip ($size MB)"
