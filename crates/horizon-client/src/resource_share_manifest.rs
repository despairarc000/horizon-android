//! Versioned, verified resources for opt-in Android peer-to-peer transfer.
//! Transport, discovery and UI live elsewhere. Never include accounts, saves or profiles.
use crate::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const MAX_FILES: usize = 100_000;
const MAX_TOTAL: u64 = 8 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Entry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub protocol: u32,
    pub version: String,
    pub files: Vec<Entry>,
}

pub fn prepared_root(home: &Path) -> PathBuf {
    home.join(".local/horizon-client")
        .join(horizon_core::VANILLA_VERSION)
}

pub fn incoming_root(home: &Path) -> PathBuf {
    home.join(".local/horizon-client/resource-share.partial")
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains('\0')
        && !path.starts_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub fn verify_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.protocol != 1 || manifest.version != horizon_core::VANILLA_VERSION {
        return Err("Incompatible resource transfer protocol or Minecraft version".into());
    }
    if manifest.files.is_empty() || manifest.files.len() > MAX_FILES {
        return Err("Invalid resource transfer file count".into());
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    for entry in &manifest.files {
        if !safe_path(&entry.path)
            || !names.insert(&entry.path)
            || entry.bytes > MAX_FILE
            || entry.sha256.len() != 64
            || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid resource transfer file entry".into());
        }
        total = total.checked_add(entry.bytes).ok_or("Resource transfer size overflow")?;
        if total > MAX_TOTAL {
            return Err("Resource transfer exceeds 8 GiB".into());
        }
    }
    for needed in ["horizon-resources.json", "android-downloads-v2.json"] {
        if !names.contains(needed) {
            return Err(format!("Missing required resource marker: {needed}").into());
        }
    }
    Ok(())
}

fn checksum(file: impl Read) -> Result<String> {
    let mut file = file;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn scan(root: &Path, at: &Path, entries: &mut Vec<Entry>) -> Result<()> {
    for item in fs::read_dir(at)? {
        let path = item?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err("Refusing to share resource symlink".into());
        }
        if meta.is_dir() {
            scan(root, &path, entries)?;
        } else if meta.is_file() {
            if entries.len() >= MAX_FILES {
                return Err("Too many resource files".into());
            }
            let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            if !safe_path(&relative) || meta.len() > MAX_FILE {
                return Err("Invalid resource file".into());
            }
            entries.push(Entry {
                path: relative,
                bytes: meta.len(),
                sha256: checksum(fs::File::open(path)?)?,
            });
        } else {
            return Err("Unsupported resource file type".into());
        }
    }
    Ok(())
}

