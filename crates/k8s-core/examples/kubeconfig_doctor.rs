//! Why a context the app imported does not authenticate.
//!
//!   cargo run -p k8s-core --example kubeconfig_doctor -- [dir] [--fix]
//!
//! `--fix` qualifies cluster and user names in each file the way import now
//! does, so files written before that change stop colliding. Each file is
//! copied to `<name>.bak` first.
//!
//! Reports structure only — which credential fields a context carries, whether
//! file-based ones resolve, and whether names collide between imported files.
//! No key material, token, or certificate is read or printed.

use std::{collections::BTreeMap, path::PathBuf};

use kube::config::Kubeconfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fix = args.iter().any(|a| a == "--fix");
    let dir = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directories::ProjectDirs::from("dev", "kubernaut", "Kubernaut")
                .map(|d| d.config_dir().join("clusters"))
                .expect("no config directory")
        });
    println!("managed directory: {}\n", dir.display());

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        // Match what the app loads, so the report cannot blame a file the app
        // never reads — a `.bak` left by --fix, for instance.
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|extension| extension == "yaml" || extension == "yml")
        })
        .collect();
    files.sort();

    // Which file first defined each cluster/user name. `Kubeconfig::merge` is
    // first-wins, so a later file's entry of the same name is discarded — and a
    // context then silently binds to another cluster's credentials.
    let mut cluster_owner: BTreeMap<String, String> = BTreeMap::new();
    let mut user_owner: BTreeMap<String, String> = BTreeMap::new();
    let mut collisions = Vec::new();

    for path in &files {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let config = match Kubeconfig::read_from(path) {
            Ok(config) => config,
            Err(err) => {
                println!("{name}: UNREADABLE — {err}\n");
                continue;
            }
        };

        println!("── {name}");
        for context in &config.contexts {
            let Some(ctx) = &context.context else {
                continue;
            };
            let user = ctx.user.clone().unwrap_or_default();
            println!(
                "   context {}  →  cluster {}  user {}",
                context.name, ctx.cluster, user
            );

            let auth = config
                .auth_infos
                .iter()
                .find(|a| a.name == user)
                .and_then(|a| a.auth_info.as_ref());
            match auth {
                None => println!("      ✕ no `users` entry named `{user}` in this file"),
                Some(auth) => {
                    let mut fields = Vec::new();
                    if auth.exec.is_some() {
                        fields.push("exec".to_string());
                    }
                    if auth.token.is_some() {
                        fields.push("token".into());
                    }
                    if auth.client_certificate_data.is_some() {
                        fields.push("client-certificate-data".into());
                    }
                    if auth.client_key_data.is_some() {
                        fields.push("client-key-data".into());
                    }
                    // File-based credentials are the ones that break when a
                    // kubeconfig is copied somewhere else.
                    for (label, value) in [
                        ("client-certificate", &auth.client_certificate),
                        ("client-key", &auth.client_key),
                        ("token-file", &auth.token_file),
                    ] {
                        if let Some(p) = value {
                            let exists = std::path::Path::new(p).exists();
                            fields.push(format!(
                                "{label}=<file {}>",
                                if exists { "present" } else { "MISSING" }
                            ));
                        }
                    }
                    if fields.is_empty() {
                        println!("      ✕ the `users` entry carries no credential at all");
                    } else {
                        println!("      credential: {}", fields.join(", "));
                    }
                }
            }

            let cluster = config.clusters.iter().find(|c| c.name == ctx.cluster);
            match cluster.and_then(|c| c.cluster.as_ref()) {
                None => println!("      ✕ no `clusters` entry named `{}`", ctx.cluster),
                Some(c) => {
                    let ca = c.certificate_authority_data.is_some();
                    let ca_file = c.certificate_authority.as_ref().map(|p| {
                        format!(
                            "<file {}>",
                            if std::path::Path::new(p).exists() {
                                "present"
                            } else {
                                "MISSING"
                            }
                        )
                    });
                    println!(
                        "      cluster: ca-data={ca}{} insecure={:?}",
                        ca_file.map(|f| format!(" ca-file={f}")).unwrap_or_default(),
                        c.insecure_skip_tls_verify
                    );
                }
            }
        }

        for c in &config.clusters {
            if let Some(first) = cluster_owner.get(&c.name) {
                collisions.push(format!(
                    "cluster `{}` defined in {first} and {name}",
                    c.name
                ));
            } else {
                cluster_owner.insert(c.name.clone(), name.clone());
            }
        }
        for a in &config.auth_infos {
            if let Some(first) = user_owner.get(&a.name) {
                collisions.push(format!("user `{}` defined in {first} and {name}", a.name));
            } else {
                user_owner.insert(a.name.clone(), name.clone());
            }
        }
        println!();
    }

    if fix {
        println!("\n── repairing");
        for path in &files {
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let yaml = std::fs::read_to_string(path)?;
            let repaired = k8s_core::kubeconfig::qualify_entries(&yaml, &stem)?;
            if repaired.trim() == yaml.trim() {
                println!("   {stem}: already qualified");
                continue;
            }
            // A kubeconfig holds credentials; keep the original beside it.
            std::fs::copy(path, path.with_extension("yaml.bak"))?;
            std::fs::write(path, repaired)?;
            println!("   {stem}: qualified (original kept as {stem}.yaml.bak)");
        }
        println!("\nre-run without --fix to confirm\n");
        return Ok(());
    }

    if collisions.is_empty() {
        println!("no name collisions between files");
    } else {
        println!("NAME COLLISIONS — merge is first-wins, so the later entry is discarded:");
        for line in &collisions {
            println!("  ✕ {line}");
        }
    }
    Ok(())
}
