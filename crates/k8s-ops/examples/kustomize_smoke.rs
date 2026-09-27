//! Headless check of the standalone kustomize build, no cluster required.
//!
//!   cargo run -p k8s-ops --example kustomize_smoke -- <overlay-path>
//!
//! Resolves kustomize (bundled sidecar first, then PATH) and prints the built
//! YAML. Cross-check against `kustomize build <overlay-path>` run directly.

use k8s_ops::kustomize::Kustomize;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: kustomize_smoke <overlay-path>")?;

    let kustomize = Kustomize::resolve(None)?;
    let yaml = kustomize.build(std::path::Path::new(&path)).await?;
    println!("{yaml}");
    Ok(())
}
