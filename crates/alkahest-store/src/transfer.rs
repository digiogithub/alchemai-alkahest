//! Moving models between two Alkahest stores (ALK-US-0136): `alkahest import --store <dir>`.
//!
//! The typical case is a machine where models were pulled by a user's own server
//! (`~/.local/share/alkahest`) before the system service (`/var/lib/alkahest`) was installed:
//! the service starts with an empty store, and the user's models look gone.
//!
//! A transfer keeps the manifest bytes as they are, so a model keeps its digest and its
//! provenance (registry, Hugging Face revision, import record). Blobs missing from the
//! destination are streamed through a verified [`crate::BlobWriter`]; blobs it already has are
//! shared. The source is opened with [`AlkahestStore::open_existing`], which creates and
//! cleans nothing, and is only ever read.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::digest::Digest;
use crate::error::{Result, StoreError};
use crate::fsutil;
use crate::local::{AlkahestStore, StoredManifest};
use crate::overlay::OVERLAY_DIR;
use crate::reference::ModelName;

/// What [`AlkahestStore::transfer_model`] did with one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferOutcome {
    /// The model was copied.
    Imported {
        /// Blobs copied from the source.
        blobs_copied: usize,
        /// Bytes copied.
        bytes_copied: u64,
        /// Blobs the destination already had.
        blobs_shared: usize,
    },
    /// The destination already holds this exact manifest under this name.
    AlreadyPresent,
    /// The destination holds another model under this name; it is left alone.
    NameTaken {
        /// Digest of the destination's manifest.
        existing: Digest,
    },
}

/// A source store found from a folder given on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreLocation {
    /// The models folder (`manifests/`, `blobs/`).
    pub models_dir: PathBuf,
    /// The per-model settings folder (`model-config/`) next to it, when there is one.
    pub overlay_dir: Option<PathBuf>,
    /// The data folder that holds the models folder (and `engines/`), when it has a parent.
    pub data_dir: Option<PathBuf>,
}

impl StoreLocation {
    /// Accepts either an Alkahest data folder (`<dir>/models/manifests`) or a models folder
    /// (`<dir>/manifests`). `None` when `dir` is neither.
    pub fn find(dir: &Path) -> Option<Self> {
        let (models_dir, data_dir) = if dir.join("models").join("manifests").is_dir() {
            (dir.join("models"), Some(dir.to_path_buf()))
        } else if dir.join("manifests").is_dir() {
            (dir.to_path_buf(), dir.parent().map(Path::to_path_buf))
        } else {
            return None;
        };
        let overlay_dir = data_dir
            .as_ref()
            .map(|d| d.join(OVERLAY_DIR))
            .filter(|d| d.is_dir());
        Some(Self {
            models_dir,
            overlay_dir,
            data_dir,
        })
    }
}

impl AlkahestStore {
    /// Opens an existing store without creating directories or sweeping `tmp/`: the handle
    /// for a store that must not change, such as the source of a transfer.
    pub fn open_existing(root: impl Into<PathBuf>) -> Result<Self> {
        let store = Self::at(root.into());
        if !store.manifests_dir().is_dir() {
            return Err(StoreError::NoStore(store.root().to_path_buf()));
        }
        Ok(store)
    }

    /// Copies the model `sm` of the store `src` into this store under the same name, with
    /// the same manifest bytes. A name already used here by another manifest is never
    /// overwritten. Holds the shared lock, so it never races with GC.
    pub fn transfer_model(
        &self,
        src: &AlkahestStore,
        sm: &StoredManifest,
    ) -> Result<TransferOutcome> {
        let bytes = fsutil::read_bounded(&sm.path, fsutil::MAX_DOCUMENT)?;
        let digest = Digest::of(&bytes);
        if let Some(existing) = self.read_manifest(&sm.name)? {
            return Ok(if existing.digest == digest {
                TransferOutcome::AlreadyPresent
            } else {
                TransferOutcome::NameTaken {
                    existing: existing.digest,
                }
            });
        }
        let _lock = self.lock_shared()?;
        let (mut blobs_copied, mut bytes_copied, mut blobs_shared) = (0, 0, 0);
        let mut seen = BTreeSet::new();
        for d in sm.manifest.blobs() {
            if !seen.insert(d.digest) {
                continue;
            }
            if self.blob_size(&d.digest)? == Some(d.size) {
                blobs_shared += 1;
                continue;
            }
            bytes_copied += self.copy_blob_from(&src.blob_path(&d.digest), d)?;
            blobs_copied += 1;
        }
        self.write_manifest_bytes(&sm.name, &bytes)?;
        Ok(TransferOutcome::Imported {
            blobs_copied,
            bytes_copied,
            blobs_shared,
        })
    }
}

