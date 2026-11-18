use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mera_app_host::dto::{StoragePreflightInputDto, StoragePreflightReason, StorageWriteKind};
use mera_app_host::{
    AppHostResult, DiskSpaceInfo, DiskSpaceProvider, ModStorePaths, RecoveryHook,
    StorageBudgetService, StorageCategory, WorkspaceHostBuilder, WorkspaceScope,
};
use mera_backend_worker_host::WorkerStorePaths;
use mera_config::{AppConfig, AppPaths, FIFTY_GIB_BYTES, StorageBudgetConfig};
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::event::Timestamp;
use mera_core::model::ProjectId;
use mera_core::project::ProjectMetadata;
use mera_package_installer::FsLock;
use tempfile::tempdir;

#[derive(Debug, Clone)]
struct MockDiskSpaceProvider {
    state: Arc<Mutex<DiskSpaceInfo>>,
}

impl MockDiskSpaceProvider {
    fn new(available_bytes: u64, total_bytes: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(DiskSpaceInfo {
                available_bytes,
                total_bytes,
            })),
        }
    }

    fn set(&self, available_bytes: u64, total_bytes: u64) {
        *self.state.lock().unwrap() = DiskSpaceInfo {
            available_bytes,
            total_bytes,
        };
    }
}

impl DiskSpaceProvider for MockDiskSpaceProvider {
    fn disk_space(&self, _path: &Path) -> AppHostResult<DiskSpaceInfo> {
        Ok(*self.state.lock().unwrap())
    }
}

#[derive(Debug)]
struct MockHistoryHook {
    reclaimable: Mutex<u64>,
    calls: Mutex<Vec<u64>>,
}

impl MockHistoryHook {
    fn new(reclaimable: u64) -> Self {
        Self {
            reclaimable: Mutex::new(reclaimable),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<u64> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl RecoveryHook for MockHistoryHook {
    fn name(&self) -> &'static str {
        "file_history_stub"
    }

    fn priority(&self) -> u8 {
        3
    }

    async fn estimate_reclaimable_bytes(&self) -> u64 {
        *self.reclaimable.lock().unwrap()
    }

    async fn reclaim(&self, target_bytes: u64) -> AppHostResult<u64> {
        self.calls.lock().unwrap().push(target_bytes);
        let mut avail = self.reclaimable.lock().unwrap();
        let freed = (*avail).min(target_bytes);
        *avail -= freed;
        Ok(freed)
    }
}

fn human_ctx() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Human).with_id("test-user"),
        CommandProvenance::Direct,
    )
}

fn write_bytes(path: &Path, len: usize) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, vec![0xABu8; len]).unwrap();
}

