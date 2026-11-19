param(
    [string]$Out = "docs\screenshots\after.png",
    [int]$Delay = 3,
    [string]$Exe = "target\release\claude-deck.exe",
    [string]$Title = "Claude Deck"
)
# Launches the app, waits, captures only its window to a PNG, then closes it.
# P/Invoke is defined in memory (no Add-Type/csc, which writes a temp dll).
$root = Split-Path -Parent $PSScriptRoot
Add-Type -AssemblyName System.Drawing

$asm = [AppDomain]::CurrentDomain.DefineDynamicAssembly((New-Object Reflection.AssemblyName 'Win32Cap'), 'Run')
$tb = $asm.DefineDynamicModule('m').DefineType('U', 'Public,Class')
function Add-Import($name, $ret, [Type[]]$params) {
    $m = $tb.DefinePInvokeMethod($name, 'user32.dll', 'Public,Static,PinvokeImpl', 'Standard', $ret, $params, 'Winapi', 'Unicode')
    $m.SetImplementationFlags($m.GetMethodImplementationFlags() -bor 128)  # PreserveSig
}
Add-Import 'FindWindowW' ([IntPtr]) @([string], [string])
Add-Import 'GetWindowRect' ([bool]) @([IntPtr], [int[]])
Add-Import 'SetForegroundWindow' ([bool]) @([IntPtr])
Add-Import 'ShowWindow' ([bool]) @([IntPtr], [int])
Add-Import 'SetProcessDPIAware' ([bool]) @()
$U = $tb.CreateType()
$U::SetProcessDPIAware() | Out-Null

$exePath = if ([IO.Path]::IsPathRooted($Exe)) { $Exe } else { Join-Path $root $Exe }
$outPath = if ([IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $root $Out }
New-Item -ItemType Directory -Force (Split-Path -Parent $outPath) | Out-Null

$proc = Start-Process -FilePath $exePath -PassThru
Start-Sleep -Seconds $Delay
try {
    $proc.Refresh(); $h = $proc.MainWindowHandle
    if ($h -eq [IntPtr]::Zero) { $h = $U::FindWindowW($null, $Title) }
    if ($h -eq [IntPtr]::Zero) { throw "Window '$Title' not found" }
    $U::ShowWindow($h, 9) | Out-Null
    $U::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Milliseconds 600
    $r = New-Object 'int[]' 4
    $U::GetWindowRect($h, $r) | Out-Null
    $w = $r[2] - $r[0]; $hh = $r[3] - $r[1]
    $bmp = New-Object System.Drawing.Bitmap $w, $hh
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r[0], $r[1], 0, 0, $bmp.Size)
    $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Output "Saved $outPath (${w}x${hh})"
}
finally {
    if (-not $proc.HasExited) {
        $proc.CloseMainWindow() | Out-Null
        if (-not $proc.WaitForExit(3000)) { $proc.Kill() }
    }
}
