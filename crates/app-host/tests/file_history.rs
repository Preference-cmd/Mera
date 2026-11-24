use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mera_app_host::{
    AppHostResult, DiskSpaceInfo, DiskSpaceProvider, FileEntryKind, ScanOptions, ScanPhase,
    SnapshotSource, TrashMover, WorkspaceHost, WorkspaceHostBuilder, WorkspaceScope,
};
use mera_core::command::{CommandActor, CommandActorKind};
use mera_core::event::Timestamp;
use mera_core::model::ProjectId;
use mera_core::project::ProjectMetadata;
use mera_core::task::CancelToken;
use tempfile::tempdir;

#[derive(Debug)]
struct PlentyDiskSpace;

impl DiskSpaceProvider for PlentyDiskSpace {
    fn disk_space(&self, _path: &Path) -> AppHostResult<DiskSpaceInfo> {
        Ok(DiskSpaceInfo {
            available_bytes: 100 * 1024 * 1024 * 1024,
            total_bytes: 200 * 1024 * 1024 * 1024,
        })
    }
}

async fn build_host(scope: &str, data_root: &Path) -> WorkspaceHost {
    WorkspaceHostBuilder::new(WorkspaceScope::new(scope), data_root)
        .disk_space_provider(Arc::new(PlentyDiskSpace))
        .build()
        .await
        .unwrap()
}

#[derive(Debug, Default)]
struct RecordingTrashMover {
    trashed: Mutex<Vec<PathBuf>>,
}

impl RecordingTrashMover {
    fn take(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.trashed.lock().unwrap())
    }
}

impl TrashMover for RecordingTrashMover {
    fn move_to_trash(&self, path: &Path) -> Result<(), String> {
        self.trashed.lock().unwrap().push(path.to_path_buf());
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

fn collect_relative_tree(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let ft = entry.file_type().unwrap();
            out.insert(rel.clone());
            if !ft.is_symlink() && ft.is_dir() {
                stack.push((entry.path(), rel));
            }
        }
    }
    out
}

fn human_actor() -> CommandActor {
    CommandActor::new(CommandActorKind::Human).with_id("tester")
}

#[tokio::test]
async fn incremental_scan_10000_files_hashes_only_changed_file() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace-10k");
    std::fs::create_dir_all(&workspace_dir).unwrap();

    for i in 0..10_000u32 {
        let sub = workspace_dir.join(format!("dir-{:02}", i / 200));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("file-{i:05}.txt")), format!("content-{i}")).unwrap();
    }

    let host = build_host("fh-10k", &data_root).await;
    let project_id = ProjectId::new("default");
    host.project_service()
        .add_folder(&project_id, &workspace_dir)
        .await
        .unwrap();

    let scan1 = host
        .file_history()
        .scan_project(&project_id, ScanOptions::for_source(SnapshotSource::Manual))
        .await
        .unwrap();
    assert_eq!(scan1.visited_files, 10_000);
    assert_eq!(scan1.hashed_files, 10_000);
    assert_eq!(scan1.recorded_snapshots, 10_000);

    // Modify 1 file and verify scan #2 hashes only that 1 file.
    let modified = workspace_dir.join("dir-00/file-00042.txt");
    std::fs::write(&modified, b"updated-content-42-longer").unwrap();

    let scan2 = host
        .file_history()
        .scan_project(&project_id, ScanOptions::for_source(SnapshotSource::Manual))
        .await
        .unwrap();
    assert_eq!(scan2.visited_files, 10_000);
    assert_eq!(scan2.hashed_files, 1);
    assert_eq!(scan2.recorded_snapshots, 1);

    // Subsequent TurnStart scan records lightweight TurnStart metadata entries with 0 extra hashing.
    let turn_start = host
        .file_history()
        .scan_project(
            &project_id,
            ScanOptions::for_source(SnapshotSource::TurnStart),
        )
        .await
        .unwrap();
    assert_eq!(turn_start.visited_files, 10_000);
    assert_eq!(turn_start.hashed_files, 0);
    assert_eq!(turn_start.recorded_snapshots, 10_000);

    // Immediate repeated TurnStart scan with no changes skips duplicate TurnStart records.
    let turn_start_again = host
        .file_history()
        .scan_project(
            &project_id,
            ScanOptions::for_source(SnapshotSource::TurnStart),
        )
        .await
        .unwrap();
    assert_eq!(turn_start_again.hashed_files, 0);
    assert_eq!(turn_start_again.recorded_snapshots, 0);
}