/// Sender: enumerate only the prepared versioned resources, never profile or account files.
pub fn inventory(home: &Path) -> Result<Manifest> {
    if !crate::official_install::ready(home) {
        return Err("Only fully prepared Minecraft resources can be shared".into());
    }
    let root = prepared_root(home);
    if fs::symlink_metadata(&root)?.file_type().is_symlink() {
        return Err("Refusing redirected resource directory".into());
    }
    let mut files = Vec::new();
    scan(&root, &root, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = Manifest {
        protocol: 1,
        version: horizon_core::VANILLA_VERSION.into(),
        files,
    };
    verify_manifest(&manifest)?;
    Ok(manifest)
}

/// Reject symlinks in any existing component of a received resource path.
fn existing_file(root: &Path, relative: &str) -> Result<Option<PathBuf>> {
    if !safe_path(relative) {
        return Err("Unsafe resource transfer path".into());
    }
    let mut path = root.to_path_buf();
    for component in relative.split('/') {
        path.push(component);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if meta.file_type().is_symlink() {
            return Err("Refusing resource symlink".into());
        }
    }
    if !fs::metadata(&path)?.is_file() {
        return Err("Resource path is not a regular file".into());
    }
    Ok(Some(path))
}

fn matches_file(root: &Path, entry: &Entry) -> Result<bool> {
    let Some(path) = existing_file(root, &entry.path)? else {
        return Ok(false);
    };
    let file = fs::File::open(path)?;
    Ok(file.metadata()?.len() == entry.bytes
        && checksum(file)?.eq_ignore_ascii_case(&entry.sha256))
}

/// Receiver: count only files missing or corrupt in the resumable staging directory.
pub fn missing(home: &Path, manifest: &Manifest) -> Result<Vec<Entry>> {
    verify_manifest(manifest)?;
    manifest
        .files
        .iter()
        .filter_map(|entry| match matches_file(&incoming_root(home), entry) {
            Ok(true) => None,
            Ok(false) => Some(Ok(entry.clone())),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

/// Receiver: stream one approved entry to a .partial file and verify before accepting it.
pub fn receive_file(
    home: &Path,
    manifest: &Manifest,
    name: &str,
    source: impl Read,
) -> Result<()> {
    verify_manifest(manifest)?;
    let entry = manifest
        .files
        .iter()
        .find(|entry| entry.path == name)
        .ok_or("Unlisted resource transfer file")?;
    let root = incoming_root(home);
    let target = root.join(&entry.path);
    let parent = target.parent().ok_or("No destination parent")?;
    fs::create_dir_all(parent)?;
    // Checking again prevents following a symlink left by a malicious local provider.
    let _ = existing_file(&root, name)?;
    let temp = target.with_extension("share-tmp");
    if temp.exists() || fs::symlink_metadata(&temp).is_ok() {
        fs::remove_file(&temp)?;
    }
    let result = (|| -> Result<()> {
        let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        let count = std::io::copy(&mut source.take(entry.bytes + 1), &mut output)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);
        if count != entry.bytes || !checksum(fs::File::open(&temp)?)?.eq_ignore_ascii_case(&entry.sha256) {
            return Err("Received resource does not match its size and SHA-256".into());
        }
        fs::rename(&temp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// Only activate when every file is verified. Never replace a working installation.
pub fn finalize(home: &Path, manifest: &Manifest) -> Result<()> {
    verify_manifest(manifest)?;
    let stage = incoming_root(home);
    for entry in &manifest.files {
        if !matches_file(&stage, entry)? {
            return Err(format!("Resource transfer incomplete: {}", entry.path).into());
        }
    }
    let target = prepared_root(home);
    if target.exists() || fs::symlink_metadata(&target).is_ok() {
        return Err("Refusing to replace an existing resource installation".into());
    }
    fs::rename(stage, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-resource-share-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn copies_only_missing_verified_resources() {
        let sender = home();
        let receiver = home();
        let src = prepared_root(&sender);
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join("horizon-resources.json"), b"{}").unwrap();
        fs::write(src.join("android-downloads-v2.json"), b"{\"schema\":2}").unwrap();
        fs::write(src.join("assets/a.txt"), b"resource").unwrap();
        let manifest = inventory(&sender).unwrap();
        assert_eq!(missing(&receiver, &manifest).unwrap().len(), 3);
        for entry in &manifest.files {
            let bytes = fs::read(src.join(&entry.path)).unwrap();
            receive_file(&receiver, &manifest, &entry.path, bytes.as_slice()).unwrap();
        }
        assert!(missing(&receiver, &manifest).unwrap().is_empty());
        finalize(&receiver, &manifest).unwrap();
        assert!(crate::official_install::ready(&receiver));
        assert_eq!(fs::read(prepared_root(&receiver).join("assets/a.txt")).unwrap(), b"resource");
        fs::remove_dir_all(sender).unwrap();
        fs::remove_dir_all(receiver).unwrap();
    }

    #[test]
    fn rejects_corrupt_and_unsafe_entries_without_installing() {
        let receiver = home();
        let manifest = Manifest {
            protocol: 1,
            version: horizon_core::VANILLA_VERSION.into(),
            files: vec![
                Entry { path: "horizon-resources.json".into(), bytes: 2, sha256: format!("{:x}", Sha256::digest(b"{}")) },
                Entry { path: "android-downloads-v2.json".into(), bytes: 2, sha256: format!("{:x}", Sha256::digest(b"{}")) },
                Entry { path: "assets/data".into(), bytes: 5, sha256: format!("{:x}", Sha256::digest(b"valid")) },
            ],
        };
        assert!(receive_file(&receiver, &manifest, "assets/data", b"wrong".as_slice()).is_err());
        assert!(!prepared_root(&receiver).exists());
        let mut unsafe_manifest = manifest;
        unsafe_manifest.files[2].path = "../accounts.json".into();
        assert!(verify_manifest(&unsafe_manifest).is_err());
        if receiver.exists() { fs::remove_dir_all(receiver).unwrap(); }
    }
}
