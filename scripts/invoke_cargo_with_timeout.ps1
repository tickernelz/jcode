param(
    [Parameter(Mandatory=$true)]
    [ValidateNotNullOrEmpty()]
    [string]$Name,

    [Parameter(Mandatory=$true)]
    [ValidateNotNullOrEmpty()]
    [string[]]$CargoArgs,

    [ValidateRange(1, 86400)]
    [int]$TimeoutSeconds = 300,

    [switch]$RequireTests
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Stop-ProcessTree {
    param(
        [Parameter(Mandatory=$true)]
        [System.Diagnostics.Process]$Process
    )

    if ($Process.HasExited) {
        return
    }

    $taskkill = Get-Command taskkill.exe -ErrorAction SilentlyContinue
    if ($taskkill) {
        & $taskkill.Source /PID $Process.Id /T /F | ForEach-Object { Write-Host $_ }
        return
    }

    Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
}

$timeoutMilliseconds = $TimeoutSeconds * 1000
$stdoutPath = [System.IO.Path]::GetTempFileName()
$stderrPath = [System.IO.Path]::GetTempFileName()

Write-Host "::group::$Name"
try {
    Write-Host "cargo $($CargoArgs -join ' ')"
    $process = Start-Process -FilePath 'cargo' -ArgumentList $CargoArgs -NoNewWindow -PassThru `
        -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath

    if (-not $process.WaitForExit($timeoutMilliseconds)) {
        Stop-ProcessTree -Process $process
        throw "$Name timed out after $TimeoutSeconds seconds"
    }

    $exitCode = $process.ExitCode
    $output = (Get-Content -Raw $stdoutPath), (Get-Content -Raw $stderrPath) -join "`n"
    Write-Host $output
    if ($exitCode -ne 0) {
        throw "$Name failed with exit code $exitCode"
    }
    if ($RequireTests) {
        $executedTests = [regex]::Matches($output, '(?m)^running (\d+) tests?\r?$') |
            ForEach-Object { [int]$_.Groups[1].Value } |
            Measure-Object -Sum
        if (-not $executedTests.Count -or $executedTests.Sum -eq 0) {
            throw "$Name did not execute any tests"
        }
    }
} finally {
    Remove-Item $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    Write-Host '::endgroup::'
}
