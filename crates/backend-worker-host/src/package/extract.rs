use std::collections::HashSet;
use std::path::Path;

use reimagine_package_installer::{
    ArchiveError, decompress_gzip, extract_tar_safely, read_tar_member, validate_relative_path,
    verify_file_sha256,
};

use super::error::{PackageError, PackageResult};
use super::manifest::{ExtractionLimits, PackageManifest};

/// Extracts a tar.gz archive into a staging directory with
/// safety guarantees against traversal, link attacks, and
/// manifest mismatch.
pub struct PackageExtractor {
    limits: ExtractionLimits,
}

impl PackageExtractor {
    #[must_use]
    pub fn new(limits: ExtractionLimits) -> Self {
        Self { limits }
    }

    /// Extract a package archive into the staging directory.
    ///
    /// Verification steps:
    /// 1. Read and validate the embedded `package.json` manifest
    /// 2. Verify manifest matches expected identity (if provided)
    /// 3. Extract all entries with safety constraints
    /// 4. Verify extracted file hashes against manifest
    ///
    /// Returns the validated `PackageManifest`.
    pub fn extract(
        &self,
        archive_data: &[u8],
        staging_dir: &Path,
        expected_identity: Option<&crate::ExpectedWorkerIdentity>,
    ) -> PackageResult<PackageManifest> {
        let decompressed = decompress_gzip(archive_data, &self.limits).map_err(map_archive_err)?;
        let manifest = self.read_manifest(&decompressed)?;
        self.validate_manifest(&manifest, expected_identity)?;

        let extracted_files = extract_tar_safely(
            &decompressed,
            staging_dir,
            &self.limits,
            &["package.json", "package.spdx.json"],
            Some("package/"),
        )
        .map_err(map_archive_err)?;

        let manifest_paths = manifest
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<HashSet<_>>();
        if extracted_files != manifest_paths {
            return Err(PackageError::ManifestMismatch {
                field: "files".to_owned(),
                expected: format!("{manifest_paths:?}"),
                actual: format!("{extracted_files:?}"),
            });
        }
        for file in &manifest.files {
            let path = staging_dir.join(&file.path);
            let metadata = std::fs::metadata(&path)?;
            if metadata.len() != file.size {
                return Err(PackageError::ManifestMismatch {
                    field: format!("files[{}].size", file.path),
                    expected: file.size.to_string(),
                    actual: metadata.len().to_string(),
                });
            }
            verify_file_hash(&path, &file.sha256)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o777 != file.mode & 0o777 {
                    return Err(PackageError::ManifestMismatch {
                        field: format!("files[{}].mode", file.path),
                        expected: format!("{:o}", file.mode & 0o777),
                        actual: format!("{:o}", metadata.permissions().mode() & 0o777),
                    });
                }
            }
        }

        Ok(manifest)
    }

    fn validate_manifest(
        &self,
        manifest: &PackageManifest,
        expected_identity: Option<&crate::ExpectedWorkerIdentity>,
    ) -> PackageResult<()> {
        if manifest.schema_version != 1 {
            return manifest_mismatch("schema_version", "1", manifest.schema_version);
        }
        if manifest.package_kind != "burn-worker" {
            return manifest_mismatch("package_kind", "burn-worker", &manifest.package_kind);
        }
        if let Some(expected) = expected_identity
            && &manifest.identity != expected
        {
            return manifest_mismatch(
                "identity",
                format!("{expected:?}"),
                format!("{:?}", manifest.identity),
            );
        }
        if manifest.files.len() > self.limits.max_entries {
            return Err(PackageError::EntryCountLimit {
                max: self.limits.max_entries,
                actual: manifest.files.len(),
            });
        }
        let required_size = manifest.files.iter().try_fold(0u64, |total, file| {
            total
                .checked_add(file.size)
                .ok_or(PackageError::ExpandedSizeLimit {
                    max: self.limits.max_expanded_size,
                    actual: u64::MAX,
                })
        })?;
        if manifest.required_entries != manifest.files.len() {
            return manifest_mismatch(
                "required_entries",
                manifest.files.len(),
                manifest.required_entries,
            );
        }
        if manifest.required_size != required_size {
            return manifest_mismatch("required_size", required_size, manifest.required_size);
        }
        let mut paths = HashSet::new();
        for file in &manifest.files {
            validate_relative_path(&file.path).map_err(map_archive_err)?;
            if !paths.insert(file.path.clone()) {
                return Err(PackageError::DuplicatePath {
                    entry: file.path.clone(),
                });
            }
            if file.size > self.limits.max_entry_size {
                return Err(PackageError::ExpandedSizeLimit {
                    max: self.limits.max_entry_size,
                    actual: file.size,
                });
            }
            if file.executable && file.mode & 0o111 == 0 {
                return manifest_mismatch(
                    format!("files[{}].mode", file.path).as_str(),
                    "an executable mode",
                    format!("{:o}", file.mode),
                );
            }
        }
        let executables = manifest.files.iter().filter(|file| file.executable).count();
        if executables != 1 {
            return manifest_mismatch("executable_count", 1, executables);
        }
        Ok(())
    }

    fn read_manifest(&self, archive_data: &[u8]) -> PackageResult<PackageManifest> {
        let content = read_tar_member(
            archive_data,
            "package.json",
            Some("package/"),
            16 * 1024 * 1024,
        )
        .map_err(map_archive_err)?
        .ok_or(PackageError::ManifestMissing)?;
        serde_json::from_slice(&content).map_err(|e| PackageError::ManifestMismatch {
            field: "json".to_string(),
            expected: "valid package.json".to_string(),
            actual: e.to_string(),
        })
    }
}