#[tokio::test]
async fn accounting_covers_all_managed_roots_and_excludes_external_dirs() {
    let temp = tempdir().unwrap();
    let workspace_root = temp.path().join("data");
    let app_data_root = temp.path().join("app-data");
    let mods_root = temp.path().join("mods");
    let external_folder = temp.path().join("user-folder");
    let external_models = temp.path().join("external-models");

    std::fs::create_dir_all(&external_folder).unwrap();
    write_bytes(&external_folder.join("huge-user-file.bin"), 200_000);
    write_bytes(&external_models.join("huge-ext-model.bin"), 300_000);

    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-accounting"), &workspace_root)
        .app_data_root(&app_data_root)
        .mods_root(&mods_root)
        .build()
        .await
        .unwrap();

    // Register external folder on default project; must NOT be counted.
    host.project_service()
        .add_folder(&ProjectId::new("default"), &external_folder)
        .await
        .unwrap();

    // Populate managed directories.
    write_bytes(&workspace_root.join("library/assets/aa/hash1"), 1_000);
    write_bytes(&workspace_root.join("library/scratch/tmp1.bin"), 250);
    write_bytes(
        &workspace_root.join("projects/default/assets/bb/hash2"),
        2_000,
    );
    write_bytes(&workspace_root.join("projects/default/history/h1.bin"), 400);
    write_bytes(&workspace_root.join("projects/default/scratch/s1.bin"), 150);
    write_bytes(&workspace_root.join("models/m1/weights.bin"), 3_000);
    write_bytes(&workspace_root.join("tmp/ephemeral.bin"), 100);
    write_bytes(&workspace_root.join("thumbnails/t1.png"), 50);
    write_bytes(&workspace_root.join("input/old.png"), 60);
    write_bytes(&workspace_root.join("output/old.png"), 70);
    write_bytes(&workspace_root.join("workflows/legacy.json"), 80);

    write_bytes(&app_data_root.join("workers/installed/w1/bin"), 5_000);
    write_bytes(&app_data_root.join("workers/inventory/w1.json"), 120);
    write_bytes(&app_data_root.join("workers/staging/abandoned/pkg"), 300);

    write_bytes(&mods_root.join("packages/m1/0.1.0/mod.py"), 4_000);
    write_bytes(&mods_root.join("data/m1/state.json"), 200);
    write_bytes(&mods_root.join("runtime/uv/uv"), 800);
    write_bytes(&mods_root.join("staging/old-mod/archive"), 350);
    write_bytes(&mods_root.join("runtime/uv-cache/wheel.whl"), 450);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            &external_models,
            workspace_root.join("models/linked-external"),
        )
        .unwrap();
    }

    let categories = host.storage_budget().calibrate().await.unwrap();
    let default_proj_overhead =
        mera_app_host::storage_budget::dir_size_sync(&workspace_root.join("projects/default"))
            - 2_000
            - 400
            - 150;
    let workers_index_overhead = mera_app_host::storage_budget::dir_size_sync(
        &app_data_root.join("workers/inventory/index.json"),
    );

    assert_eq!(categories.library_bytes, 1_000);
    assert_eq!(
        categories.projects_total_bytes,
        2_000 + 400 + default_proj_overhead
    );
    assert_eq!(categories.history_total_bytes, 400);
    assert_eq!(categories.models_bytes, 3_000);
    assert_eq!(categories.workers_bytes, 5_120 + workers_index_overhead);
    assert_eq!(categories.mods_bytes, 5_000);
    assert_eq!(
        categories.cache_bytes,
        250 + 150 + 100 + 50 + 300 + 350 + 450
    );
    assert_eq!(categories.legacy_bytes, 60 + 70 + 80);

    let status = host.storage_budget().status().await.unwrap();
    assert_eq!(status.total_bytes, categories.total_bytes());
    assert!(status.total_bytes < 50_000);
    assert_eq!(status.projects.len(), 1);
    assert_eq!(status.projects[0].project_id, "default");
    assert_eq!(status.projects[0].name, "Default");
    assert_eq!(status.projects[0].history_bytes, 400);

    // Verify camelCase IPC JSON shape expected by UI/Tauri.
    let json = serde_json::to_value(&status).unwrap();
    assert!(json.get("totalBytes").is_some());
    assert!(json.get("showFiftyGbNudge").is_some());
    assert!(json.get("scanRunning").is_some());
    assert_eq!(
        json["categories"]["workersBytes"],
        5_120 + workers_index_overhead
    );
    assert_eq!(json["categories"]["modsBytes"], 5_000);
    assert_eq!(json["categories"]["libraryRemovedBytes"], 0);
}

#[tokio::test]
async fn set_budget_rejects_zero_limit() {
    let temp = tempdir().unwrap();
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-zero"), temp.path())
        .build()
        .await
        .unwrap();

    let err = host.storage_budget().set_budget(Some(0)).await.unwrap_err();
    assert!(err.to_string().contains("greater than 0"));
}

