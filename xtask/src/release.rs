//! The release manifest (#179, DR 0018): written from the installers a
//! release ships, then signed with the key in `UPDATE_SIGNING_KEY`.

use std::path::Path;

use blueprint_update::{Asset, Manifest, RELEASES};
use sha2::{Digest, Sha256};

/// The platform an installer is for, from its file name as cargo-packager
/// and cargo-generate-rpm name it; `None` for anything else.
#[must_use]
pub fn platform_of(name: &str) -> Option<&'static str> {
    if name.ends_with("_amd64.deb") {
        Some("linux-x86_64-deb")
    } else if name.ends_with(".x86_64.rpm") {
        Some("linux-x86_64-rpm")
    } else if name.ends_with("_x64-setup.exe") {
        Some("windows-x86_64")
    } else if name.ends_with("_aarch64.dmg") {
        Some("macos-aarch64")
    } else {
        None
    }
}

/// The manifest of release `version` (tag `v<version>`) from its
/// installers: one per platform, each named for this version, hashed and
/// sized, with its download URL on the release. Checked as the app would
/// check it.
///
/// # Errors
/// A file that is no installer, two for one platform, one named for another
/// version, an unreadable file, or a manifest the app would refuse.
pub fn write_manifest(
    version: &str,
    notes: &str,
    files: &[impl AsRef<Path>],
) -> Result<Manifest, String> {
    let mut m = Manifest {
        version: version.to_owned(),
        notes: notes.to_owned(),
        assets: Default::default(),
    };
    for f in files {
        let f = f.as_ref();
        let name = f
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{}: no file name", f.display()))?;
        let platform = platform_of(name).ok_or_else(|| format!("{name}: not an installer"))?;
        if !name.contains(&format!("_{version}_")) && !name.contains(&format!("-{version}-")) {
            return Err(format!("{name}: not named for version {version}"));
        }
        let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let asset = Asset {
            url: format!("{RELEASES}v{version}/{name}"),
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: bytes.len() as u64,
        };
        if m.assets.insert(platform.to_owned(), asset).is_some() {
            return Err(format!("two installers for {platform}"));
        }
    }
    m.validate().map_err(|e| e.to_string())?;
    Ok(m)
}
