# Builds dist\SpaceScope-<Name>.zip
#   -Target  Rust target triple (default: the host, 64-bit)
#   -Name    suffix of the zip, e.g. windows-x64 or windows-x86
#   -ExeDir  take spacescope.exe and sscan.exe from here instead of building
#            (used by CI to package the signed binaries)
param(
    [string]$Target = "",
    [string]$Name = "windows-x64",
    [string]$ExeDir = ""
)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
if (-not $ExeDir) {
    if ($Target) {
        cargo build --release --target $Target
        $ExeDir = "target\$Target\release"
    } else {
        cargo build --release
        $ExeDir = "target\release"
    }
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
}
$stage = "dist\SpaceScope-$Name"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$ExeDir\spacescope.exe", "$ExeDir\sscan.exe", README.md, CHANGELOG.md, LICENSE, NOTICE $stage
Compress-Archive -Path "$stage\*" -DestinationPath "$stage.zip" -Force
Remove-Item $stage -Recurse -Force
Get-Item "$stage.zip" | Select-Object Name, Length
