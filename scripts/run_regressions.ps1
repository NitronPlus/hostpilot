<#
回归测试脚本（多场景）

此脚本在仓库根目录运行，用于生成三类测试数据并运行上传回归：
  many_small : 在运行目录创建 tmp/many_small（若存在则跳过），生成 400 个文件，随机大小，最大 16 KB
  few_large  : 在运行目录创建 tmp/few_large（若存在则跳过），生成 10 个文件，随机大小，最小 50 MB，最大 150 MB
  mixed      : 在运行目录创建 tmp/mixed（若存在则跳过），生成 100 个文件，随机大小，最小 16 KB，最大 50 MB
  all        : 将以上三个目录作为多个源，一次性传输（ts tmp/many_small tmp/few_large tmp/mixed remote:/path）

用法示例（PowerShell，项目根）：
  .\scripts\run_regressions.ps1 -Alias hdev -RemoteTarget '~/dist' -Scenarios @('many_small','few_large','mixed') -Runs @( @{Concur=8; Name='concur8'} , @{Concur=16; Name='concur16'} )

脚本行为：
#>

param(
    [Parameter(Mandatory=$true)]
将失败文件列表写入 `scripts/logs/<scenario>_<run>.failures.txt`（程序会在用户主目录的 HostPilot 日志目录 `~/.hostpilot/logs/` 中追加失败项；本脚本仍会把运行摘要写入本地 logs 以便汇总）
    [Parameter(Mandatory=$true)]
    [string]$RemoteTarget,
    [Parameter(Mandatory=$false)]
    [string[]]$Scenarios = @('many_small','few_large','mixed'),
    [Parameter(Mandatory=$false)]
    [array]$Runs = @(@{Concur=8; Name='concur8'}, @{Concur=16; Name='concur16'})
)

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
# repo root is parent of scripts dir
$repoRoot = Split-Path -Parent $scriptRoot
$logsDir = Join-Path $scriptRoot 'logs'
if (-not (Test-Path $logsDir)) { New-Item -ItemType Directory -Path $logsDir | Out-Null }

# tmp directory under repository root as requested
$tmpRoot = Join-Path $repoRoot 'tmp'
if (-not (Test-Path $tmpRoot)) { New-Item -ItemType Directory -Path $tmpRoot | Out-Null }

function Ensure-Dir([string]$p) {
    if (-not (Test-Path $p)) { New-Item -ItemType Directory -Path $p | Out-Null }
}

# Create a random file. For large sizes use fsutil to avoid allocating large arrays in memory.
function New-RandomFile([string]$path, [long]$sizeBytes) {
    $dir = Split-Path -Parent $path
    Ensure-Dir $dir
    if ($sizeBytes -le 0) { New-Item -ItemType File -Path $path | Out-Null; return }
    if ($sizeBytes -ge 1048576) {
        # Use fsutil to create a zero-filled file of requested size (fast and low memory)
        # fsutil requires size in bytes
        $fsutil = Get-Command fsutil -ErrorAction SilentlyContinue
        if ($null -ne $fsutil) {
            # fsutil file createnew <filename> <length>
            & fsutil file createnew $path $sizeBytes | Out-Null
            return
        }
    }
    # Small files: write random bytes
    $rnd = New-Object System.Random
    $bytes = New-Object 'System.Byte[]' ([int]$sizeBytes)
    $rnd.NextBytes($bytes)
    [System.IO.File]::WriteAllBytes($path, $bytes)
}

function Create-Scenario-ManySmall([string]$dir) {
    $count = 400
    Ensure-Dir $dir
    $existing = (Get-ChildItem -Path $dir -File -Recurse -ErrorAction SilentlyContinue | Measure-Object).Count
    if ($existing -ge $count) { Write-Output "Skipping generation for $dir (already has $existing files)."; return }
    Write-Output "Generating many_small: $count files under $dir"
    for ($i = 1; $i -le $count; $i++) {
        $size = Get-Random -Minimum 1 -Maximum 16384
        $path = Join-Path $dir ("file_{0:D4}.bin" -f $i)
        New-RandomFile -path $path -sizeBytes $size
    }
}

function Create-Scenario-FewLarge([string]$dir) {
    $count = 10
    Ensure-Dir $dir
    $existing = (Get-ChildItem -Path $dir -File -Recurse -ErrorAction SilentlyContinue | Measure-Object).Count
    if ($existing -ge $count) { Write-Output "Skipping generation for $dir (already has $existing files)."; return }
    Write-Output "Generating few_large: $count files (50MB-150MB) under $dir"
    for ($i = 1; $i -le $count; $i++) {
        $size = Get-Random -Minimum (50MB) -Maximum ((150MB)+1)
        $path = Join-Path $dir ("file_{0:D2}.bin" -f $i)
        New-RandomFile -path $path -sizeBytes $size
    }
}

function Create-Scenario-Mixed([string]$dir) {
    $count = 100
    Ensure-Dir $dir
    $existing = (Get-ChildItem -Path $dir -File -Recurse -ErrorAction SilentlyContinue | Measure-Object).Count
    if ($existing -ge $count) { Write-Output "Skipping generation for $dir (already has $existing files)."; return }
    Write-Output "Generating mixed: $count files (16KB-50MB) under $dir"
    for ($i = 1; $i -le $count; $i++) {
        $size = Get-Random -Minimum 16384 -Maximum ((50MB)+1)
        $path = Join-Path $dir ("file_{0:D3}.bin" -f $i)
        New-RandomFile -path $path -sizeBytes $size
    }
}