#[tokio::test]
async fn skip_rules_ignore_txt_oversized_and_symlinks() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace-skips");
    let external_dir = temp.path().join("external-secret");
    std::fs::create_dir_all(&workspace_dir).unwrap();
    std::fs::create_dir_all(&external_dir).unwrap();
    std::fs::write(external_dir.join("secret.txt"), b"outside").unwrap();

    // Workspace .gitignore MUST NOT be honored; only fixed rules and project ignore.txt apply.
    std::fs::write(workspace_dir.join(".gitignore"), "kept_by_h3.txt\n").unwrap();
    std::fs::write(workspace_dir.join("kept_by_h3.txt"), b"must be tracked").unwrap();

    // Fixed skips: .git/, node_modules/, ~$*, .DS_Store, *.tmp.
    std::fs::create_dir_all(workspace_dir.join(".git/objects")).unwrap();
    std::fs::write(workspace_dir.join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
    std::fs::create_dir_all(workspace_dir.join("node_modules/pkg")).unwrap();
    std::fs::write(
        workspace_dir.join("node_modules/pkg/index.js"),
        b"export {}",
    )
    .unwrap();
    std::fs::write(workspace_dir.join("~$report.docx"), b"lock").unwrap();
    std::fs::write(workspace_dir.join(".DS_Store"), b"finder").unwrap();
    std::fs::write(workspace_dir.join("draft.tmp"), b"temp").unwrap();

    // Ignored via per-project ignore.txt.
    std::fs::create_dir_all(workspace_dir.join("build")).unwrap();
    std::fs::write(workspace_dir.join("build/artifact.bin"), b"ignored-dir").unwrap();
    std::fs::write(workspace_dir.join("notes.log"), b"ignored-glob").unwrap();

    // Oversized file (> 64 bytes configured for this test).
    std::fs::write(workspace_dir.join("huge.bin"), vec![0x7Au8; 128]).unwrap();

    // Normal tracked file.
    std::fs::write(workspace_dir.join("doc.md"), b"# Hello").unwrap();

    // Symlink pointing outside workspace (must never be followed).
    #[cfg(unix)]
    std::os::unix::fs::symlink(&external_dir, workspace_dir.join("ext-link")).unwrap();

    let host = build_host("fh-skips", &data_root).await;
    host.file_history().set_max_file_bytes(64);
    let project_id = ProjectId::new("default");
    host.project_service()
        .add_folder(&project_id, &workspace_dir)
        .await
        .unwrap();

    host.file_history()
        .set_ignore_rules(&project_id, "build/\n*.log\n")
        .await
        .unwrap();
    assert_eq!(
        host.file_history()
            .get_ignore_rules(&project_id)
            .await
            .unwrap(),
        "build/\n*.log\n"
    );

    let summary = host
        .file_history()
        .scan_project(&project_id, ScanOptions::for_source(SnapshotSource::Manual))
        .await
        .unwrap();
    assert_eq!(summary.skipped_oversized, 1);

    let history = host
        .file_history()
        .list_history(&project_id, None, None)
        .await
        .unwrap();
    let paths: BTreeSet<&str> = history.iter().map(|r| r.rel_path.as_str()).collect();

    assert!(
        paths.contains("kept_by_h3.txt"),
        "workspace .gitignore must not be read"
    );
    assert!(paths.contains("doc.md"));
    assert!(paths.contains("huge.bin"));
    assert!(!paths.iter().any(|p| p.contains(".git/") || *p == ".git"));
    assert!(!paths.iter().any(|p| p.contains("node_modules")));
    assert!(!paths.contains("~$report.docx"));
    assert!(!paths.contains(".DS_Store"));
    assert!(!paths.contains("draft.tmp"));
    assert!(!paths.iter().any(|p| p.starts_with("build/")));
    assert!(!paths.contains("notes.log"));
    assert!(!paths.iter().any(|p| p.contains("secret.txt")));

    let huge_rec = history.iter().find(|r| r.rel_path == "huge.bin").unwrap();
    assert!(huge_rec.oversized);
    assert_eq!(huge_rec.hash, None);

    let err = host
        .file_history()
        .restore_file(&project_id, &huge_rec.id, Some(human_actor()))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("exceeded the maximum size limit")
            || format!("{err:?}").contains("OVERSIZED_NOT_RESTORABLE"),
        "expected OVERSIZED_NOT_RESTORABLE diagnostic, got: {err:?}"
    );

    #[cfg(unix)]
    {
        let link_rec = history.iter().find(|r| r.rel_path == "ext-link").unwrap();
        assert_eq!(link_rec.kind, FileEntryKind::Symlink);
        assert_eq!(link_rec.hash, None);
        assert_eq!(
            link_rec.symlink_target.as_deref(),
            Some(external_dir.to_str().unwrap())
        );
    }
}

