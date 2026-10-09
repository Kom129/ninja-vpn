# Скачивает сетевые ядра из engines/engines.json и проверяет SHA-256.
# Запуск из папки проекта:  powershell -ExecutionPolicy Bypass -File scripts\fetch-engines.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$manifest = Get-Content (Join-Path $root 'engines\engines.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$binDir = Join-Path $root 'engines\bin'
New-Item -ItemType Directory -Force $binDir | Out-Null

foreach ($e in $manifest.engines) {
    $dir = Join-Path $binDir "$($e.name)-$($e.version)"
    $exe = Join-Path $dir $e.exe
    if (Test-Path $exe) { Write-Host "$($e.name) $($e.version): уже есть"; continue }

    $zip = Join-Path $binDir ([IO.Path]::GetFileName($e.url))
    Write-Host "$($e.name) $($e.version): скачиваю $($e.url)"
    Invoke-WebRequest $e.url -OutFile $zip -UseBasicParsing

    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($hash -ne $e.sha256) {
        Remove-Item $zip
        throw "$($e.name): контрольная сумма не совпала (ожидали $($e.sha256), получили $hash). Файл удалён."
    }
    Expand-Archive $zip -DestinationPath $dir -Force
    Remove-Item $zip
    Write-Host "$($e.name) $($e.version): готово, SHA-256 совпала"
}