function Prepare-Scenario([string]$name) {
    switch ($name) {
        'many_small' { return (Join-Path $tmpRoot 'many_small') }
        'few_large'  { return (Join-Path $tmpRoot 'few_large') }
        'mixed'      { return (Join-Path $tmpRoot 'mixed') }
        default      { throw "Unknown scenario: $name" }
    }
}

# Helper: run a single scenario with given local sources and a run config
function Run-Scenario($sources, $concurrency, $name) {
    $logFile = Join-Path $logsDir "$name.log"
    $failFile = Join-Path $logsDir "$name.failures.txt"

    # Use hp.exe directly from PATH (user indicated hp.exe is on PATH)
    $exeCmd = 'hp.exe'
    $argsPrefix = @()
    $remoteSpec = "{0}:{1}" -f $Alias, $RemoteTarget
    # failures are written unconditionally to the user's HostPilot logs dir (~/.hostpilot/logs/)
    $procArgs = $argsPrefix + @('ts') + $sources + @($remoteSpec, '--concurrency', "$concurrency", '--verbose')

    # safe join: quote any arg that contains whitespace or special chars
    function Join-Args($a) {
        $parts = @()
        foreach ($x in $a) {
            if ($null -eq $x) { continue }
            $s = [string]$x
            if ($s -match '\s' -or $s -match '["`$]') {
                $parts += '"' + ($s -replace '"','\"') + '"'
            } else {
                $parts += $s
            }
        }
        return $parts -join ' '
    }

    $cmdLine = (Join-Args $procArgs)
    Write-Output "Running scenario $name (concurrency=$concurrency) -> $logFile"
    Write-Output "Command: $exeCmd $cmdLine"
    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $exeCmd
    $startInfo.Arguments = $cmdLine
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.UseShellExecute = $false

    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $startInfo
    $outBuilder = New-Object System.Text.StringBuilder
    $errBuilder = New-Object System.Text.StringBuilder

    $proc.Start() | Out-Null
    $stdOut = $proc.StandardOutput
    $stdErr = $proc.StandardError

    while (-not $proc.HasExited) {
        Start-Sleep -Milliseconds 200
        while (-not $stdOut.EndOfStream) {
            $line = $stdOut.ReadLine()
            $outBuilder.AppendLine($line) | Out-Null
            Write-Host $line
        }
        while (-not $stdErr.EndOfStream) {
            $line = $stdErr.ReadLine()
            $errBuilder.AppendLine($line) | Out-Null
            Write-Host $line -ForegroundColor Red
        }
    }
    while (-not $stdOut.EndOfStream) { $outBuilder.AppendLine($stdOut.ReadLine()) | Out-Null }
    while (-not $stdErr.EndOfStream) { $errBuilder.AppendLine($stdErr.ReadLine()) | Out-Null }

    # prepend a short header to the log with command and exit code
    $header = "# Command: $exeCmd $cmdLine`n# ExitCode: $($proc.ExitCode)`n# --- STDOUT ---`n"
    $header + $outBuilder.ToString() | Out-File -FilePath $logFile -Encoding utf8
    $errBuilder.ToString() | Out-File -FilePath ($logFile + '.err') -Encoding utf8

    # Copy debug.log if exists
    $homeDir = [Environment]::GetFolderPath('UserProfile')
    $hpLog = Join-Path $homeDir ".hostpilot\logs\debug.log"
    if (Test-Path $hpLog) {
        Copy-Item -Path $hpLog -Destination (Join-Path $logsDir ("$name.debug.log")) -Force
    }

    Write-Output "Scenario $name finished. ExitCode=$($proc.ExitCode). Logs: $logFile"
}

# Prepare requested scenarios (generate files if missing)
foreach ($s in $Scenarios) {
    if ($s -eq 'all') { continue }
    $dir = Prepare-Scenario $s
    switch ($s) {
        'many_small' { Create-Scenario-ManySmall $dir }
        'few_large'  { Create-Scenario-FewLarge $dir }
        'mixed'      { Create-Scenario-Mixed $dir }
        default      { Write-Warning "Unknown scenario $s - skipping generation" }
    }
}

# Run combinations: for each scenario and each run config
foreach ($s in $Scenarios) {
    if ($s -eq 'all') {
        # sources are all three scenario dirs
        $sources = @( (Prepare-Scenario 'many_small'), (Prepare-Scenario 'few_large'), (Prepare-Scenario 'mixed') )
        foreach ($r in $Runs) {
            $name = "all_{0}" -f $r.Name
            Run-Scenario -sources $sources -concurrency $r.Concur -name $name
        }
        continue
    }

    $local = Prepare-Scenario $s
    foreach ($r in $Runs) {
        $name = "{0}_{1}" -f $s, $r.Name
        Run-Scenario -sources @($local) -concurrency $r.Concur -name $name
    }
}

Write-Output "All scenarios complete. Logs in: $logsDir"
