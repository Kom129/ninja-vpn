# Проверка мотора, пока другой VPN (например, Durev VPN) выключен.
# Без VPN Claude недоступен, поэтому скрипт всё делает сам и пишет итог в runtime\check-log.txt.
# Запуск:  powershell -ExecutionPolicy Bypass -File scripts\check-without-other-vpn.ps1
param([double]$WaitMinutes = 5, [string]$Server = '')
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root 'target\debug\ninja.exe'
$log = Join-Path $root 'runtime\check-log.txt'
New-Item -ItemType Directory -Force (Join-Path $root 'runtime') | Out-Null
if (-not (Test-Path $exe)) { Write-Host "Не найден $exe — сначала нужно собрать проект (cargo build)."; exit 1 }

# Включённые адаптеры VPN: sing-tun, Wintun, WireGuard, TAP, Amnezia…
function Get-OtherVpn {
    Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and $_.InterfaceDescription -match 'tun|Wintun|WireGuard|TAP|Amnezia|VPN' }
}

$vpn = Get-OtherVpn
if ($vpn) {
    Write-Host "Сейчас включён VPN: $(($vpn | ForEach-Object { $_.Name }) -join ', ')"
    Write-Host "1) Выключи его (кнопка «Отключить»). Жду до $WaitMinutes мин…"
    $deadline = (Get-Date).AddMinutes($WaitMinutes)
    while (Get-OtherVpn) {
        if ((Get-Date) -gt $deadline) { Write-Host 'VPN так и не выключился — проверка отменена, ничего не изменено.'; exit 1 }
        Start-Sleep -Seconds 2
    }
}
Write-Host '2) Другой VPN выключен. Проверяю мотор, это около минуты…'
Start-Sleep -Seconds 3  # даём сети прийти в себя после отключения

"=== $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') — проверка без другого VPN" | Out-File $log -Encoding utf8
"--- refresh" | Out-File $log -Append -Encoding utf8
cmd /c "`"$exe`" refresh >> `"$log`" 2>&1"
"--- connect" | Out-File $log -Append -Encoding utf8
# echo. отправляет Enter: мотор проверит подключение и сразу отключится.
# Без -Server — первый подходящий сервер; можно свой: -Server 19 или -Server нидерл
cmd /c "echo.| `"$exe`" connect $Server >> `"$log`" 2>&1"

Write-Host ''
Get-Content $log -Encoding UTF8
Write-Host ''
Write-Host '3) Готово. Включай свой VPN обратно и напиши Claude «проверил».'
