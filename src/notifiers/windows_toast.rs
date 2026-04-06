/// Windows-native toast notifications via PowerShell EncodedCommand.
/// Non-blocking: spawns a background PowerShell process and returns immediately.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "windows")]
    {
        let safe_title = sanitize(title, 63);
        let safe_body = sanitize(body, 200);

        let script = format!(
            r#"Add-Type -AssemblyName System.Windows.Forms
$n = [System.Windows.Forms.NotifyIcon]::new()
$n.Icon = [System.Drawing.SystemIcons]::Warning
$n.Visible = $true
$n.ShowBalloonTip(10000,'{title}','{body}',[System.Windows.Forms.ToolTipIcon]::Warning)
Start-Sleep -Milliseconds 11000
$n.Visible = $false"#,
            title = safe_title,
            body = safe_body
        );

        // Encode as UTF-16LE Base64 for -EncodedCommand (avoids all shell escaping issues)
        let utf16: Vec<u16> = script.encode_utf16().collect();
        let bytes: Vec<u8> = utf16.iter().flat_map(|w| w.to_le_bytes()).collect();
        let encoded = base64_encode(&bytes);

        let _ = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-EncodedCommand",
                &encoded,
            ])
            .spawn();
    }

    #[cfg(not(target_os = "windows"))]
    let _ = (title, body);
}

/// Strip or replace only characters that break PowerShell single-quoted strings, then truncate.
/// Because we use -EncodedCommand (UTF-16LE Base64), full Unicode (emoji etc.) is fine.
fn sanitize(s: &str, max_chars: usize) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\x00'..='\x08' | '\x0B' | '\x0C' | '\x0E'..='\x1F'))
        // Replace any single-quote variant with a backtick (safe in PS single-quoted strings)
        .map(|c| match c {
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '`',
            _ => c,
        })
        .take(max_chars)
        .collect()
}

/// Minimal Base64 encoder (no external crate needed).
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((combined >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((combined >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((combined >> 6) & 0x3F) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(combined & 0x3F) as usize] as char } else { '=' });
    }
    out
}
