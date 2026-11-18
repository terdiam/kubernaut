//! Headless check of reading and changing a node's pod limit.
//!
//!   cargo run -p k8s-ops --example node_pods_smoke -- <context> <node> [<limit>]
//!
//! With only a node this reads the limit. With a limit it RESTARTS the kubelet
//! on that node, so point it only at a cluster where that is acceptable.

use k8s_core::{ClusterManager, ConnectOptions};
use k8s_ops::node_pods;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: node_pods_smoke <context> <node> [<limit>]";
    let context = args.next().ok_or(usage)?;
    let node = args.next().ok_or(usage)?;
    let limit: Option<u32> = args.next().map(|text| text.parse()).transpose()?;

    k8s_core::paths::hydrate_process_path(&[]).await;
    let manager = ClusterManager::from_env()?;
    let cluster = manager.connect(&context, ConnectOptions::default()).await?;

    println!("before: {:?}", node_pods::read(&cluster, &node).await?);
    if let Some(limit) = limit {
        match node_pods::set(&cluster, &node, limit, "default", &node).await {
            Ok(()) => println!("applied {limit}"),
            Err(err) => println!("refused: {err}"),
        }
        println!("after: {:?}", node_pods::read(&cluster, &node).await?);
    }
    Ok(())
}