#[tokio::test]
async fn restore_undo_trash_zero_workspace_pollution_and_symlink_escape_guard() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace-restore");
    let outside_dir = temp.path().join("outside");
    std::fs::create_dir_all(&workspace_dir).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();

    let file_path = workspace_dir.join("notes/report.bin");
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    let v1_bytes = b"version-1-\x00\x01\x02-binary";
    std::fs::write(&file_path, v1_bytes).unwrap();

    let trash_mock = Arc::new(RecordingTrashMover::default());
    let host = build_host("fh-restore", &data_root).await;
    host.file_history().set_trash_mover(trash_mock.clone());

    let project_id = ProjectId::new("default");
    let folder = host
        .project_service()
        .add_folder(&project_id, &workspace_dir)
        .await
        .unwrap();

    // Snapshot v1, then overwrite with v2, then create an extra file and scan TurnStart + TurnEnd.
    let snap_v1 = host
        .file_history()
        .snapshot_file(
            &project_id,
            folder.id(),
            "notes/report.bin",
            SnapshotSource::TurnStart,
            Some("turn-1".to_owned()),
            Some(human_actor()),
            None,
            false,
        )
        .await
        .unwrap()
        .expect("v1 recorded");

    let v2_bytes = b"version-2-mutated";
    std::fs::write(&file_path, v2_bytes).unwrap();
    std::fs::write(
        workspace_dir.join("new-created.txt"),
        b"created during turn",
    )
    .unwrap();
    host.file_history()
        .scan_project(
            &project_id,
            ScanOptions::for_source(SnapshotSource::TurnEnd),
        )
        .await
        .unwrap();

    // Restore v1 -> byte-for-byte identical, and undo via pre_restore_snapshot_id restores v2.
    let restore_res = host
        .file_history()
        .restore_file(&project_id, &snap_v1.id, Some(human_actor()))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&file_path).unwrap(), v1_bytes);
    assert!(!restore_res.moved_to_trash);

    let undo_res = host
        .file_history()
        .restore_file(
            &project_id,
            &restore_res.pre_restore_snapshot_id,
            Some(human_actor()),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&file_path).unwrap(), v2_bytes);
    assert!(!undo_res.moved_to_trash);

    // Delete `new-created.txt` -> scan records Deleted -> restore new-created.txt back ->
    // then restore the Deleted snapshot while file exists on disk -> moves to mock trash!
    let new_file = workspace_dir.join("new-created.txt");
    let new_file_snap = host
        .file_history()
        .list_history(&project_id, Some(folder.id()), Some("new-created.txt"))
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    std::fs::remove_file(&new_file).unwrap();
    host.file_history()
        .scan_project(&project_id, ScanOptions::for_source(SnapshotSource::Manual))
        .await
        .unwrap();

    let deleted_snap = host
        .file_history()
        .list_history(&project_id, Some(folder.id()), Some("new-created.txt"))
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.kind == FileEntryKind::Deleted)
        .expect("deleted record emitted");

    // Recreate file via restore, then restore Deleted snapshot -> TrashMover invoked.
    host.file_history()
        .restore_file(&project_id, &new_file_snap.id, Some(human_actor()))
        .await
        .unwrap();
    assert!(new_file.is_file());

    let del_restore = host
        .file_history()
        .restore_file(&project_id, &deleted_snap.id, Some(human_actor()))
        .await
        .unwrap();
    assert!(del_restore.moved_to_trash);
    assert!(!new_file.exists());
    assert_eq!(
        trash_mock.take(),
        vec![
            std::fs::canonicalize(&workspace_dir)
                .unwrap()
                .join("new-created.txt")
        ]
    );

    // Zero workspace pollution: only `notes` and `notes/report.bin` exist in workspace_dir.
    let tree = collect_relative_tree(&workspace_dir);
    assert_eq!(
        tree,
        BTreeSet::from(["notes".to_owned(), "notes/report.bin".to_owned()])
    );

    // Symlink escape guard: replacing `notes` with a symlink pointing outside must fail restore.
    #[cfg(unix)]
    {
        let escape_target = workspace_dir.join("escape_sub/pwn.txt");
        let escape_snap = {
            std::fs::create_dir_all(escape_target.parent().unwrap()).unwrap();
            std::fs::write(&escape_target, b"safe").unwrap();
            host.file_history()
                .snapshot_file(
                    &project_id,
                    folder.id(),
                    "escape_sub/pwn.txt",
                    SnapshotSource::Manual,
                    None,
                    None,
                    None,
                    false,
                )
                .await
                .unwrap()
                .unwrap()
        };
        std::fs::remove_dir_all(workspace_dir.join("escape_sub")).unwrap();
        std::os::unix::fs::symlink(&outside_dir, workspace_dir.join("escape_sub")).unwrap();

        let err = host
            .file_history()
            .restore_file(&project_id, &escape_snap.id, Some(human_actor()))
            .await
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("SYMLINK_ESCAPE"),
            "expected SYMLINK_ESCAPE, got {err:?}"
        );
        assert!(!outside_dir.join("pwn.txt").exists());
    }
}

