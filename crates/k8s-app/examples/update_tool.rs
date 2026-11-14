//! Release-side tool for signed updates.
//!
//!   cargo run -p k8s-app --example update_tool -- keygen
//!       Prints a new key pair. Put the public key in `PUBLIC_KEY_HEX`
//!       (crates/k8s-app/src/updates.rs) and the private key in the repository
//!       secret `UPDATE_SIGNING_KEY`. Keep the private key nowhere else.
//!
//!   UPDATE_SIGNING_KEY=<hex> cargo run -p k8s-app --example update_tool -- sign <zip>...
//!       Writes `<zip>.sig` beside each zip.
//!
//!   cargo run -p k8s-app --example update_tool -- verify <zip>...
//!       Checks each zip against its `<zip>.sig` and the public key compiled into
//!       the app, as an installed copy would before accepting it.

use std::path::Path;

use k8s_app::updates;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("keygen") => {
            let (private, public) = updates::generate_keypair()?;
            println!("public  (PUBLIC_KEY_HEX): {public}");
            println!("private (UPDATE_SIGNING_KEY secret): {private}");
            Ok(())
        }
        Some("sign") => {
            let key = std::env::var("UPDATE_SIGNING_KEY")
                .map_err(|_| "UPDATE_SIGNING_KEY is not set".to_string())?;
            let zips: Vec<String> = args.collect();
            if zips.is_empty() {
                return Err("name at least one zip to sign".into());
            }
            for zip in zips {
                let signature = updates::sign(&key, Path::new(&zip))?;
                std::fs::write(format!("{zip}.sig"), signature).map_err(|e| e.to_string())?;
                println!("signed {zip}");
            }
            Ok(())
        }
        Some("verify") => {
            let key = updates::compiled_public_key()
                .ok_or_else(|| "no public key is compiled into this build".to_string())?;
            let zips: Vec<String> = args.collect();
            if zips.is_empty() {
                return Err("name at least one zip to verify".into());
            }
            for zip in zips {
                let signature = std::fs::read_to_string(format!("{zip}.sig"))
                    .map_err(|e| format!("{zip}.sig: {e}"))?;
                updates::verify(&key, Path::new(&zip), &signature)?;
                println!("ok {zip}");
            }
            Ok(())
        }
        _ => Err("usage: update_tool keygen | sign <zip>... | verify <zip>...".into()),
    }
}
