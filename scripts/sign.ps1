param(
    [Parameter(Mandatory = $true)][string]$CertificateBase64,
    [Parameter(Mandatory = $true)][string]$Password,
    [Parameter(Mandatory = $true)][string]$Path
)

# Authenticode signing for release binaries.
# Requires a code-signing certificate in base64 PFX format and Windows SDK signtool.exe.
$ErrorActionPreference = 'Stop'

$pfx = Join-Path $env:TEMP 'mtkclient-signing.pfx'
[System.IO.File]::WriteAllBytes($pfx, [System.Convert]::FromBase64String($CertificateBase64))

try {
    $signtool = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
    if (-not $signtool) {
        $candidates = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe' -ErrorAction SilentlyContinue
        $signtool = $candidates | Select-Object -Last 1 -ExpandProperty FullName
    }
    if (-not $signtool) { throw 'signtool.exe not found; install the Windows SDK.' }

    & $signtool sign /f $pfx /p $Password /tr 'http://timestamp.digicert.com' /td sha256 /fd sha256 $Path
    if ($LASTEXITCODE -ne 0) { throw "signtool failed with exit code $LASTEXITCODE" }
    & $signtool verify /pa $Path
    if ($LASTEXITCODE -ne 0) { throw "signature verification failed with exit code $LASTEXITCODE" }
}
finally {
    Remove-Item $pfx -Force -ErrorAction SilentlyContinue
}