/// Copies the per-model settings files of `name` (`<tag>.toml`, `<tag>.recommended.json`)
/// from the overlay folder `from` to `to` when the destination has none of its own. Returns
/// the files written.
pub fn transfer_overlays(from: &Path, to: &Path, name: &ModelName) -> Result<Vec<PathBuf>> {
    let src = crate::overlay::ModelOverlays::new(from);
    let dst = crate::overlay::ModelOverlays::new(to);
    let mut written = Vec::new();
    for (s, d) in [
        (src.model_path(name), dst.model_path(name)),
        (src.recommended_path(name), dst.recommended_path(name)),
    ] {
        if !s.is_file() || d.exists() {
            continue;
        }
        let bytes = fsutil::read_bounded(&s, fsutil::MAX_DOCUMENT)?;
        let dir = d.parent().ok_or_else(|| StoreError::NoStore(d.clone()))?;
        fs::create_dir_all(dir).map_err(|e| StoreError::io("create", dir, e))?;
        fs::write(&d, bytes).map_err(|e| StoreError::io("write", &d, e))?;
        written.push(d);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::manifest::{Descriptor, Manifest, media_type};
    use crate::reference::ModelRef;

    fn name(s: &str) -> ModelName {
        s.parse::<ModelRef>()
            .unwrap()
            .store_name()
            .unwrap()
            .unwrap()
    }

    fn put(store: &AlkahestStore, n: &str, weights: &[u8]) -> Digest {
        let config = store
            .put_json_blob(
                media_type::CONFIG,
                &serde_json::json!({"model_format":"gguf"}),
            )
            .unwrap();
        let layer = Descriptor::new(
            media_type::MODEL,
            store.put_blob(weights).unwrap(),
            weights.len() as u64,
        );
        store
            .write_manifest(&name(n), &Manifest::new(config, vec![layer]))
            .unwrap()
    }

    fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            for e in fs::read_dir(&p).unwrap() {
                let path = e.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn models_move_with_their_digest_and_the_source_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let user = AlkahestStore::open(tmp.path().join("user/models")).unwrap();
        let a = put(
            &user,
            "hf.co/prism-ml/Ternary-Bonsai-2-4B-gguf:PTQ1_0",
            b"bonsai",
        );
        put(&user, "shared:one", b"same weights");
        let before = tree(user.root());

        let service = AlkahestStore::open(tmp.path().join("service/models")).unwrap();
        put(&service, "shared:two", b"same weights");
        let src = AlkahestStore::open_existing(user.root()).unwrap();
        let mut outcomes = Vec::new();
        for sm in src.list_manifests().unwrap() {
            outcomes.push((
                sm.name.to_string(),
                service.transfer_model(&src, &sm).unwrap(),
            ));
        }

        assert_eq!(tree(user.root()), before, "the source store changed");
        let moved = service
            .read_manifest(&name("hf.co/prism-ml/Ternary-Bonsai-2-4B-gguf:PTQ1_0"))
            .unwrap()
            .unwrap();
        assert_eq!(moved.digest, a);
        for (n, o) in &outcomes {
            match o {
                TransferOutcome::Imported { blobs_shared, .. } if n.contains("shared") => {
                    // The weights and the config are already there.
                    assert_eq!(*blobs_shared, 2, "{n}");
                }
                // Only the weights: the config blob is the same JSON in both stores.
                TransferOutcome::Imported {
                    blobs_copied,
                    blobs_shared,
                    ..
                } => assert_eq!((*blobs_copied, *blobs_shared), (1, 1)),
                other => panic!("{n}: {other:?}"),
            }
        }

        // A second run changes nothing.
        for sm in src.list_manifests().unwrap() {
            assert_eq!(
                service.transfer_model(&src, &sm).unwrap(),
                TransferOutcome::AlreadyPresent
            );
        }
    }

    #[test]
    fn a_different_model_under_the_same_name_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let user = AlkahestStore::open(tmp.path().join("u")).unwrap();
        let service = AlkahestStore::open(tmp.path().join("s")).unwrap();
        put(&user, "m:1", b"user's");
        let theirs = put(&service, "m:1", b"service's");
        let sm = user.list_manifests().unwrap().remove(0);
        assert_eq!(
            service.transfer_model(&user, &sm).unwrap(),
            TransferOutcome::NameTaken { existing: theirs }
        );
        assert_eq!(
            service.read_manifest(&name("m:1")).unwrap().unwrap().digest,
            theirs
        );
    }

    #[test]
    fn locations_and_overlays() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let _ = AlkahestStore::open(data.join("models")).unwrap();
        assert!(StoreLocation::find(&data).unwrap().overlay_dir.is_none());
        let n = name("hf.co/prism-ml/Ternary-Bonsai-2-9B-gguf:PQ2_0");
        let from = data.join(OVERLAY_DIR);
        let src = crate::overlay::ModelOverlays::new(&from);
        fs::create_dir_all(src.model_path(&n).parent().unwrap()).unwrap();
        fs::write(src.model_path(&n), "context_length = 8192\n").unwrap();
        fs::write(src.recommended_path(&n), "{}").unwrap();

        let by_data = StoreLocation::find(&data).unwrap();
        let by_models = StoreLocation::find(&data.join("models")).unwrap();
        assert_eq!(by_data, by_models);
        assert_eq!(by_data.overlay_dir.as_deref(), Some(from.as_path()));
        assert!(StoreLocation::find(tmp.path()).is_none());
        assert!(AlkahestStore::open_existing(tmp.path().join("nope")).is_err());

        let to = tmp.path().join("service/model-config");
        let dst = crate::overlay::ModelOverlays::new(&to);
        fs::create_dir_all(dst.model_path(&n).parent().unwrap()).unwrap();
        fs::write(dst.model_path(&n), "context_length = 4096\n").unwrap();
        let written = transfer_overlays(&from, &to, &n).unwrap();
        // The service's own settings win; the missing recommended file is added.
        assert_eq!(written, vec![dst.recommended_path(&n)]);
        assert_eq!(
            fs::read_to_string(dst.model_path(&n)).unwrap(),
            "context_length = 4096\n"
        );
    }
}
