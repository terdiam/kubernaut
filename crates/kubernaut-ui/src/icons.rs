//! Icon sources: the component library's bundled set plus the few extra
//! Lucide icons this app uses. Only these are embedded, so the binary does not
//! carry the whole catalogue.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString, assets};

assets::icon_assets!(
    ExtraIcons,
    [
        Box,
        GitBranch,
        HardDrive,
        KeyRound,
        Network,
        Puzzle,
        Server,
        ShieldCheck,
        ShipWheel,
        SlidersHorizontal,
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