#[tokio::test]
async fn incremental_accounting_matches_full_scan_within_one_percent() {
    let temp = tempdir().unwrap();
    let workspace_root = temp.path().join("data");
    let app_data_root = temp.path().join("app-data");
    let mods_root = temp.path().join("mods");

    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-incremental"), &workspace_root)
        .app_data_root(&app_data_root)
        .mods_root(&mods_root)
        .build()
        .await
        .unwrap();

    // Seed baseline and calibrate.
    write_bytes(&workspace_root.join("models/base.bin"), 100_000);
    host.storage_budget().calibrate().await.unwrap();

    // Add a large library file through LibraryService (triggers AssetStore incremental callback).
    let src_file = temp.path().join("sample_asset.bin");
    write_bytes(&src_file, 250_000);
    let ctx = human_ctx();
    host.library_service()
        .add(vec![src_file], &ctx)
        .await
        .unwrap();

    // Create and delete a project (ProjectService subtracts deleted project bytes).
    let p2 = ProjectId::new("ephemeral");
    host.project_service()
        .create_project(
            p2.clone(),
            ProjectMetadata::new(
                "Ephemeral",
                "desc",
                Timestamp::new("2026-11-18T00:00:00Z"),
                Timestamp::new("2026-11-18T00:00:00Z"),
            ),
        )
        .await
        .unwrap();
    let p2_overhead =
        mera_app_host::storage_budget::dir_size_sync(&workspace_root.join("projects/ephemeral"));
    host.storage_budget()
        .record_added(StorageCategory::Projects, p2_overhead);
    host.project_service().delete_project(&p2).await.unwrap();

    // Add cache and then clean it.
    write_bytes(&workspace_root.join("tmp/work.bin"), 20_000);
    host.storage_budget()
        .record_added(StorageCategory::Cache, 20_000);
    host.storage_budget().clean_cache().await.unwrap();

    let incremental_total = host.storage_budget().cached_categories().total_bytes();
    let full_total = host
        .storage_budget()
        .full_scan()
        .await
        .unwrap()
        .total_bytes();
    let diff = incremental_total.abs_diff(full_total);
    let pct_diff = (diff as f64) / (full_total.max(1) as f64);

    assert!(
        pct_diff < 0.01,
        "incremental ({incremental_total}) vs full ({full_total}) diff exceeded 1%: {pct_diff:.4}"
    );
}

#[tokio::test]
async fn set_budget_recovers_to_ninety_percent_in_priority_order_and_protects_removed_library_entries()
 {
    let temp = tempdir().unwrap();
    let workspace_root = temp.path().join("data");
    let app_data_root = temp.path().join("app-data");
    let mods_root = temp.path().join("mods");

    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-recovery"), &workspace_root)
        .app_data_root(&app_data_root)
        .mods_root(&mods_root)
        .build()
        .await
        .unwrap();

    // Add a library item and mark it removed (MUST NOT be auto-reclaimed!).
    let lib_src = temp.path().join("protected.bin");
    write_bytes(&lib_src, 1_000);
    let ctx = human_ctx();
    let added = host
        .library_service()
        .add(vec![lib_src], &ctx)
        .await
        .unwrap();
    let removed_id = added.items[0].id.clone();
    host.library_service()
        .remove(vec![removed_id.clone()], &ctx)
        .await
        .unwrap();

    // Protected project file.
    let proj_file = workspace_root.join("projects/default/assets/aa/proj.bin");
    write_bytes(&proj_file, 500);

    let baseline = host.storage_budget().calibrate().await.unwrap();
    assert_eq!(baseline.library_removed_bytes, 1_000);
    let protected_total = baseline.total_bytes();

    // Priority 1 caches: 400 bytes across tmp, library/scratch, project scratch, abandoned worker/mod staging.
    write_bytes(&workspace_root.join("tmp/p1.bin"), 100);
    write_bytes(&workspace_root.join("library/scratch/p1.bin"), 100);
    write_bytes(&workspace_root.join("projects/default/scratch/p1.bin"), 100);
    write_bytes(&app_data_root.join("workers/staging/old-worker/bin"), 50);
    write_bytes(&mods_root.join("staging/old-mod/bin"), 50);

    // Priority 2 caches: 600 bytes across thumbnails and uv-cache.
    write_bytes(&workspace_root.join("thumbnails/thumb.png"), 300);
    write_bytes(&mods_root.join("runtime/uv-cache/pkg.whl"), 300);

    host.storage_budget().calibrate().await.unwrap();

    // Set limit such that 90% target = protected_total + 300,
    // which requires reclaiming all 400 bytes of Priority 1 plus 300 bytes of Priority 2!
    let target_after_reclaim = protected_total + 300;
    let limit = (target_after_reclaim * 100) / 90;
    let status = host.storage_budget().set_budget(Some(limit)).await.unwrap();

    assert!(
        status.total_bytes <= (limit * 90) / 100,
        "total {} should be <= 90% target {}",
        status.total_bytes,
        (limit * 90) / 100
    );

    // All Priority 1 items must be gone.
    assert!(!workspace_root.join("tmp/p1.bin").exists());
    assert!(!workspace_root.join("library/scratch/p1.bin").exists());
    assert!(
        !workspace_root
            .join("projects/default/scratch/p1.bin")
            .exists()
    );
    assert!(!app_data_root.join("workers/staging/old-worker").exists());
    assert!(!mods_root.join("staging/old-mod").exists());

    // Priority 2 had at least one item reclaimed.
    let thumb_exists = workspace_root.join("thumbnails/thumb.png").exists();
    let uv_exists = mods_root.join("runtime/uv-cache/pkg.whl").exists();
    assert!(!thumb_exists || !uv_exists);

    // Protected project file and removed library entry MUST still exist!
    assert!(proj_file.exists());
    assert_eq!(status.categories.library_removed_bytes, 1_000);
    let listed = host
        .library_service()
        .list(&mera_app_host::LibraryListFilter {
            include_removed: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].removed);

    // Journal was recorded as completed.
    let journal_bytes = std::fs::read(host.storage_budget().cleanup_journal_path()).unwrap();
    let journal: mera_app_host::CleanupJournal = serde_json::from_slice(&journal_bytes).unwrap();
    assert_eq!(journal.state, mera_app_host::CleanupRunState::Completed);
}

