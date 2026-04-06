# repo-radar startup script
# Starts Redis (if not running) then the repo-radar server

param(
    [int]$Port = 8080
)

$redisExe  = "C:\redis\redis-server.exe"
$redisConf = "C:\redis\redis.windows.conf"
$redisCli  = "C:\redis\redis-cli.exe"
$serverExe = "$PSScriptRoot\target2\debug\repo-radar.exe"

# ── Redis ─────────────────────────────────────────────────────────────────────
$pong = & $redisCli ping 2>$null
if ($pong -ne "PONG") {
    Write-Host "Starting Redis..." -ForegroundColor Cyan
    Start-Process -FilePath $redisExe -ArgumentList $redisConf -WindowStyle Hidden
    Start-Sleep 2
    $pong = & $redisCli ping 2>$null
    if ($pong -ne "PONG") {
        Write-Host "ERROR: Redis failed to start." -ForegroundColor Red
        exit 1
    }
}
Write-Host "Redis OK  ($(& $redisCli DBSIZE) keys)" -ForegroundColor Green

# ── Kill any stale instance ───────────────────────────────────────────────────
Get-Process repo-radar -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 1

# ── Start repo-radar ──────────────────────────────────────────────────────────
Write-Host "Starting repo-radar on port $Port..." -ForegroundColor Cyan
& $serverExe serve --port $Port
