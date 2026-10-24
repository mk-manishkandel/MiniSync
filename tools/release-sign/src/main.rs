//! Release signing for minisync's self-updater.
//!
//!   release-sign keygen <secret-key-file>
//!       Creates a new key pair. Keep the secret file private (store it as the
//!       RELEASE_SIGNING_KEY GitHub secret). Put the printed public key in
//!       release-public-key.txt.
//!
//!   release-sign sign <file>...
//!       Writes <file>.sig for each file, using the secret key (hex) from the
//!       RELEASE_SIGNING_KEY environment variable.
//!
//!   release-sign verify <public-key-hex> <file>
//!       Checks <file>.sig.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Result<[u8; N]> {
    let s = s.trim();
    if s.len() != N * 2 {
        bail!("expected {} hex characters, got {}", N * 2, s.len());
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let path = args.get(1).context("usage: release-sign keygen <secret-key-file>")?;
            if std::path::Path::new(path).exists() {
                bail!("{path} already exists; refusing to overwrite a signing key");
            }
            let mut seed = [0u8; 32];
            getrandom::getrandom(&mut seed).map_err(|e| anyhow::anyhow!("{e}"))?;
            let key = SigningKey::from_bytes(&seed);
            std::fs::write(path, hex(&seed))?;
            println!("{}", hex(key.verifying_key().as_bytes()));
        }
        Some("sign") => {
            let secret = std::env::var("RELEASE_SIGNING_KEY").context("RELEASE_SIGNING_KEY is not set")?;
            let key = SigningKey::from_bytes(&unhex::<32>(&secret).context("RELEASE_SIGNING_KEY")?);
            if args.len() < 2 {
                bail!("usage: release-sign sign <file>...");
            }
            for file in &args[1..] {
                let data = std::fs::read(file).with_context(|| format!("reading {file}"))?;
                let sig = key.sign(&data);
                std::fs::write(format!("{file}.sig"), hex(&sig.to_bytes()))?;
                println!("signed {file}");
            }
        }
        Some("verify") => {
            let (Some(public), Some(file)) = (args.get(1), args.get(2)) else {
                bail!("usage: release-sign verify <public-key-hex> <file>");
            };
            let key = VerifyingKey::from_bytes(&unhex::<32>(public)?)?;
            let data = std::fs::read(file)?;
            let sig = Signature::from_bytes(&unhex::<64>(&std::fs::read_to_string(format!("{file}.sig"))?)?);
            key.verify_strict(&data, &sig)?;
            println!("OK: {file} is correctly signed");
        }
        _ => bail!("usage: release-sign keygen <file> | sign <file>... | verify <public-key> <file>"),
    }
    Ok(())
}
