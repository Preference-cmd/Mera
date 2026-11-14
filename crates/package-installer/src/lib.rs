//! Generic verified package extraction, atomic staging/promotion, and TUF
//! metadata verification shared across worker and mod installers.

#![deny(unsafe_code)]

pub mod archive;
pub mod fs_tx;
pub mod tuf;
pub mod tuf_error;
pub mod tuf_state;

pub use archive::{
    ArchiveError, ArchiveResult, ExtractionLimits, decompress_gzip, extract_tar_gz_safely,
    extract_tar_safely, read_tar_member, sha256_hex, validate_relative_path, verify_file_sha256,
};
pub use fs_tx::{FsLock, FsTxError, FsTxResult, atomic_promote_dir, atomic_write_file, sync_tree};
pub use tuf::{
    RoleConfig, RootMetadata, RootSigned, SignatureEntry, SnapshotMetaEntry, SnapshotMetadata,
    SnapshotSigned, TargetDesc, TargetsMetaEntry, TargetsMetadata, TargetsSigned,
    TimestampMetadata, TimestampSigned, TufKey, TufKeyVal, verify_root, verify_snapshot,
    verify_target_content, verify_targets, verify_timestamp,
};
pub use tuf_error::{CatalogError, CatalogResult, CatalogSigningKeyError};
pub use tuf_state::{
    TrustedCatalogState, TrustedStateError, load_trusted_state, save_trusted_state,
};
