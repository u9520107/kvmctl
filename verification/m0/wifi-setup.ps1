if ($PSVersionTable.PSEdition -ne 'Desktop') {
    throw 'Run with Windows PowerShell 5.1 (powershell.exe), not PowerShell 7.'
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()

$form = New-Object System.Windows.Forms.Form
$form.Text = 'kvmctl M0 Wi-Fi setup'
$form.ClientSize = New-Object System.Drawing.Size(560, 300)
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false

$portLabel = New-Object System.Windows.Forms.Label
$portLabel.Text = 'Board COM port'
$portLabel.SetBounds(16, 18, 110, 24)
$form.Controls.Add($portLabel)

$portList = New-Object System.Windows.Forms.ComboBox
$portList.DropDownStyle = 'DropDownList'
$portList.SetBounds(130, 14, 110, 28)
$form.Controls.Add($portList)

$openButton = New-Object System.Windows.Forms.Button
$openButton.Text = 'Open serial'
$openButton.SetBounds(250, 13, 110, 30)
$form.Controls.Add($openButton)

$refreshPorts = New-Object System.Windows.Forms.Button
$refreshPorts.Text = 'Refresh ports'
$refreshPorts.SetBounds(370, 13, 110, 30)
$form.Controls.Add($refreshPorts)

$ssidLabel = New-Object System.Windows.Forms.Label
$ssidLabel.Text = '2.4 GHz Wi-Fi name'
$ssidLabel.SetBounds(16, 62, 150, 24)
$form.Controls.Add($ssidLabel)

$ssidBox = New-Object System.Windows.Forms.TextBox
$ssidBox.SetBounds(170, 58, 350, 26)
$form.Controls.Add($ssidBox)

$passwordLabel = New-Object System.Windows.Forms.Label
$passwordLabel.Text = 'Wi-Fi password'
$passwordLabel.SetBounds(16, 102, 150, 24)
$form.Controls.Add($passwordLabel)

$passwordBox = New-Object System.Windows.Forms.TextBox
$passwordBox.UseSystemPasswordChar = $true
$passwordBox.SetBounds(170, 98, 350, 26)
$form.Controls.Add($passwordBox)

$connectButton = New-Object System.Windows.Forms.Button
$connectButton.Text = 'Connect board'
$connectButton.Enabled = $false
$connectButton.SetBounds(170, 140, 130, 32)
$form.Controls.Add($connectButton)

$checkButton = New-Object System.Windows.Forms.Button
$checkButton.Text = 'Refresh status'
$checkButton.Enabled = $false
$checkButton.SetBounds(310, 140, 130, 32)
$form.Controls.Add($checkButton)

$statusLabel = New-Object System.Windows.Forms.Label
$statusLabel.Text = 'Choose board COM port, then open serial.'
$statusLabel.SetBounds(16, 188, 520, 30)
$form.Controls.Add($statusLabel)

$note = New-Object System.Windows.Forms.Label
$note.Text = 'Credentials travel over the CH343 USB cable. Probe keeps Wi-Fi config in RAM; it does not save it to flash.'
$note.SetBounds(16, 235, 520, 42)
$form.Controls.Add($note)

$script:serial = $null
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 1500

function Update-PortList {
    $selected = [string]$portList.SelectedItem
    $portList.Items.Clear()
    foreach ($name in [System.IO.Ports.SerialPort]::GetPortNames() | Sort-Object) {
        [void]$portList.Items.Add($name)
    }
    if ($selected -and $portList.Items.Contains($selected)) {
        $portList.SelectedItem = $selected
    } elseif ($portList.Items.Contains('COM6')) {
        $portList.SelectedItem = 'COM6'
    } elseif ($portList.Items.Count -gt 0) {
        $portList.SelectedIndex = 0
    }
}

function Get-BoardStatus {
    if (-not $script:serial -or -not $script:serial.IsOpen) { return }
    try {
        $script:serial.DiscardInBuffer()
        $script:serial.WriteLine('STATUS')
        $watch = [System.Diagnostics.Stopwatch]::StartNew()
        while ($watch.ElapsedMilliseconds -lt 1000) {
            try {
                $line = $script:serial.ReadLine().Trim()
                if ($line -match '^M0 STATUS wifi=(?<wifi>\w+) ip=(?<ip>[0-9.]+) mdns=(?<mdns>\w+) reason=(?<reason>\d+)') {
                    $statusLabel.Text = "Wi-Fi: $($Matches.wifi); IP: $($Matches.ip); mDNS: $($Matches.mdns); disconnect reason: $($Matches.reason)"
                    if ($Matches.wifi -eq 'connected') { $connectButton.Enabled = $false }
                    return
                }
            } catch [System.TimeoutException] { }
        }
        $statusLabel.Text = 'No M0 status reply. Check board image and selected COM port.'
    } catch {
        $statusLabel.Text = "Serial read failed: $($_.Exception.Message)"
    }
}

$refreshPorts.Add_Click({ Update-PortList })

$openButton.Add_Click({
    if ($script:serial -and $script:serial.IsOpen) {
        $timer.Stop()
        $script:serial.Close()
        $script:serial.Dispose()
        $script:serial = $null
        $openButton.Text = 'Open serial'
        $connectButton.Enabled = $false
        $checkButton.Enabled = $false
        $statusLabel.Text = 'Serial port closed.'
        return
    }

    if (-not $portList.SelectedItem) {
        $statusLabel.Text = 'No COM port selected.'
        return
    }
    try {
        $script:serial = New-Object System.IO.Ports.SerialPort([string]$portList.SelectedItem, 115200, [System.IO.Ports.Parity]::None, 8, [System.IO.Ports.StopBits]::One)
        $script:serial.DtrEnable = $false
        $script:serial.RtsEnable = $false
        $script:serial.ReadTimeout = 200
        $script:serial.WriteTimeout = 1000
        $script:serial.NewLine = "`n"
        $script:serial.Open()
        $script:serial.DiscardInBuffer()
        $openButton.Text = 'Close serial'
        $connectButton.Enabled = $true
        $checkButton.Enabled = $true
        $statusLabel.Text = "Opened $($portList.SelectedItem) at 115200 baud."
        $timer.Start()
    } catch {
        $statusLabel.Text = "Could not open serial port: $($_.Exception.Message)"
        $script:serial = $null
    }
})

$connectButton.Add_Click({
    if (-not $script:serial -or -not $script:serial.IsOpen) { return }
    $ssidBytes = [System.Text.Encoding]::UTF8.GetBytes($ssidBox.Text)
    $passwordBytes = [System.Text.Encoding]::UTF8.GetBytes($passwordBox.Text)
    if ($ssidBytes.Length -lt 1 -or $ssidBytes.Length -gt 32) {
        $statusLabel.Text = 'Wi-Fi name must contain 1–32 UTF-8 bytes.'
        return
    }
    $passwordValid = ($passwordBytes.Length -eq 0) -or ($passwordBytes.Length -ge 8 -and $passwordBytes.Length -le 63) -or ($passwordBox.Text -match '\A[0-9A-Fa-f]{64}\z')
    if (-not $passwordValid) {
        $statusLabel.Text = 'Password must be blank, 8–63 UTF-8 bytes, or 64 hexadecimal characters.'
        return
    }

    $ssidHex = [BitConverter]::ToString($ssidBytes).Replace('-', '')
    $passwordHex = if ($passwordBytes.Length -eq 0) { '-' } else { [BitConverter]::ToString($passwordBytes).Replace('-', '') }
    try {
        $script:serial.WriteLine("WIFI $ssidHex $passwordHex")
        $ssidBox.Clear()
        $passwordBox.Clear()
        $connectButton.Enabled = $false
        $statusLabel.Text = 'Credentials sent over local USB serial; waiting for Wi-Fi and DHCP status.'
        $timer.Start()
    } catch {
        $statusLabel.Text = "Serial write failed: $($_.Exception.Message)"
    } finally {
        [Array]::Clear($ssidBytes, 0, $ssidBytes.Length)
        [Array]::Clear($passwordBytes, 0, $passwordBytes.Length)
        $ssidHex = $null
        $passwordHex = $null
    }
})

$checkButton.Add_Click({ Get-BoardStatus })
$timer.Add_Tick({ Get-BoardStatus })
$form.Add_FormClosed({
    $timer.Stop()
    if ($script:serial -and $script:serial.IsOpen) {
        $script:serial.Close()
        $script:serial.Dispose()
    }
})

Update-PortList
[void]$form.ShowDialog()
