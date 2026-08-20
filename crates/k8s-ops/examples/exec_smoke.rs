//! Verify a pod terminal end to end.
//!
//!   cargo run -p k8s-ops --example exec_smoke -- <context> <namespace> <pod>
//!
//! Opens an interactive shell in the pod, runs two harmless read-only commands
//! and exits. Nothing in the cluster is created or modified.

use std::time::Duration;

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_ops::exec::{ExecOptions, TerminalEvent, TerminalManager};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("KUBERNAUT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let context = args
        .next()
        .ok_or("usage: exec_smoke <context> <namespace> <pod>")?;
    let namespace = args.next().ok_or("missing namespace")?;
    let pod = args.next().ok_or("missing pod")?;
    let container = args.next();

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;

    let terminals = TerminalManager::new();
    let session = terminals
        .open(
            &cluster,
            ExecOptions {
                namespace: namespace.clone(),
                pod: pod.clone(),
                container,
                command: Vec::new(),
                columns: 100,
                rows: 30,
            },
        )
        .await?;
    println!(
        "opened {} ({})",
        session.descriptor.title, session.descriptor.kind
    );

    // Interactive shells discard input typed before their line editor is ready.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    session
        .write(b"echo KUBERNAUT_OK; id; hostname; exit\n".to_vec())
        .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut transcript = String::new();
    let mut closed: Option<String> = None;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            batch = session.next_batch() => match batch {
                Some(events) => {
                    let mut done = false;
                    for event in events {
                        match event {
                            TerminalEvent::Output { data } => transcript.push_str(&data),
                            TerminalEvent::Status { message } => println!("  status: {message}"),
                            TerminalEvent::Closed { status } => {
                                closed = Some(status);
                                done = true;
                            }
                            TerminalEvent::Failed { message } => {
                                println!("  failed: {message}");
                                done = true;
                            }
                        }
                    }
                    if done { break; }
                }
                None => break,
            },
        }
    }

    println!("\n--- transcript ---");
    for line in transcript.lines() {
        println!("| {line}");
    }
    println!("--- end ---\n");

    let ok = transcript.contains("KUBERNAUT_OK");
    println!("shell executed the command: {ok}");
    println!(
        "closed with: {}",
        closed.unwrap_or_else(|| "(still open)".into())
    );

    session.stop();
    manager.disconnect(&context);
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