#[tokio::test]
async fn disk_floor_triggers_recovery_without_budget_limit() {
    let temp = tempdir().unwrap();
    let app_paths = AppPaths::new(temp.path().join("data"));
    app_paths.ensure_all().await.unwrap();
    let worker_paths = WorkerStorePaths::new(temp.path().join("app-data"));
    let mod_paths = ModStorePaths::new(temp.path().join("app-data"));

    // 10% floor of 10_000 = 1_000; available = 700 (300 short).
    let disk_mock = Arc::new(MockDiskSpaceProvider::new(700, 10_000));
    let config = Arc::new(AppConfig::new(app_paths.clone()));
    let service = StorageBudgetService::with_disk_space_provider(
        config,
        worker_paths,
        mod_paths.clone(),
        disk_mock.clone(),
    );

    // Priority 1 has 400 bytes; Priority 2 has 500 bytes.
    write_bytes(&app_paths.tmp_dir().join("old.tmp"), 400);
    write_bytes(&mod_paths.uv_cache_dir().join("wheel.whl"), 500);
    service.calibrate().await.unwrap();

    let status_before = service.status().await.unwrap();
    assert!(status_before.below_disk_floor);
    assert_eq!(status_before.disk_floor_bytes, Some(1_000));

    let freed = service.recover_if_needed().await.unwrap();
    assert_eq!(freed, 400);
    // Priority 1 satisfied the 300-byte shortfall, so Priority 2 must remain untouched.
    assert!(!app_paths.tmp_dir().join("old.tmp").exists());
    assert!(mod_paths.uv_cache_dir().join("wheel.whl").exists());
}

#[tokio::test]
async fn priority3_hook_runs_only_after_priority1_and_2_are_exhausted() {
    let temp = tempdir().unwrap();
    let app_paths = AppPaths::new(temp.path().join("data"));
    app_paths.ensure_all().await.unwrap();
    let worker_paths = WorkerStorePaths::new(temp.path().join("app-data"));
    let mod_paths = ModStorePaths::new(temp.path().join("app-data"));
    let disk_mock = Arc::new(MockDiskSpaceProvider::new(50_000, 100_000));

    let service = StorageBudgetService::with_disk_space_provider(
        Arc::new(AppConfig::new(app_paths.clone())),
        worker_paths,
        mod_paths,
        disk_mock,
    );
    let hook = Arc::new(MockHistoryHook::new(500));
    service.register_recovery_hook(hook.clone());

    write_bytes(&app_paths.models_dir().join("model.bin"), 1_000);
    write_bytes(&app_paths.tmp_dir().join("cache.tmp"), 200);
    service.calibrate().await.unwrap();

    // Limit = 1000 => 90% target = 900. Total = 1200.
    // Reclaiming tmp (200) leaves 1000 (> 900), so Priority 3 hook is asked for 100 bytes.
    service.set_budget(Some(1_000)).await.unwrap();
    assert_eq!(hook.calls(), vec![100]);
}

