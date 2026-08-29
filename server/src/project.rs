//! Opening a *run* rather than a file.
//!
//! A `clearmap-ng` workspace is a directory of assets whose names are the
//! pipeline's own vocabulary — `binary/ch0/filled.npy`, `skeleton/ch0/…` — plus
//! a `manifest.json` that indexes them. Asking a user to type each of those
//! into the layer box is asking them to do a `find` by hand, so this walks the
//! directory and builds the session.
//!
//! Two forms are accepted, and both are just lists of layers in the end:
//!
//! * a **directory** — scanned by extension and by the workspace's own layout;
//! * a **project file** (`.json`) — an explicit list, which is also what the
//!   viewer writes when a view is worth keeping.
//!
//! What the scan does *not* do is guess at intent beyond the layer kind: it
//! does not decide contrast, colour or ordering beyond "images under masks
//! under objects", because those are the things a person adjusts and a guess
//! would only be in the way.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::objects::ObjectSpace;
use crate::session::{LayerRole, Session};
use crate::source::{SourceRegistry, SourceSpec};

/// One layer, as a project file names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectLayer {
    /// A source URI or a path relative to the project file.
    pub source: String,
    /// `image`, `labels`, `objects`, or absent for auto-detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Object layers only: world pixels per source unit, `z,y,x`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<String>,
}

/// A saved view: the layers, in draw order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub name: Option<String>,
    pub layers: Vec<ProjectLayer>,
}

impl Project {
    /// Read a project file, resolving relative sources against its directory.
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut project: Project = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        for layer in &mut project.layers {
            layer.source = resolve(base, &layer.source);
        }
        Ok(project)
    }

    /// Open every layer into a session, in order.
    ///
    /// A layer that fails to open is **reported and skipped**, not fatal: a run
    /// directory with one truncated output should still show the rest, and the
    /// alternative is a viewer that refuses to open anything.
    pub async fn open(&self, registry: &SourceRegistry, session: &mut Session) -> Result<usize> {
        let mut opened = 0;
        for layer in &self.layers {
            let spec = match SourceSpec::parse(&layer.source) {
                Ok(spec) => spec,
                Err(e) => {
                    log::warn!("skipping `{}`: {e:#}", layer.source);
                    continue;
                }
            };
            let space = ObjectSpace::parse(layer.scale.as_deref(), layer.offset.as_deref())
                .unwrap_or_default();
            let role = LayerRole::parse(layer.role.as_deref());
            match session
                .add(registry, spec, role, layer.name.clone(), space)
                .await
            {
                Ok(id) => {
                    opened += 1;
                    log::info!("opened {id} from {}", layer.source);
                }
                Err(e) => log::warn!("skipping `{}`: {e:#}", layer.source),
            }
        }
        Ok(opened)
    }

    /// Build a project from a directory: a run, a workspace, or a folder of
    /// outputs.
    pub fn scan(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            bail!("{} is not a directory", root.display());
        }
        let mut images = Vec::new();
        let mut volumes = Vec::new();
        let mut objects = Vec::new();
        walk(root, 0, &mut images, &mut volumes, &mut objects)?;

        // Images first so they are the reference layer and sit at the bottom,
        // then masks over them, then the objects on top.
        images.sort();
        volumes.sort();
        objects.sort();
        let layers = images
            .into_iter()
            .map(|path| layer(root, path, None))
            .chain(volumes.into_iter().map(|path| layer(root, path, None)))
            .chain(
                objects
                    .into_iter()
                    .map(|path| layer(root, path, Some("objects"))),
            )
            .collect();

        Ok(Project {
            name: root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            layers,
        })
    }
}

/// One project layer for a discovered file, named by its path inside the run.
fn layer(root: &Path, path: PathBuf, role: Option<&str>) -> ProjectLayer {
    let name = path
        .strip_prefix(root)
        .unwrap_or(&path)
        .to_string_lossy()
        .into_owned();
    ProjectLayer {
        source: path.to_string_lossy().into_owned(),
        role: role.map(str::to_string),
        name: Some(name),
        scale: None,
        offset: None,
    }
}

/// How deep a scan goes.
///
/// `clearmap-ng`'s layout is `<root>/<family>/<channel>/<asset>.npy` — three
/// levels — and going deeper turns a scan of a run into a scan of whatever the
/// run happens to sit next to.
const MAX_DEPTH: usize = 3;

