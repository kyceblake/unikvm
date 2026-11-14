use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use unikvm_network::identity::{Fingerprint, Identity, MAX_IDENTITY_SIZE, TrustedPeer};
use unikvm_platform::credential;

pub(super) const USAGE: &str = "Usage:
  unikvm status
  unikvm identity init PATH
  unikvm identity fingerprint PATH
  unikvm identity export PATH CERTIFICATE
  unikvm peer verify CERTIFICATE FINGERPRINT";

pub(super) fn identity_command(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let command = args.next().context(USAGE)?;
    let path = args.next().context(USAGE)?;
    let export = if command == "export" {
        Some(args.next().context(USAGE)?)
    } else {
        None
    };
    anyhow::ensure!(args.next().is_none(), USAGE);
    let path = Path::new(&path);
    let identity = match command.to_str() {
        Some("init") => {
            let identity = Identity::generate()?;
            credential::create(path, &identity.to_bytes())
                .with_context(|| format!("create identity {}", path.display()))?;
            identity
        }
        Some("fingerprint" | "export") => {
            let bytes = credential::read(path, MAX_IDENTITY_SIZE)
                .with_context(|| format!("read identity {}", path.display()))?;
            Identity::from_bytes(&bytes)?
        }
        _ => bail!(USAGE),
    };
    if let Some(destination) = export {
        let path = Path::new(&destination);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("create public certificate {}", path.display()))?;
        file.write_all(identity.certificate())?;
    } else {
        println!("{}", identity.fingerprint());
    }
    Ok(())
}

pub(super) fn verify_command(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    if args.next().as_deref().and_then(|value| value.to_str()) != Some("verify") {
        bail!(USAGE);
    }
    let path = args.next().context(USAGE)?;
    let pin = args.next().context(USAGE)?;
    if args.next().is_some() {
        bail!(USAGE);
    }
    let pin: Fingerprint = pin.to_str().context("fingerprint must be ASCII")?.parse()?;
    let path = Path::new(&path);
    let file =
        File::open(path).with_context(|| format!("open peer certificate {}", path.display()))?;
    TrustedPeer::read(file, pin)?;
    println!("Verified peer certificate {pin}");
    Ok(())
}