#[tokio::test]
async fn relink_folder_preserves_file_history_and_restores_to_moved_path() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let original_dir = temp.path().join("folder-old");
    let moved_dir = temp.path().join("folder-moved");
    std::fs::create_dir_all(&original_dir).unwrap();
    std::fs::write(original_dir.join("story.md"), b"chapter 1 original").unwrap();

    let host = build_host("fh-relink", &data_root).await;
    let project_id = ProjectId::new("default");
    let folder = host
        .project_service()
        .add_folder(&project_id, &original_dir)
        .await
        .unwrap();

    host.file_history()
        .scan_project(
            &project_id,
            ScanOptions::for_source(SnapshotSource::TurnStart),
        )
        .await
        .unwrap();
    let snaps = host
        .file_history()
        .list_history(&project_id, Some(folder.id()), Some("story.md"))
        .await
        .unwrap();
    assert_eq!(snaps.len(), 1);

    // Move workspace folder on disk and edit the file after moving.
    std::fs::rename(&original_dir, &moved_dir).unwrap();
    std::fs::write(moved_dir.join("story.md"), b"chapter 1 ruined").unwrap();

    let relinked = host
        .project_service()
        .relink_folder(&project_id, folder.id(), &moved_dir)
        .await
        .unwrap();
    assert_eq!(relinked.id(), folder.id());

    // Restore original snapshot -> writes into moved_dir.
    host.file_history()
        .restore_file(&project_id, &snaps[0].id, Some(human_actor()))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(moved_dir.join("story.md")).unwrap(),
        b"chapter 1 original"
    );
}

#[tokio::test]
async fn operation_service_exposes_no_file_history_or_restore_operations() {
    let temp = tempdir().unwrap();
    let host = build_host("fh-ops", temp.path()).await;

    let ops = host
        .operation_service()
        .list(&ProjectId::new("default"))
        .await;
    for op in &ops {
        let name = &op.spec.name;
        assert!(
            !name.contains("file_history")
                && !name.contains("history.")
                && !name.contains("restore")
                && !name.contains("storage"),
            "forbidden operation exposed to agents: {name}"
        );
    }
}

