use serde::{Deserialize, Serialize};

pub use reimagine_package_installer::ExtractionLimits;

use crate::ExpectedWorkerIdentity;

/// The manifest embedded inside a worker package archive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageManifest {
    /// Schema version for forward compatibility.
    pub schema_version: u16,
    /// Kind identifier (e.g., "burn-worker").
    pub package_kind: String,
    /// Human-readable package version (e.g., "0.1.0").
    #[serde(default = "default_version")]
    pub version: String,
    /// The expected worker identity for this package.
    pub identity: ExpectedWorkerIdentity,
    /// File entries in the package.
    pub files: Vec<PackageFileEntry>,
    /// Required disk space in bytes (extracted).
    pub required_size: u64,
    /// Number of entries in the archive.
    pub required_entries: usize,
    /// Worker wire protocol versions this package speaks. Packages
    /// predating the field default to v1, which the host's
    /// compatibility check rejects.
    #[serde(default = "default_protocol_version")]
    pub protocol_version_min: u16,
    #[serde(default = "default_protocol_version")]
    pub protocol_version_max: u16,
}

fn default_protocol_version() -> u16 {
    1
}

fn default_version() -> String {
    "0.0.0".to_string()
}

/// A single file entry in the package manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageFileEntry {
    /// Relative path within the package.
    pub path: String,
    /// SHA-256 hex digest.
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
    /// POSIX permissions (octal, e.g., 0o755).
    pub mode: u32,
    /// Whether this is the main executable.
    #[serde(default)]
    pub executable: bool,
}