#[tokio::test]
async fn active_staging_is_never_deleted_while_locked_or_registered() {
    let temp = tempdir().unwrap();
    let app_paths = AppPaths::new(temp.path().join("data"));
    app_paths.ensure_all().await.unwrap();
    let worker_paths = WorkerStorePaths::new(temp.path().join("app-data"));
    let mod_paths = ModStorePaths::new(temp.path().join("app-data"));

    let service = StorageBudgetService::new(
        Arc::new(AppConfig::new(app_paths.clone())),
        worker_paths.clone(),
        mod_paths.clone(),
    );

    let worker_staging = worker_paths.staging_dir("w-active");
    let mod_staging_locked = mod_paths.staging_root().join("m-locked");
    let mod_staging_registered = mod_paths.staging_root().join("m-registered");
    write_bytes(&worker_staging.join("bin"), 200);
    write_bytes(&mod_staging_locked.join("mod.py"), 200);
    write_bytes(&mod_staging_registered.join("mod2.py"), 200);

    // Hold worker install lock and register mod staging.
    let _worker_lock = FsLock::acquire(&worker_paths.install_lock_path()).unwrap();
    service.register_active_staging(&mod_staging_registered);

    // Without mod lock, mod_staging_locked is abandoned while registered & worker_locked are active.
    let clean = service.clean_cache().await.unwrap();
    assert_eq!(clean.freed_bytes, 200);
    assert!(worker_staging.exists());
    assert!(!mod_staging_locked.exists());
    assert!(mod_staging_registered.exists());

    // Release locks -> now clean_cache removes the remaining staging dirs.
    drop(_worker_lock);
    service.unregister_active_staging(&mod_staging_registered);
    let clean2 = service.clean_cache().await.unwrap();
    assert_eq!(clean2.freed_bytes, 400);
    assert!(!worker_staging.exists());
    assert!(!mod_staging_registered.exists());
}

#[tokio::test]
async fn crashed_journal_resumes_cache_cleanup_and_rejects_protected_paths() {
    let temp = tempdir().unwrap();
    let app_paths = AppPaths::new(temp.path().join("data"));
    app_paths.ensure_all().await.unwrap();
    let worker_paths = WorkerStorePaths::new(temp.path().join("app-data"));
    let mod_paths = ModStorePaths::new(temp.path().join("app-data"));

    let service = StorageBudgetService::new(
        Arc::new(AppConfig::new(app_paths.clone())),
        worker_paths,
        mod_paths,
    );

    let pending_cache = app_paths.tmp_dir().join("crashed-cleanup.tmp");
    let protected_asset = app_paths.library_dir().join("assets/aa/protected.bin");
    write_bytes(&pending_cache, 256);
    write_bytes(&protected_asset, 512);

    // Simulate an interrupted InProgress cleanup journal that also contains a non-cache path.
    let crashed_journal = mera_app_host::CleanupJournal {
        schema_version: 1,
        run_id: "crashed-1".to_owned(),
        started_at: "2026-11-18T00:00:00Z".to_owned(),
        completed_at: None,
        reason: "test_crash".to_owned(),
        state: mera_app_host::CleanupRunState::InProgress,
        entries: vec![
            mera_app_host::CleanupJournalEntry {
                path: pending_cache.clone(),
                priority: 1,
                kind: "tmp".to_owned(),
                estimated_bytes: 256,
                freed_bytes: 0,
                deleted_files: 0,
                status: mera_app_host::CleanupEntryStatus::Pending,
            },
            mera_app_host::CleanupJournalEntry {
                path: protected_asset.clone(),
                priority: 1,
                kind: "tampered".to_owned(),
                estimated_bytes: 512,
                freed_bytes: 0,
                deleted_files: 0,
                status: mera_app_host::CleanupEntryStatus::Pending,
            },
        ],
    };
    tokio::fs::write(
        service.cleanup_journal_path(),
        serde_json::to_vec_pretty(&crashed_journal).unwrap(),
    )
    .await
    .unwrap();

    service.startup_calibrate_and_recover().await.unwrap();

    assert!(!pending_cache.exists());
    assert!(protected_asset.exists());

    let resumed: mera_app_host::CleanupJournal = serde_json::from_slice(
        &tokio::fs::read(service.cleanup_journal_path())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resumed.state, mera_app_host::CleanupRunState::Completed);
    assert_eq!(
        resumed.entries[0].status,
        mera_app_host::CleanupEntryStatus::Completed
    );
    assert!(matches!(
        resumed.entries[1].status,
        mera_app_host::CleanupEntryStatus::Failed { .. }
    ));
}

