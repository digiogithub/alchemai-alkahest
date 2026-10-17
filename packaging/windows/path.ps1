<#
.SYNOPSIS
  Adds or removes a directory in the persistent PATH (ALK-US-0076). Shipped as
  `tools\path.ps1` by the Windows installer, which calls it for the "Add to PATH" component
  and again from the uninstaller.

.DESCRIPTION
  Works on the registry value directly with `DoNotExpandEnvironmentNames` and writes it back
  as REG_EXPAND_SZ. `[Environment]::SetEnvironmentVariable` would expand `%SystemRoot%` and
  friends and store the result as plain REG_SZ for good. The edit is idempotent: the directory
  is dropped from the list first (trailing backslashes and case ignored) and appended once for
  `add`. Running programs are told with WM_SETTINGCHANGE; terminals that are already open keep
  their old PATH.

.PARAMETER Action
  `add` or `remove`.
.PARAMETER Dir
  The directory to add or remove.
.PARAMETER Scope
  `Machine` (default; needs administrator rights) or `User`.
#>
param(
    [Parameter(Mandatory = $true)][ValidateSet('add', 'remove')][string]$Action,
    [Parameter(Mandatory = $true)][string]$Dir,
    [ValidateSet('Machine', 'User')][string]$Scope = 'Machine'
)

$ErrorActionPreference = 'Stop'

# Splits a PATH value into its non-empty entries.
function Split-PathList([string]$Value) {
    if ([string]::IsNullOrEmpty($Value)) { return @() }
    return @($Value -split ';' | Where-Object { $_ -ne '' })
}

# Compares two directories the way Windows does: case-insensitive, no trailing backslash.
function Test-SameDir([string]$A, [string]$B) {
    return $A.TrimEnd('\') -ieq $B.TrimEnd('\')
}

# Returns the PATH after `add` or `remove`; unchanged entries keep their order and spelling.
function Update-PathList([string]$Current, [string]$Directory, [string]$What) {
    $kept = @(Split-PathList $Current | Where-Object { -not (Test-SameDir $_ $Directory) })
    if ($What -eq 'add') { $kept += $Directory }
    return ($kept -join ';')
}

if ($Scope -eq 'Machine') {
    $root = [Microsoft.Win32.Registry]::LocalMachine
    $subKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
} else {
    $root = [Microsoft.Win32.Registry]::CurrentUser
    $subKey = 'Environment'
}

$key = $root.OpenSubKey($subKey, $true)
if ($null -eq $key) { throw "cannot open the $Scope environment key" }
try {
    $current = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $updated = Update-PathList $current $Dir $Action
    if ($updated -eq $current) {
        Write-Output "PATH already up to date ($Scope)."
        exit 0
    }
    $key.SetValue('Path', $updated, [Microsoft.Win32.RegistryValueKind]::ExpandString)
} finally {
    $key.Close()
}

# Tell running programs (Explorer, new terminals) that the environment changed.
Add-Type -Namespace Alkahest -Name Native -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
$result = [UIntPtr]::Zero
[void][Alkahest.Native]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
Write-Output "PATH updated ($Action $Dir, $Scope). Open a new terminal to use it."