fn walk(
    directory: &Path,
    depth: usize,
    images: &mut Vec<PathBuf>,
    volumes: &mut Vec<PathBuf>,
    objects: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            // A `.zarr` directory is a store, not a directory to walk into.
            if name.ends_with(".zarr") || path.join("zarr.json").exists() || path.join(".zattrs").exists() {
                images.push(path);
            } else if depth < MAX_DEPTH {
                walk(&path, depth + 1, images, volumes, objects)?;
            }
            continue;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some("npy") => {
                // A points file and a volume are both `.npy`; the name is the
                // only signal before opening, and `cells` is the pipeline's own
                // word for the former.
                if name.contains("cell") || name.contains("point") || name.contains("spot") {
                    objects.push(path);
                } else {
                    volumes.push(path);
                }
            }
            Some("csv") | Some("tsv") | Some("blob") => objects.push(path),
            _ => {}
        }
    }
    Ok(())
}

/// Resolve a project file's source against the file's own directory.
fn resolve(base: &Path, source: &str) -> String {
    if source.contains("://") {
        return source.to_string();
    }
    let path = Path::new(source);
    if path.is_absolute() {
        return source.to_string();
    }
    base.join(path).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        // The layout `clearmap-ng`'s `Workspace` writes.
        for asset in ["binary/ch0/filled.npy", "binary/ch0/final.npy", "skeleton/ch0/skeleton.npy"] {
            let path = root.join(asset);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        std::fs::write(root.join("manifest.json"), b"{}").unwrap();
        std::fs::write(root.join("cells.csv"), b"").unwrap();
        std::fs::write(root.join("cells.npy"), b"").unwrap();
        std::fs::create_dir_all(root.join("raw.zarr")).unwrap();
        std::fs::write(root.join("raw.zarr/zarr.json"), b"{}").unwrap();
        // Something the scan should ignore.
        std::fs::write(root.join("notes.txt"), b"").unwrap();
        dir
    }

    #[test]
    fn a_scan_finds_a_workspaces_assets_and_orders_them() {
        let dir = workspace();
        let project = Project::scan(dir.path()).expect("scan");
        let names: Vec<&str> = project
            .layers
            .iter()
            .map(|l| l.name.as_deref().unwrap_or_default())
            .collect();

        assert_eq!(names[0], "raw.zarr", "the image is the bottom layer");
        assert!(
            names.contains(&"binary/ch0/filled.npy"),
            "masks are volumes: {names:?}"
        );
        assert_eq!(
            names.last().copied(),
            Some("cells.npy"),
            "objects are on top: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.ends_with(".txt")),
            "a scan reads volumes and tables, not notes"
        );
    }

    #[test]
    fn a_cell_npy_is_an_object_layer_and_a_mask_npy_is_not() {
        let dir = workspace();
        let project = Project::scan(dir.path()).expect("scan");
        let cells = project
            .layers
            .iter()
            .find(|l| l.name.as_deref() == Some("cells.npy"))
            .expect("cells.npy");
        assert_eq!(cells.role.as_deref(), Some("objects"));
        let mask = project
            .layers
            .iter()
            .find(|l| l.name.as_deref() == Some("binary/ch0/filled.npy"))
            .expect("filled.npy");
        assert_eq!(mask.role, None, "a mask is auto-detected as a volume");
    }

    #[test]
    fn a_project_file_round_trips_and_resolves_relative_sources() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("view.json");
        let project = Project {
            name: Some("a view".into()),
            layers: vec![
                ProjectLayer {
                    source: "image.zarr".into(),
                    role: None,
                    name: Some("image".into()),
                    scale: None,
                    offset: None,
                },
                ProjectLayer {
                    source: "s3://bucket/cells.csv".into(),
                    role: Some("objects".into()),
                    name: None,
                    scale: Some("1,2,2".into()),
                    offset: None,
                },
            ],
        };
        std::fs::write(&path, serde_json::to_string_pretty(&project).unwrap()).unwrap();

        let read = Project::read(&path).expect("read");
        assert_eq!(read.layers.len(), 2);
        assert_eq!(
            read.layers[0].source,
            dir.path().join("image.zarr").to_string_lossy(),
            "a relative source resolves against the project file"
        );
        assert_eq!(
            read.layers[1].source, "s3://bucket/cells.csv",
            "a URI is left alone"
        );
        assert_eq!(read.layers[1].scale.as_deref(), Some("1,2,2"));
    }

    #[test]
    fn scanning_something_that_is_not_a_directory_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("x.npy");
        std::fs::write(&file, b"").unwrap();
        assert!(Project::scan(&file).is_err());
    }
}