#[tokio::test]
async fn preflight_write_rules_and_fifty_gb_nudge() {
    let temp = tempdir().unwrap();
    let app_paths = AppPaths::new(temp.path().join("data"));
    app_paths.ensure_all().await.unwrap();
    let worker_paths = WorkerStorePaths::new(temp.path().join("app-data"));
    let mod_paths = ModStorePaths::new(temp.path().join("app-data"));
    let disk_mock = Arc::new(MockDiskSpaceProvider::new(
        100 * 1024 * 1024 * 1024,
        200 * 1024 * 1024 * 1024,
    ));

    let config = Arc::new(AppConfig::new(app_paths.clone()));
    let service = StorageBudgetService::with_disk_space_provider(
        Arc::clone(&config),
        worker_paths,
        mod_paths,
        disk_mock.clone(),
    );

    // Start calibrated at 0 bytes.
    service.calibrate().await.unwrap();

    // 50 GiB nudge: false at exact 50 GiB, true above 50 GiB when limit is None and not dismissed.
    service.record_added(StorageCategory::Models, FIFTY_GIB_BYTES);
    assert!(!service.status().await.unwrap().show_fifty_gb_nudge);
    service.record_added(StorageCategory::Models, 1);
    assert!(service.status().await.unwrap().show_fifty_gb_nudge);

    // Dismissing nudge hides it and persists.
    let after_dismiss = service.dismiss_nudge().await.unwrap();
    assert!(after_dismiss.nudge_dismissed);
    assert!(!after_dismiss.show_fifty_gb_nudge);
    let (saved_cfg, _) = config
        .config::<StorageBudgetConfig>()
        .unwrap()
        .load()
        .await
        .unwrap();
    assert!(saved_cfg.nudge_dismissed);

    // Reset disk mock and calibrate to clean baseline for preflight checks.
    service.calibrate().await.unwrap();
    disk_mock.set(500 * 1024 * 1024, 1_000 * 1024 * 1024); // 50% free; floor = 100 MiB
    service.set_budget(Some(50 * 1024 * 1024)).await.unwrap(); // 50 MiB budget
    service.record_added(StorageCategory::Models, 45 * 1024 * 1024); // 45 MiB used

    // Library <= 10 MiB does NOT require confirmation even if it would exceed 50 MiB budget.
    let lib_small = service
        .preflight_write(StorageWriteKind::Library, 10 * 1024 * 1024)
        .await
        .unwrap();
    assert!(lib_small.would_exceed_budget);
    assert!(!lib_small.requires_confirm);
    assert_eq!(lib_small.reason, None);

    // Library > 10 MiB DOES require confirmation when it would exceed budget.
    let lib_large = service
        .preflight_write(StorageWriteKind::Library, 10 * 1024 * 1024 + 1)
        .await
        .unwrap();
    assert!(lib_large.would_exceed_budget);
    assert!(lib_large.requires_confirm);
    assert_eq!(
        lib_large.reason,
        Some(StoragePreflightReason::WouldExceedBudget)
    );

    // Model / Worker / Mod require confirmation even for small writes when exceeding budget.
    for kind in [
        StorageWriteKind::Model,
        StorageWriteKind::Worker,
        StorageWriteKind::Mod,
    ] {
        let res = service
            .preflight_write(kind, 6 * 1024 * 1024)
            .await
            .unwrap();
        assert!(res.would_exceed_budget);
        assert!(res.requires_confirm);
        assert_eq!(res.reason, Some(StoragePreflightReason::WouldExceedBudget));

        let under = service.preflight_write(kind, 1024).await.unwrap();
        assert!(!under.would_exceed_budget);
        assert!(!under.requires_confirm);
    }

    // Disk floor check: drop available space below 10% floor (90 MiB < 100 MiB) with unlimited budget.
    service.set_budget(None).await.unwrap();
    disk_mock.set(90 * 1024 * 1024, 1_000 * 1024 * 1024);
    let model_floor = service
        .preflight_write(StorageWriteKind::Model, 1024)
        .await
        .unwrap();
    assert!(model_floor.below_disk_floor);
    assert!(model_floor.requires_confirm);
    assert_eq!(
        model_floor.reason,
        Some(StoragePreflightReason::BelowDiskFloor)
    );

    let lib_small_floor = service
        .preflight_write(StorageWriteKind::Library, 1024)
        .await
        .unwrap();
    assert!(lib_small_floor.below_disk_floor);
    assert!(!lib_small_floor.requires_confirm);

    // Local paths sizing: large (>10 MiB) file on disk triggers Library confirmation even with estimated_bytes: 0.
    let large_local = temp.path().join("large_upload.bin");
    write_bytes(&large_local, 11 * 1024 * 1024);
    let input: StoragePreflightInputDto = serde_json::from_value(serde_json::json!({
        "kind": "library",
        "estimatedBytes": 0,
        "paths": [large_local],
    }))
    .unwrap();
    let path_preflight = service.preflight_write_input(input).await.unwrap();
    assert_eq!(path_preflight.estimated_bytes, 11 * 1024 * 1024);
    assert!(path_preflight.requires_confirm);
    assert_eq!(
        path_preflight.reason,
        Some(StoragePreflightReason::BelowDiskFloor)
    );
}