#[tokio::test]
async fn storage_budget_reclaim_protects_latest_turn_start_and_pinned_and_shared_blobs() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let folder_a = temp.path().join("ws-a");
    let folder_b = temp.path().join("ws-b");
    std::fs::create_dir_all(&folder_a).unwrap();
    std::fs::create_dir_all(&folder_b).unwrap();

    let host = build_host("fh-reclaim", &data_root).await;

    let proj_a = ProjectId::new("default");
    let proj_b = ProjectId::new("proj-b");
    host.project_service()
        .create_project(
            proj_b.clone(),
            ProjectMetadata::new(
                "Project B",
                "second",
                Timestamp::new("2026-11-24T00:00:00Z"),
                Timestamp::new("2026-11-24T00:00:00Z"),
            ),
        )
        .await
        .unwrap();

    let fa = host
        .project_service()
        .add_folder(&proj_a, &folder_a)
        .await
        .unwrap();
    let fb = host
        .project_service()
        .add_folder(&proj_b, &folder_b)
        .await
        .unwrap();

    // File 1 in proj_a:
    // - TurnStart v1 (superseded by TurnStart v2 -> unprotected)
    // - Manual v_shared (same bytes as TurnStart v2 -> shares blob!)
    // - Manual v_pinned (pinned -> protected!)
    // - TurnStart v2 (latest TurnStart -> protected!)
    // - TurnEnd v3 (different bytes -> unprotected!)
    let f1 = folder_a.join("f1.bin");
    let b_v1 = vec![0x11u8; 2048];
    let b_v2 = vec![0x22u8; 4096];
    let b_pin = vec![0x33u8; 3072];
    let b_v3 = vec![0x44u8; 5120];

    std::fs::write(&f1, &b_v1).unwrap();
    let s_v1_old = host
        .file_history()
        .snapshot_file(
            &proj_a,
            fa.id(),
            "f1.bin",
            SnapshotSource::TurnStart,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();

    std::fs::write(&f1, &b_pin).unwrap();
    let s_pinned = host
        .file_history()
        .snapshot_file(
            &proj_a,
            fa.id(),
            "f1.bin",
            SnapshotSource::Manual,
            None,
            None,
            Some("keep".into()),
            false,
        )
        .await
        .unwrap()
        .unwrap();
    host.file_history()
        .set_pinned(&proj_a, &s_pinned.id, true)
        .await
        .unwrap();

    std::fs::write(&f1, &b_v2).unwrap();
    let s_shared_unprotected = host
        .file_history()
        .snapshot_file(
            &proj_a,
            fa.id(),
            "f1.bin",
            SnapshotSource::Manual,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();
    let s_v2_turn_start = host
        .file_history()
        .snapshot_file(
            &proj_a,
            fa.id(),
            "f1.bin",
            SnapshotSource::TurnStart,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s_shared_unprotected.hash, s_v2_turn_start.hash);

    std::fs::write(&f1, &b_v3).unwrap();
    let s_v3_end = host
        .file_history()
        .snapshot_file(
            &proj_a,
            fa.id(),
            "f1.bin",
            SnapshotSource::TurnEnd,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();

    // Project B also has an unprotected snapshot so cross-project clean_project_history can be tested.
    std::fs::write(folder_b.join("b.bin"), vec![0x55u8; 4096]).unwrap();
    let s_b_manual = host
        .file_history()
        .snapshot_file(
            &proj_b,
            fb.id(),
            "b.bin",
            SnapshotSource::Manual,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();

    host.storage_budget().calibrate().await.unwrap();

    // Trigger Priority 3 automatic reclaim via tiny positive budget Some(1).
    let status = host.storage_budget().set_budget(Some(1)).await.unwrap();
    assert!(status.over_budget);

    // Protected records (pinned + latest TurnStart) survived; unprotected records were removed!
    let list_a = host
        .file_history()
        .list_history(&proj_a, None, None)
        .await
        .unwrap();
    let ids_a: BTreeSet<_> = list_a.iter().map(|r| r.id.as_str()).collect();
    assert!(ids_a.contains(s_pinned.id.as_str()));
    assert!(ids_a.contains(s_v2_turn_start.id.as_str()));
    assert!(!ids_a.contains(s_v1_old.id.as_str()));
    assert!(!ids_a.contains(s_shared_unprotected.id.as_str()));
    assert!(!ids_a.contains(s_v3_end.id.as_str()));

    let list_b = host
        .file_history()
        .list_history(&proj_b, None, None)
        .await
        .unwrap();
    assert!(!list_b.iter().any(|r| r.id == s_b_manual.id));

    // Shared blob referenced by surviving TurnStart v2 MUST STILL BE READABLE!
    let blob_dir_a = data_root.join("projects/default/history/blobs");
    let v2_hash = s_v2_turn_start.hash.as_ref().unwrap();
    let pin_hash = s_pinned.hash.as_ref().unwrap();
    let v1_hash = s_v1_old.hash.as_ref().unwrap();
    let v3_hash = s_v3_end.hash.as_ref().unwrap();
    assert!(blob_dir_a.join(&v2_hash[..2]).join(v2_hash).is_file());
    assert!(blob_dir_a.join(&pin_hash[..2]).join(pin_hash).is_file());
    assert!(!blob_dir_a.join(&v1_hash[..2]).join(v1_hash).exists());
    assert!(!blob_dir_a.join(&v3_hash[..2]).join(v3_hash).exists());

    // Over-budget write suppression: while over budget (limit = Some(1)),
    // unprotected TurnEnd/Manual scans skip new changes, while TurnStart & pinned still save!
    std::fs::write(&f1, b"over-budget-edit").unwrap();
    let suppressed = host
        .file_history()
        .scan_project(&proj_a, ScanOptions::for_source(SnapshotSource::TurnEnd))
        .await
        .unwrap();
    assert!(suppressed.skipped_over_budget > 0);
    assert_eq!(suppressed.recorded_snapshots, 0);

    // Protected pinned manual snapshot saves even while over budget.
    let saved_pinned = host
        .file_history()
        .snapshot_file(
            &proj_a,
            fa.id(),
            "f1.bin",
            SnapshotSource::Manual,
            None,
            None,
            None,
            true,
        )
        .await
        .unwrap();
    assert!(saved_pinned.is_some());

    // Protected TurnStart scan saves even while over budget.
    std::fs::write(&f1, b"over-budget-turn-start").unwrap();
    let turn_start_over = host
        .file_history()
        .scan_project(&proj_a, ScanOptions::for_source(SnapshotSource::TurnStart))
        .await
        .unwrap();
    assert_eq!(turn_start_over.skipped_over_budget, 0);
    assert_eq!(turn_start_over.recorded_snapshots, 1);

    // Clear budget, add unprotected snapshots to both projects, and test clean_project_history(proj_a).
    host.storage_budget().set_budget(None).await.unwrap();
    std::fs::write(&f1, vec![0x66u8; 4096]).unwrap();
    host.file_history()
        .scan_project(&proj_a, ScanOptions::for_source(SnapshotSource::Manual))
        .await
        .unwrap();
    std::fs::write(folder_b.join("b2.bin"), vec![0x77u8; 4096]).unwrap();
    let b2_snap = host
        .file_history()
        .snapshot_file(
            &proj_b,
            fb.id(),
            "b2.bin",
            SnapshotSource::Manual,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();

    let clean_res = host.clean_project_history(&proj_a).await.unwrap();
    assert!(clean_res.freed_bytes >= 4096);
    // Project B unprotected snapshot was NOT touched by clean_project_history(proj_a).
    let list_b_after = host
        .file_history()
        .list_history(&proj_b, None, None)
        .await
        .unwrap();
    assert!(list_b_after.iter().any(|r| r.id == b2_snap.id));
}

#[tokio::test]
async fn background_scan_transitions_to_ready_and_cancel_avoids_bogus_deletes() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace-bg");
    std::fs::create_dir_all(&workspace_dir).unwrap();
    for i in 0..40 {
        std::fs::write(
            workspace_dir.join(format!("item-{i}.txt")),
            format!("data-{i}"),
        )
        .unwrap();
    }

    let host = build_host("fh-bg", &data_root).await;
    let project_id = ProjectId::new("default");
    host.project_service()
        .add_folder(&project_id, &workspace_dir)
        .await
        .unwrap();

    let initial = host
        .file_history()
        .start_background_scan(&project_id, ScanOptions::for_source(SnapshotSource::Manual))
        .await
        .unwrap();
    assert!(matches!(
        initial.phase,
        ScanPhase::Scanning | ScanPhase::Ready
    ));

    // Poll until Ready.
    let mut final_status = host.file_history().scan_status(&project_id);
    for _ in 0..100 {
        if final_status.phase == ScanPhase::Ready {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        final_status = host.file_history().scan_status(&project_id);
    }
    assert_eq!(final_status.phase, ScanPhase::Ready);
    assert!(final_status.ready);
    assert_eq!(final_status.visited_files, 40);

    // Pre-cancelled scan must cancel cleanly without emitting Deleted records for existing tracked files.
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut opts = ScanOptions::for_source(SnapshotSource::Manual);
    opts.cancel = cancel;
    let cancelled_summary = host
        .file_history()
        .scan_project(&project_id, opts)
        .await
        .unwrap();
    assert!(cancelled_summary.cancelled);
    assert_eq!(cancelled_summary.deleted_files, 0);
    assert_eq!(
        host.file_history().scan_status(&project_id).phase,
        ScanPhase::Cancelled
    );
    let history = host
        .file_history()
        .list_history(&project_id, None, None)
        .await
        .unwrap();
    assert!(!history.iter().any(|r| r.kind == FileEntryKind::Deleted));
}
