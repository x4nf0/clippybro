
use std::path::PathBuf;
use std::process::Command;

pub fn capture() -> Result<PathBuf, String> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bitmap = New-Object System.Drawing.Bitmap($screen.Width, $screen.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)

try {
    $graphics.CopyFromScreen(
        $screen.Location,
        [System.Drawing.Point]::Empty,
        $screen.Size
    )

    $folder = "D:\Obs"
    [System.IO.Directory]::CreateDirectory($folder) | Out-Null

    $name = 'Screenshot_' + (Get-Date -Format 'yyyyMMdd_HHmmss_fff') + '.png'
    $path = Join-Path $folder $name

    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Output $path
}
finally {
    $graphics.Dispose()
    $bitmap.Dispose()
}
"#;

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let path = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string();

    if path.is_empty() {
        return Err("Screenshot path was not returned".into());
    }

    Ok(PathBuf::from(path))
}