#[tokio::test]
async fn storage_budget_operations_are_not_registered_for_agents() {
    let temp = tempdir().unwrap();
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-ops"), temp.path())
        .build()
        .await
        .unwrap();

    let ops = host
        .operation_service()
        .list(&ProjectId::new("default"))
        .await;
    assert!(
        !ops.iter()
            .any(|op| op.spec.name.contains("storage") || op.spec.name.contains("budget")),
        "storage budget must never be exposed in OperationService"
    );
}

#[tokio::test]
async fn library_remove_undo_redo_and_purge_update_library_removed_bytes_without_calibrate() {
    let temp = tempdir().unwrap();
    let workspace_root = temp.path().join("data");
    let host = WorkspaceHostBuilder::new(WorkspaceScope::new("test-lib-removed"), &workspace_root)
        .build()
        .await
        .unwrap();

    host.storage_budget().calibrate().await.unwrap();

    let src = temp.path().join("asset.bin");
    write_bytes(&src, 12_345);
    let ctx = human_ctx();
    let added = host.library_service().add(vec![src], &ctx).await.unwrap();
    let id = added.items[0].id.clone();

    let s1 = host.storage_budget().status().await.unwrap();
    assert_eq!(s1.categories.library_removed_bytes, 0);
    assert!(s1.categories.library_bytes >= 12_345);

    // Remove -> library_removed_bytes updates immediately without calibrate().
    host.library_service()
        .remove(vec![id.clone()], &ctx)
        .await
        .unwrap();
    let s2 = host.storage_budget().status().await.unwrap();
    assert_eq!(s2.categories.library_removed_bytes, 12_345);

    // Undo -> back to 0 without calibrate().
    host.library_service().undo().await.unwrap();
    let s3 = host.storage_budget().status().await.unwrap();
    assert_eq!(s3.categories.library_removed_bytes, 0);

    // Redo -> back to 12_345 without calibrate().
    host.library_service().redo().await.unwrap();
    let s4 = host.storage_budget().status().await.unwrap();
    assert_eq!(s4.categories.library_removed_bytes, 12_345);

    // Manual purge -> library_removed_bytes clears to 0 and library_bytes decreases without calibrate().
    host.library_service().purge(&ctx).await.unwrap();
    let s5 = host.storage_budget().status().await.unwrap();
    assert_eq!(s5.categories.library_removed_bytes, 0);
    assert!(s5.categories.library_bytes < s4.categories.library_bytes);
}