fn map_archive_err(error: ArchiveError) -> PackageError {
    match error {
        ArchiveError::EntryCountLimit { max, actual } => {
            PackageError::EntryCountLimit { max, actual }
        }
        ArchiveError::ExpandedSizeLimit { max, actual } => {
            PackageError::ExpandedSizeLimit { max, actual }
        }
        ArchiveError::InvalidPath { entry } => PackageError::InvalidPath { entry },
        ArchiveError::PathTraversal { entry } => PackageError::PathTraversal { entry },
        ArchiveError::SymlinkRejected { entry } => PackageError::SymlinkRejected { entry },
        ArchiveError::HardlinkRejected { entry } => PackageError::HardlinkRejected { entry },
        ArchiveError::DuplicatePath { entry } => PackageError::DuplicatePath { entry },
        ArchiveError::UnexpectedFileType { entry, kind } => {
            PackageError::UnexpectedFileType { entry, kind }
        }
        ArchiveError::HashMismatch { entry } => PackageError::HashMismatch { entry },
        ArchiveError::Io { message } => PackageError::Extraction { message },
    }
}

fn manifest_mismatch<T: ToString, U: ToString, R>(
    field: &str,
    expected: T,
    actual: U,
) -> PackageResult<R> {
    Err(PackageError::ManifestMismatch {
        field: field.to_owned(),
        expected: expected.to_string(),
        actual: actual.to_string(),
    })
}

/// Verify that an extracted file's hash matches the manifest entry.
pub fn verify_file_hash(path: &Path, expected_sha256: &str) -> PackageResult<()> {
    verify_file_sha256(path, expected_sha256).map_err(map_archive_err)
}

#[cfg(test)]
mod tests {
    use super::super::manifest::PackageFileEntry;
    use super::*;
    use sha2::{Digest, Sha256};

