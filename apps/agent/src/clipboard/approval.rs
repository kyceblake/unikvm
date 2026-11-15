use std::process::{Child, Command, Stdio};

use anyhow::Result;
use ring::rand::{SecureRandom, SystemRandom};
use unikvm_protocol::clipboard::Layout;
use unikvm_protocol::local::Approval;

pub(super) fn make_approval(layout: &Layout, bytes: u64) -> Result<Approval> {
    let mut nonce = [0; 8];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow::anyhow!("clipboard approval ID unavailable"))?;
    let label = match layout {
        Layout::Files(files) if files.len() == 1 => files[0].name.clone(),
        Layout::Files(files) => format!("{} files", files.len()),
        Layout::Png(_) => "Clipboard image".to_owned(),
        Layout::Text { .. } => "Clipboard text".to_owned(),
    };
    Ok(Approval {
        id: u64::from_ne_bytes(nonce),
        bytes,
        label,
    })
}

pub(super) struct Prompt(Child);

impl Prompt {
    pub(super) fn launch(id: u64) -> Result<Self> {
        let binary = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "unikvm.exe"
        } else {
            "unikvm"
        });
        let mut command = Command::new(binary);
        command
            .arg("clipboard-approval")
            .arg(id.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Keep the console hidden on Windows. The helper creates its own desktop window.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        Ok(Self(command.spawn()?))
    }
}

impl Drop for Prompt {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
