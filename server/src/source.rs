//! Where a layer's bytes live, and how to reach them.
//!
//! Disk and object storage are peers here: a source is named by a URI, and the
//! registry turns that URI into a backend. The split that matters is not local
//! vs remote but *sync vs async* — a filesystem store is read on the calling
//! thread and everything else goes through `opendal` — and that split is
//! `zarr_reader`'s to act on, not this module's.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

/// A named place bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSpec {
    /// A local path. `file:///abs/path` or a bare path.
    File(PathBuf),
    /// `http://` or `https://` — a store served as static files.
    Http(String),
    /// `s3://[profile@]bucket/key`. The profile names an entry in
    /// [`SourceRegistry`]; an absent one means `default`.
    S3 {
        profile: String,
        bucket: String,
        key: String,
    },
}

impl SourceSpec {
    /// A source that does not exist yet.
    ///
    /// An annotation layer starts life with nowhere to be read from and nowhere
    /// to be written to — it is a set of boxes a person is about to draw. It
    /// gets a real spec the first time it is saved. Spelling that state as an
    /// empty path, with the two methods here to name it, keeps `Layer.spec`
    /// non-optional for the other three kinds, which always have a source.
    pub fn unsaved() -> Self {
        SourceSpec::File(PathBuf::new())
    }

    /// Has this source no location at all? See [`SourceSpec::unsaved`].
    pub fn is_unsaved(&self) -> bool {
        matches!(self, SourceSpec::File(path) if path.as_os_str().is_empty())
    }

    /// Parse a source URI. A string with no scheme is a path.
    pub fn parse(source: &str) -> Result<Self> {
        if let Some(rest) = source.strip_prefix("file://") {
            return Ok(SourceSpec::File(PathBuf::from(rest)));
        }
        if source.starts_with("http://") || source.starts_with("https://") {
            return Ok(SourceSpec::Http(source.to_string()));
        }
        if let Some(rest) = source.strip_prefix("s3://") {
            let (profile, rest) = match rest.split_once('@') {
                Some((profile, rest)) => (profile.to_string(), rest),
                None => ("default".to_string(), rest),
            };
            let (bucket, key) = match rest.split_once('/') {
                Some((bucket, key)) => (bucket.to_string(), key.trim_end_matches('/').to_string()),
                None => (rest.to_string(), String::new()),
            };
            if bucket.is_empty() {
                bail!("s3 source `{source}` names no bucket");
            }
            return Ok(SourceSpec::S3 {
                profile,
                bucket,
                key,
            });
        }
        Ok(SourceSpec::File(PathBuf::from(source)))
    }

    /// The URI form, round-tripping through [`SourceSpec::parse`].
    pub fn uri(&self) -> String {
        match self {
            SourceSpec::File(path) => format!("file://{}", path.display()),
            SourceSpec::Http(url) => url.clone(),
            SourceSpec::S3 {
                profile,
                bucket,
                key,
            } => {
                if profile == "default" {
                    format!("s3://{bucket}/{key}")
                } else {
                    format!("s3://{profile}@{bucket}/{key}")
                }
            }
        }
    }

    /// A short name for the layer list: the last path component.
    pub fn short_name(&self) -> String {
        let full = self.text();
        full.trim_end_matches(is_separator)
            .rsplit(is_separator)
            .find(|part| !part.is_empty())
            .unwrap_or("layer")
            .to_string()
    }

    /// The file extension, lowercased, when the source names a file.
    pub fn extension(&self) -> Option<String> {
        let full = self.text();
        let name = full
            .trim_end_matches(is_separator)
            .rsplit(is_separator)
            .next()?;
        let ext = name.rsplit_once('.')?.1;
        Some(ext.to_ascii_lowercase())
    }

    /// The source as text, for the two questions above.
    fn text(&self) -> String {
        match self {
            SourceSpec::File(path) => path.to_string_lossy().into_owned(),
            SourceSpec::Http(url) => url.clone(),
            SourceSpec::S3 { bucket, key, .. } => {
                if key.is_empty() {
                    bucket.clone()
                } else {
                    key.clone()
                }
            }
        }
    }
}

/// Both path separators, because a source can be a Windows path or a URI and
/// the last component is wanted either way.
fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Credentials and endpoint for one S3-compatible service.
#[derive(Clone, Debug, Default)]
pub struct S3Profile {
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

impl S3Profile {
    /// Build an operator rooted at `bucket`, optionally under `root`.
    pub fn operator(&self, bucket: &str, root: &str) -> Result<opendal::Operator> {
        let mut builder = opendal::services::S3::default()
            .bucket(bucket)
            .region(&self.region);
        if !self.endpoint.is_empty() {
            builder = builder.endpoint(&self.endpoint);
        }
        if self.access_key.is_empty() {
            builder = builder.allow_anonymous();
        } else {
            builder = builder
                .access_key_id(&self.access_key)
                .secret_access_key(&self.secret_key);
        }
        if !root.is_empty() {
            builder = builder.root(&format!("/{}", root.trim_start_matches('/')));
        }
        Ok(opendal::Operator::new(builder)?.finish())
    }
}

/// The named S3 profiles this server knows about.
#[derive(Clone, Debug, Default)]
pub struct SourceRegistry {
    profiles: HashMap<String, S3Profile>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_profile(mut self, name: impl Into<String>, profile: S3Profile) -> Self {
        self.profiles.insert(name.into(), profile);
        self
    }

