# Drives the two-process drag on Windows: starts the target and the source,
# moves and presses the pointer with SendInput, and waits for each line the
# programs print before the next step (see src/main.gos). Runs a text drag
# and a file drag; exits non-zero unless both land and report Copy.
param([string]$Exe = ".\target\release\windowing-oledrag.exe")
$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Pointer {
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
    public static void Move(int x, int y) { SetCursorPos(x, y); mouse_event(0x0001, 0, 0, 0, UIntPtr.Zero); }
    public static void Press() { mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero); }
    public static void Release() { mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero); }
}
"@

function Start-Role([string[]]$arguments) {
    $info = New-Object System.Diagnostics.ProcessStartInfo $Exe, ($arguments -join ' ')
    $info.RedirectStandardOutput = $true
    $info.UseShellExecute = $false
    [System.Diagnostics.Process]::Start($info)
}

function Wait-Line($process, [string]$want, [string]$name) {
    while ($null -ne ($line = $process.StandardOutput.ReadLine())) {
        Write-Host "${name}: $line"
        if ($line.StartsWith($want)) { return $line }
    }
    throw "$name ended before '$want'"
}

function Test-Drag([bool]$files) {
    $target = Start-Role @('target')
    Wait-Line $target 'target ready' 'target' | Out-Null
    $source = Start-Role $(if ($files) { @('source', 'files') } else { @('source') })
    Wait-Line $source 'source ready' 'source' | Out-Null
    [Pointer]::Move(100, 100); [Pointer]::Press(); [Pointer]::Move(110, 110)
    Wait-Line $source 'starting drag' 'source' | Out-Null
    [Pointer]::Move(500, 100); [Pointer]::Move(510, 110)
    Wait-Line $target 'drop moved' 'target' | Out-Null
    [Pointer]::Release()
    $dropped = Wait-Line $target 'dropped' 'target'
    $finished = Wait-Line $source 'finished:' 'source'
    $want = if ($files) { 'dropped file: C:\windows\win.ini' } else { 'dropped: hello drag' }
    if ($dropped -ne $want -or $finished -ne 'finished: Some(Copy)') {
        throw "drag ended as '$dropped' / '$finished'"
    }
}

Test-Drag $false
Test-Drag $true
Write-Host 'oledrag: text and file drags landed'
