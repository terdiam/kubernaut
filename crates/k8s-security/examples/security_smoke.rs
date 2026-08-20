//! Read-only security scan against a real cluster.
//!
//!   cargo run -p k8s-security --example security_smoke -- <context> [namespace]
//!
//! Lists workloads and RBAC objects and evaluates them locally. Nothing is
//! created, changed or deleted, and no image is pulled.

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_security::{Severity, scan, vulnerabilities};

fn value_of(arguments: &[String], flag: &str) -> Option<String> {
    arguments
        .iter()
        .position(|argument| argument == flag)
        .and_then(|index| arguments.get(index + 1))
        .cloned()
}

fn bar(counts: &k8s_security::SeverityCounts) -> String {
    format!(
        "critical {} · high {} · medium {} · low {} · info {}",
        counts.critical, counts.high, counts.medium, counts.low, counts.info
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KUBERNAUT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // Flags rather than positional arguments: an empty positional namespace
    // silently shifts every later argument, which is exactly what went wrong
    // the first time this was run.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let context = arguments
        .first()
        .filter(|argument| !argument.starts_with("--"))
        .cloned()
        .ok_or("usage: security_smoke <context> [--namespace NS] [--image IMAGE]")?;
    let namespace = value_of(&arguments, "--namespace");

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;

    // ---- posture ----------------------------------------------------------
    let posture = scan::posture_scan(&cluster, namespace.as_deref()).await?;
    println!(
        "posture: {} workloads examined, {} findings\n  {}",
        posture.examined,
        posture.findings.len(),
        bar(&posture.counts)
    );
    for limitation in &posture.limitations {
        println!("  limitation: {limitation}");
    }
    for finding in posture
        .findings
        .iter()
        .filter(|f| f.severity <= Severity::High)
        .take(8)
    {
        println!(
            "  [{:?}] {} {}/{}{}: {}",
            finding.severity,
            finding.kind,
            finding.namespace.as_deref().unwrap_or("-"),
            finding.name,
            finding
                .container
                .as_deref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default(),
            finding.title
        );
    }

    let by_check = {
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for finding in &posture.findings {
            *counts.entry(finding.id.as_str()).or_default() += 1;
        }
        counts
    };
    println!("\n  findings by check:");
    for (id, count) in &by_check {
        println!("    {id:<30} {count}");
    }

    // ---- rbac -------------------------------------------------------------
    let rbac = scan::rbac_scan(&cluster).await?;
    let user_findings: Vec<_> = rbac.findings.iter().filter(|f| !f.builtin).collect();
    println!(
        "\nrbac: {} objects examined, {} findings ({} on built-in roles, hidden by default)\n  {}",
        rbac.examined,
        rbac.findings.len(),
        rbac.builtin_hidden,
        bar(&rbac.counts)
    );
    println!(
        "  findings on objects somebody created: {}",
        user_findings.len()
    );
    for finding in user_findings.iter().take(10) {
        println!(
            "  [{:?}] {} {}: {}",
            finding.severity, finding.kind, finding.name, finding.message
        );
    }

    // ---- images -----------------------------------------------------------
    let images = scan::cluster_images(&cluster, namespace.as_deref()).await?;
    println!("\ndistinct images running: {}", images.len());
    for usage in images.iter().take(5) {
        println!("  {:<58} {} pods", usage.image, usage.pod_count);
    }

    let scanner = vulnerabilities::detect(&cluster, None).await;
    println!("\nvulnerability scanner: {scanner:?}");
    // Optionally scan one image, when a binary is available and the caller
    // names one. Pulling and analysing an image is slow, so it is never
    // automatic.
    let wanted_image = value_of(&arguments, "--image");
    let found = match (&scanner, &wanted_image) {
        (
            vulnerabilities::Scanner::TrivyBinary {
                path,
                database_ready,
                ..
            },
            Some(image),
        ) => {
            if !database_ready {
                println!("\ndownloading the vulnerability database (~110 MiB, once)…");
                let started = std::time::Instant::now();
                match vulnerabilities::download_database(std::path::Path::new(path)).await {
                    Ok(repository) => {
                        println!("  ready in {:?} from {repository}", started.elapsed())
                    }
                    Err(err) => println!("  {err}"),
                }
            }
            println!("\nscanning `{image}`…");
            let started = std::time::Instant::now();
            match vulnerabilities::scan_image(std::path::Path::new(path), image.as_str()).await {
                Ok(found) => {
                    println!(
                        "  {} vulnerabilities in {:?}",
                        found.len(),
                        started.elapsed()
                    );
                    found
                }
                Err(err) => {
                    println!("  {err}");
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };

    if !found.is_empty() {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for vulnerability in &found {
            *counts
                .entry(format!("{:?}", vulnerability.severity))
                .or_default() += 1;
        }
        println!("  by severity: {counts:?}");

        for vulnerability in found
            .iter()
            .filter(|v| matches!(v.severity, Severity::Critical | Severity::High))
            .take(5)
        {
            println!(
                "    [{:?}] {} in {} {} → {}",
                vulnerability.severity,
                vulnerability.id,
                vulnerability.package,
                vulnerability.installed_version,
                vulnerability
                    .fixed_version
                    .as_deref()
                    .unwrap_or("no fix yet")
            );
        }
    }

    // Re-detect: the download changes `database_ready`, and reporting the state
    // captured before it would repeat the "no database" warning after a
    // successful scan.
    let scanner = vulnerabilities::detect(&cluster, None).await;
    let report = vulnerabilities::report(found, &scanner, images.len());
    println!("\n  findings after grouping: {}", report.findings.len());
    for finding in report.findings.iter().take(4) {
        println!(
            "    [{:?}] {}: {}",
            finding.severity, finding.name, finding.message
        );
    }
    for limitation in &report.limitations {
        println!("  {limitation}");
    }

    manager.disconnect(&context);
    println!("\ndone (read-only)");
    Ok(())
}