    fn create_test_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);

            // Add package.json manifest
            let manifest = PackageManifest {
                schema_version: 1,
                package_kind: "burn-worker".to_string(),
                version: "0.0.0-test".to_string(),
                identity: crate::ExpectedWorkerIdentity {
                    backend_instance_id: crate::BackendInstanceId("burn:wgpu:default".to_string()),
                    installation_id: crate::WorkerInstallationId("burn-wgpu-v1".to_string()),
                    backend_kind: "burn".to_string(),
                    target: "aarch64-apple-darwin".to_string(),
                    manifest_digest: "abc123".to_string(),
                },
                files: files
                    .iter()
                    .map(|(name, data)| {
                        let hash = hex::encode(Sha256::digest(data));
                        let executable = name.contains("burn-worker");
                        PackageFileEntry {
                            path: name.to_string(),
                            sha256: hash,
                            size: data.len() as u64,
                            mode: if executable { 0o755 } else { 0o644 },
                            executable,
                        }
                    })
                    .collect(),
                required_size: files.iter().map(|(_, d)| d.len() as u64).sum(),
                required_entries: files.len(),
                protocol_version_min: 1,
                protocol_version_max: 1,
            };
            let manifest_json = serde_json::to_vec(&manifest).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_path("package/package.json").unwrap();
            header.set_size(manifest_json.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, &manifest_json[..]).unwrap();

            // Add files
            for (name, data) in files {
                let path = format!("package/{name}");
                let mut header = tar::Header::new_gnu();
                header.set_path(&path).unwrap();
                header.set_size(data.len() as u64);
                header.set_mode(if name.contains("burn-worker") {
                    0o755
                } else {
                    0o644
                });
                header.set_cksum();
                tar.append(&header, &data[..]).unwrap();
            }

            tar.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_valid_package() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");

        let archive = create_test_archive(&[("burn-worker", b"binary content")]);
        let extractor = PackageExtractor::new(ExtractionLimits::default());

        let manifest = extractor
            .extract(&archive, &staging, None)
            .expect("extraction should succeed");

        assert_eq!(manifest.package_kind, "burn-worker");
        assert!(staging.join("burn-worker").exists());
    }

    #[test]
    fn extract_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");

        // Create a tar with a valid manifest plus a raw traversal entry.
        let mut tar_bytes = Vec::new();
        let worker = b"binary";

        // Build the manifest
        let manifest = PackageManifest {
            schema_version: 1,
            package_kind: "burn-worker".to_string(),
            version: "0.0.0-test".to_string(),
            identity: crate::ExpectedWorkerIdentity {
                backend_instance_id: crate::BackendInstanceId("burn:wgpu:default".to_string()),
                installation_id: crate::WorkerInstallationId("burn-wgpu-v1".to_string()),
                backend_kind: "burn".to_string(),
                target: "aarch64-apple-darwin".to_string(),
                manifest_digest: "abc123".to_string(),
            },
            files: vec![PackageFileEntry {
                path: "burn-worker".to_owned(),
                sha256: hex::encode(Sha256::digest(worker)),
                size: worker.len() as u64,
                mode: 0o755,
                executable: true,
            }],
            required_size: worker.len() as u64,
            required_entries: 1,
            protocol_version_min: 1,
            protocol_version_max: 1,
        };
        let manifest_json = serde_json::to_vec(&manifest).unwrap();

        // Write the manifest entry via tar builder
        {
            let gz = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_path("package/package.json").unwrap();
            header.set_size(manifest_json.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, &manifest_json[..]).unwrap();
            let mut worker_header = tar::Header::new_gnu();
            worker_header.set_path("package/burn-worker").unwrap();
            worker_header.set_size(worker.len() as u64);
            worker_header.set_mode(0o755);
            worker_header.set_cksum();
            tar.append(&worker_header, &worker[..]).unwrap();

            // Bypass tar::Header::set_path's own traversal rejection so the
            // extractor receives an actually malicious archive entry.
            let mut hdr = tar::Header::new_gnu();
            let raw_name = b"package/../../evil";
            hdr.as_mut_bytes()[..raw_name.len()].copy_from_slice(raw_name);
            hdr.set_size(4);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            tar.append(&hdr, b"data" as &[u8]).unwrap();
            tar.finish().unwrap();
        }

        let extractor = PackageExtractor::new(ExtractionLimits::default());
        let result = extractor.extract(&tar_bytes, &staging, None);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    #[test]
    fn extract_rejects_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");

        // Use tar builder which rejects symlinks with non-empty names
        // We test that the extraction logic correctly handles
        // directory type entries
        let mut buf = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);

            let worker = b"binary";
            let manifest = PackageManifest {
                schema_version: 1,
                package_kind: "burn-worker".to_string(),
                version: "0.0.0-test".to_string(),
                identity: crate::ExpectedWorkerIdentity {
                    backend_instance_id: crate::BackendInstanceId("burn:wgpu:default".to_string()),
                    installation_id: crate::WorkerInstallationId("burn-wgpu-v1".to_string()),
                    backend_kind: "burn".to_string(),
                    target: "aarch64-apple-darwin".to_string(),
                    manifest_digest: "abc123".to_string(),
                },
                files: vec![PackageFileEntry {
                    path: "burn-worker".to_owned(),
                    sha256: hex::encode(Sha256::digest(worker)),
                    size: worker.len() as u64,
                    mode: 0o755,
                    executable: true,
                }],
                required_size: worker.len() as u64,
                required_entries: 1,
                protocol_version_min: 1,
                protocol_version_max: 1,
            };
            let manifest_json = serde_json::to_vec(&manifest).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_path("package/package.json").unwrap();
            header.set_size(manifest_json.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, &manifest_json[..]).unwrap();

            let mut worker_header = tar::Header::new_gnu();
            worker_header.set_path("package/burn-worker").unwrap();
            worker_header.set_size(worker.len() as u64);
            worker_header.set_mode(0o755);
            worker_header.set_cksum();
            tar.append(&worker_header, &worker[..]).unwrap();

            // Add a symlink that is not part of the signed manifest.
            let mut header = tar::Header::new_gnu();
            header.set_path("package/latest").unwrap();
            header.set_link_name("burn-worker").unwrap();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            tar.append(&header, &[][..]).unwrap();

            tar.finish().unwrap();
        }

        let extractor = PackageExtractor::new(ExtractionLimits::default());
        let result = extractor.extract(&buf, &staging, None);
        assert!(matches!(result, Err(PackageError::SymlinkRejected { .. })));
    }
}
