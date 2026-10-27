# Runs the accessibility provider probe against the UI Automation client on
# Windows: the client reads the tree, presses, toggles and sets each control,
# the probe applies the requests, and the client reads the results back.
# Exits non-zero unless every step is seen as expected.
param(
    [string]$Probe = ".\target\release\windowing-uia.exe",
    [string]$Client = ".\uia-client.exe"
)
$ErrorActionPreference = 'Stop'

function Start-Program([string]$path, [string]$arguments, [bool]$withInput) {
    $info = New-Object System.Diagnostics.ProcessStartInfo $path, $arguments
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardInput = $withInput
    $info.UseShellExecute = $false
    [System.Diagnostics.Process]::Start($info)
}

$seen = New-Object System.Collections.Generic.List[string]
function Wait-Line($process, [string]$want, [string]$name) {
    while ($null -ne ($line = $process.StandardOutput.ReadLine())) {
        Write-Host "${name}: $line"
        $seen.Add("${name}: $line")
        if ($line.StartsWith($want)) { return $line }
    }
    throw "$name ended before '$want'"
}

$probe = Start-Program $Probe '' $false
Wait-Line $probe 'probe ready' 'probe' | Out-Null
$client = Start-Program $Client '' $true
Wait-Line $client 'set range' 'client' | Out-Null
Wait-Line $probe 'updated' 'probe' | Out-Null
$client.StandardInput.WriteLine('phase2')
$client.StandardInput.Flush()
Wait-Line $client 'set focus' 'client' | Out-Null
Wait-Line $probe 'focus returned' 'probe' | Out-Null

$expected = @(
    'client: root: UIA probe',
    'client: child: 50000 Save',
    'client: child: 50002 Wrap lines',
    'client: child: 50004 Name',
    'client: child: 50015 Volume',
    'client: save has focus: 1',
    'client: before: toggle 0, name Ada, volume 40',
    'client: invoke: ok',
    'client: toggle: ok',
    'client: set value: ok',
    'client: set range: ok',
    'probe: request: 2 press ',
    'probe: request: 3 press ',
    'probe: request: 4 set-text Grace',
    'probe: request: 5 set-value 70',
    'client: after: toggle 1, name Grace, volume 70',
    'client: set focus: ok'
)
$missing = $expected | Where-Object { -not $seen.Contains($_) }
if ($missing) {
    throw "missing: $($missing -join '; ')"
}
Write-Host 'uia: the client read and drove every control'