    pub fn profile(&self, name: &str) -> Result<&S3Profile> {
        self.profiles
            .get(name)
            .with_context(|| format!("no S3 profile named `{name}` is configured"))
    }

    /// An operator for a source, and the root-relative path inside it.
    ///
    /// `File` sources have no operator: they are read through
    /// `zarrs::filesystem`, which is why this returns `None` for them rather
    /// than an `opendal` filesystem operator. Two backends is one more than
    /// nobody wants, and the sync one is what makes a local read a read rather
    /// than a task.
    pub fn operator(&self, spec: &SourceSpec) -> Result<Option<opendal::Operator>> {
        match spec {
            SourceSpec::File(_) => Ok(None),
            SourceSpec::Http(url) => {
                let op = opendal::Operator::new(
                    opendal::services::Http::default().endpoint(url.trim_end_matches('/')),
                )?
                .finish();
                Ok(Some(op))
            }
            SourceSpec::S3 {
                profile,
                bucket,
                key,
            } => Ok(Some(self.profile(profile)?.operator(bucket, key)?)),
        }
    }
}

/// Read a source into memory, optionally only its first `limit` bytes.
///
/// The prefix form is what layer-kind detection uses: a `.npy` header says
/// whether the file is a volume or a table of points, and reading a gigabyte to
/// find that out would be a poor way to open a directory.
pub async fn read_bytes(
    registry: &SourceRegistry,
    spec: &SourceSpec,
    limit: Option<usize>,
) -> Result<Vec<u8>> {
    match registry.operator(spec)? {
        None => {
            let SourceSpec::File(path) = spec else {
                anyhow::bail!("source {} has no operator and is not a file", spec.uri());
            };
            match limit {
                None => std::fs::read(path).with_context(|| format!("reading {}", path.display())),
                Some(limit) => {
                    use std::io::Read;
                    let mut file = std::fs::File::open(path)
                        .with_context(|| format!("opening {}", path.display()))?;
                    let mut buffer = vec![0u8; limit];
                    let read = file
                        .read(&mut buffer)
                        .with_context(|| format!("reading {}", path.display()))?;
                    buffer.truncate(read);
                    Ok(buffer)
                }
            }
        }
        Some(op) => {
            let key = match spec {
                SourceSpec::S3 { .. } => String::new(),
                SourceSpec::Http(url) => url.rsplit('/').next().unwrap_or_default().to_string(),
                SourceSpec::File(_) => unreachable!("file sources have no operator"),
            };
            // Object storage can serve a range; ask for one rather than the
            // whole object when only the header is wanted.
            let data = match limit {
                None => op.read(&key).await,
                Some(limit) => op.read_with(&key).range(0..limit as u64).await,
            }
            .with_context(|| format!("reading {}", spec.uri()))?;
            Ok(data.to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_scheme() {
        assert_eq!(
            SourceSpec::parse("/data/run.zarr").unwrap(),
            SourceSpec::File(PathBuf::from("/data/run.zarr"))
        );
        assert_eq!(
            SourceSpec::parse("file:///data/run.zarr").unwrap(),
            SourceSpec::File(PathBuf::from("/data/run.zarr"))
        );
        assert_eq!(
            SourceSpec::parse("https://example.com/x.zarr").unwrap(),
            SourceSpec::Http("https://example.com/x.zarr".into())
        );
        assert_eq!(
            SourceSpec::parse("s3://bucket/a/b.zarr").unwrap(),
            SourceSpec::S3 {
                profile: "default".into(),
                bucket: "bucket".into(),
                key: "a/b.zarr".into()
            }
        );
        assert_eq!(
            SourceSpec::parse("s3://lab@bucket/a/b.zarr").unwrap(),
            SourceSpec::S3 {
                profile: "lab".into(),
                bucket: "bucket".into(),
                key: "a/b.zarr".into()
            }
        );
    }

    #[test]
    fn uri_round_trips() {
        for uri in [
            "file:///data/run.zarr",
            "https://example.com/x.zarr",
            "s3://bucket/a/b.zarr",
            "s3://lab@bucket/a/b.zarr",
        ] {
            let spec = SourceSpec::parse(uri).unwrap();
            assert_eq!(spec.uri(), uri);
            assert_eq!(SourceSpec::parse(&spec.uri()).unwrap(), spec);
        }
    }

    #[test]
    fn a_windows_path_still_has_a_last_component() {
        let spec = SourceSpec::parse(r"C:\\data\\run\\filled.npy").unwrap();
        assert_eq!(spec.short_name(), "filled.npy");
        assert_eq!(spec.extension().as_deref(), Some("npy"));
    }

    #[test]
    fn names_and_extensions() {
        let spec = SourceSpec::parse("/data/binary/ch0/filled.npy").unwrap();
        assert_eq!(spec.short_name(), "filled.npy");
        assert_eq!(spec.extension().as_deref(), Some("npy"));
        let spec = SourceSpec::parse("/data/run.zarr/").unwrap();
        assert_eq!(spec.short_name(), "run.zarr");
        assert_eq!(spec.extension().as_deref(), Some("zarr"));
    }
}
