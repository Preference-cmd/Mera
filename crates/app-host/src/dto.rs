//! Cross-host shared data transfer objects.
//!
//! These types are the V1 wire contract exposed to host adapters
//! (such as Tauri). The shapes are intentionally minimal — they wrap
//! host-neutral types from `mera-core` so that the JSON surface stays
//! stable even as the underlying types evolve.

mod boards;
mod health;
mod mods;
mod projects;
mod scripts;
mod storage;

pub use boards::{BoardCommandResultDto, BoardSnapshotDto};
pub use health::{HealthDto, HealthResponse};
pub use mera_core::command::{CommandBatch, CommandResult, CommandStatus};
pub use mods::{
    ModBrowseResponseDto, ModDownloadTaskDto, ModEntryDto, ModHealthDto, ModInstallPreviewDto,
    ModListResponseDto, ModOperationSummaryDto, ModRuntimeEnvironmentDto, ModSandboxStatusDto,
    ModSourceDto, ModSourcesListDto, RemoteModCatalogItemDto, ThirdPartySourceDto,
};
pub use projects::{ProjectDto, ProjectMetadataInputDto};
pub use scripts::ScriptApprovalsSnapshotDto;
pub use storage::{
    ProjectStorageDto, StorageCategoriesDto, StorageCleanResultDto, StoragePreflightInputDto,
    StoragePreflightReason, StoragePreflightResultDto, StorageStatusDto, StorageWriteKind,
};
