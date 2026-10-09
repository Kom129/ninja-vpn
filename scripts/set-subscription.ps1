# Берёт ссылку-подписку (https://…) или ключ vless://… из буфера обмена и записывает в .env.
# Сама ссылка на экран не выводится.
# Запуск:  powershell -ExecutionPolicy Bypass -File scripts\set-subscription.ps1
# Параметр -Value нужен только для проверки скрипта без буфера обмена.
param([string]$Value)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$envPath = Join-Path $root '.env'

$value = if ($Value) { $Value } else { Get-Clipboard -Raw }
if ($null -eq $value) { $value = '' }
$value = $value.Trim()

if ($value -match '^https://\S+$' -and $value.Length -le 2048) { $key = 'NINJA_SUBSCRIPTION_URL' }
elseif ($value -match '^vless://\S+$' -and $value.Length -le 4096) { $key = 'NINJA_VLESS_KEY' }
else {
    Write-Host 'В буфере обмена нет ссылки https://… или ключа vless://. Скопируй её (Ctrl+C) и запусти снова.'
    exit 1
}

# Меняем только нужную строку, остальное в .env оставляем как было.
$lines = @()
if (Test-Path $envPath) { $lines = [IO.File]::ReadAllLines($envPath) }
$found = $false
$lines = @(foreach ($line in $lines) {
    if ($line -match "^\s*$key\s*=") { $found = $true; "$key=$value" } else { $line }
})
if (-not $found) { $lines += "$key=$value" }
[IO.File]::WriteAllLines($envPath, [string[]]$lines, (New-Object Text.UTF8Encoding $false))
Write-Host "Готово: $key сохранён в .env ($($value.Length) символов). Сама ссылка не показывается."
