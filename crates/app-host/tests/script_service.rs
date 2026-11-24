use std::path::{Path, PathBuf};
use std::sync::Arc;

use mera_app_host::script_env::ScriptEnvSpec;
use mera_app_host::{
    InvokeRequest, ModStorePaths, ResolvedPythonInterpreter, ScriptPythonResolver,
    ScriptServiceOptions, WorkspaceHost, WorkspaceHostBuilder, WorkspaceScope,
};
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::model::ProjectId;
use mera_core::task::CancelToken;
use serde_json::json;
use tempfile::tempdir;

fn human_ctx() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Human),
        CommandProvenance::Direct,
    )
}

fn agent_ctx() -> OperationContext {
    OperationContext::new(
        CommandActor::new(CommandActorKind::Agent).with_id("agent-1"),
        CommandProvenance::Direct,
    )
}

fn find_host_python3() -> Option<(PathBuf, Vec<PathBuf>)> {
    let out = std::process::Command::new("python3")
        .args([
            "-c",
            "import sys, os; print(sys.executable); print(os.path.realpath(sys.executable)); print(sys.prefix); print(sys.base_prefix)",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let exe = PathBuf::from(lines.next()?);
    let real_exe = PathBuf::from(lines.next()?);
    let prefix = PathBuf::from(lines.next()?);
    let base_prefix = PathBuf::from(lines.next()?);
    if !exe.is_file() {
        return None;
    }

    let mut ro = Vec::new();
    for p in [&exe, &real_exe] {
        if let Some(parent) = p.parent() {
            ro.push(parent.to_path_buf());
        }
    }
    ro.push(prefix);
    ro.push(base_prefix);
    if exe.starts_with("/opt/homebrew") || real_exe.starts_with("/opt/homebrew") {
        ro.push(PathBuf::from("/opt/homebrew"));
    }
    if exe.starts_with("/Library/Developer/CommandLineTools")
        || real_exe.starts_with("/Library/Developer/CommandLineTools")
    {
        ro.push(PathBuf::from("/Library/Developer/CommandLineTools"));
    }
    Some((exe, ro))
}

struct HostPythonTestResolver {
    executable: PathBuf,
    read_only_paths: Vec<PathBuf>,
}

impl ScriptPythonResolver for HostPythonTestResolver {
    fn resolve(&self, spec: &ScriptEnvSpec) -> Option<ResolvedPythonInterpreter> {
        if !spec.dependencies.is_empty() {
            return None;
        }
        Some(ResolvedPythonInterpreter {
            executable: self.executable.clone(),
            python_version: spec.python.clone(),
            read_only_paths: self.read_only_paths.clone(),
            env_hash: spec.env_hash(),
            venv_dir: None,
            lock_sha256: None,
        })
    }
}

async fn build_host_with_python(
    base: &Path,
    app_data: &Path,
    options: ScriptServiceOptions,
) -> WorkspaceHost {
    WorkspaceHostBuilder::new(WorkspaceScope::new("script-service-test"), base)
        .app_data_root(app_data)
        .script_service_options(options)
        .build()
        .await
        .expect("build WorkspaceHost")
}

fn extract_json_suffix(message: &str) -> serde_json::Value {
    let idx = message.find('{').expect("diagnostic contains json payload");
    serde_json::from_str(&message[idx..]).expect("valid json payload in diagnostic")
}

#[tokio::test]
async fn script_python_approval_batch_reuse_tamper_recheck_and_asset_roundtrip() {
    let Some((py_exe, py_ro)) = find_host_python3() else {
        return;
    };
    let temp = tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let host = build_host_with_python(
        &base,
        &app_data,
        ScriptServiceOptions {
            python_resolver: Some(Arc::new(HostPythonTestResolver {
                executable: py_exe,
                read_only_paths: py_ro,
            })),
            ..Default::default()
        },
    )
    .await;

    let project = ProjectId::new("default");
    let src_a = temp.path().join("a.txt");
    let src_b = temp.path().join("b.txt");
    std::fs::write(&src_a, "alpha").unwrap();
    std::fs::write(&src_b, "beta").unwrap();
    let imported = host
        .library_service()
        .add(vec![src_a, src_b], &human_ctx())
        .await
        .unwrap();
    let import_res = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "asset.import".to_owned(),
                input: json!({
                    "library_items": [
                        imported.items[0].id.to_string(),
                        imported.items[1].id.to_string(),
                    ]
                }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .unwrap();
    let asset_a = import_res.output["assets"][0].as_str().unwrap().to_owned();
    let asset_b = import_res.output["assets"][1].as_str().unwrap().to_owned();

    let code = r#"
import pathlib

def main(ctx):
    text = pathlib.Path(ctx.assets["doc"]).read_text(encoding="utf-8")
    out_file = ctx.out_dir / "upper.txt"
    out_file.write_text(text.upper(), encoding="utf-8")
    ctx.emit_output("upper", out_file, media_type="text/plain")
    print("logged:" + text)
    return {"length": len(text), "tag": ctx.input.get("tag")}
"#;

    // 1. Unapproved invocation returns SCRIPT/APPROVAL_REQUIRED and never executes.
    let err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({
                    "code": code,
                    "input": { "tag": "batch-1" },
                    "assets": [{ "name": "doc", "asset": asset_a }],
                }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect_err("first run without approval must fail");
    assert_eq!(err.code().as_str(), "SCRIPT/APPROVAL_REQUIRED");
    let info = extract_json_suffix(err.message());
    let digest = info["exec_digest"].as_str().unwrap().to_owned();
    assert_eq!(info["offline_sandbox"], json!(true));
    let review = host
        .script_service()
        .pending_execution_review(&project, &digest)
        .expect("pending review stored");
    assert_eq!(review.code, code);

    // 2. Approve once -> batch invocations over N assets succeed without re-prompting.
    host.script_service()
        .approve_execution(&project, &digest)
        .await
        .unwrap();

    for (asset_id, expected_upper, expected_len) in [(asset_a, "ALPHA", 5), (asset_b, "BETA", 4)] {
        let res = host
            .operation_service()
            .invoke(
                &project,
                InvokeRequest {
                    name: "script.python".to_owned(),
                    input: json!({
                        "code": code,
                        "input": { "tag": "batch-1" },
                        "assets": [{ "name": "doc", "asset": asset_id }],
                    }),
                    context: agent_ctx(),
                    cancel: CancelToken::new(),
                    run: None,
                    input_sources: Default::default(),
                },
            )
            .await
            .expect("approved script executes inside OS sandbox");

        assert_eq!(
            res.output["result"],
            json!({ "length": expected_len, "tag": "batch-1" })
        );
        let out_asset_id = res.output["outputs"][0]["asset"].as_str().unwrap();
        assert_ne!(
            out_asset_id, "_mera/out/upper.txt",
            "OperationService must ingest output into AssetStore"
        );
        let asset_store = mera_app_host::AssetStore::new(base.join("projects/default"));
        let rec = asset_store
            .get_asset(&mera_core::model::AssetId::new(out_asset_id))
            .await
            .unwrap()
            .expect("ingested asset exists");
        let stored_text = std::fs::read_to_string(asset_store.content_path(&rec.hash)).unwrap();
        assert_eq!(stored_text, expected_upper);
    }

    // 3. Modifying 1 byte of code triggers a new digest and blocks again.
    let modified_code = format!("{code} ");
    let err_tamper = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({
                    "code": modified_code,
                    "input": { "tag": "batch-1" },
                }),
                context: agent_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect_err("1-byte code change must require new approval");
    assert_eq!(err_tamper.code().as_str(), "SCRIPT/APPROVAL_REQUIRED");
}

#[tokio::test]
async fn script_python_enforces_sandbox_isolation_and_64k_stdio_truncation() {
    let Some((py_exe, py_ro)) = find_host_python3() else {
        return;
    };
    let temp = tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let forbidden_path = temp.path().join("forbidden-write.txt");

    let host = build_host_with_python(
        &base,
        &app_data,
        ScriptServiceOptions {
            python_resolver: Some(Arc::new(HostPythonTestResolver {
                executable: py_exe,
                read_only_paths: py_ro,
            })),
            ..Default::default()
        },
    )
    .await;
    let project = ProjectId::new("default");
    let probe_src = temp.path().join("probe.txt");
    std::fs::write(&probe_src, "immutable").unwrap();
    let probe_lib = host
        .library_service()
        .add(vec![probe_src], &human_ctx())
        .await
        .unwrap();
    let probe_import = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "asset.import".to_owned(),
                input: json!({
                    "library_items": [probe_lib.items[0].id.to_string()]
                }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .unwrap();
    let probe_asset = probe_import.output["assets"][0]
        .as_str()
        .unwrap()
        .to_owned();

    let forbidden_json = serde_json::to_string(forbidden_path.to_string_lossy().as_ref()).unwrap();
    let code = format!(
        r#"
import os, socket, sys

def main(ctx):
    net_blocked = False
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(0.5)
        s.connect(("127.0.0.1", 9))
        s.close()
    except OSError:
        net_blocked = True

    # Input assets are mounted read-only on all OS backends, so mutating them
    # must fail with OSError.
    write_blocked = False
    try:
        with open(ctx.assets["probe"], "w") as fh:
            fh.write("tampered")
    except OSError:
        write_blocked = True

    # Attempting to write outside scratch must never reach the host filesystem
    # (on macOS Seatbelt this raises OSError; on Linux bwrap, tempdirs under
    # /tmp are shadowed by a private ephemeral tmpfs that vanishes on exit).
    try:
        with open({forbidden_json}, "w") as fh:
            fh.write("escaped")
    except OSError:
        pass

    sys.stdout.write("A" * 80000)
    sys.stderr.write("E" * 80000)
    return {{
        "net_blocked": net_blocked,
        "write_blocked": write_blocked,
        "bytecode_disabled": sys.dont_write_bytecode,
        "safe_path": os.environ.get("PYTHONSAFEPATH"),
    }}
"#,
    );

    let err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({
                    "code": code,
                    "assets": [{ "name": "probe", "asset": probe_asset }],
                }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .unwrap_err();
    let digest = extract_json_suffix(err.message())["exec_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    host.script_service()
        .approve_execution(&project, &digest)
        .await
        .unwrap();

    let res = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({
                    "code": code,
                    "assets": [{ "name": "probe", "asset": probe_asset }],
                }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("sandboxed script succeeds");

    assert_eq!(res.output["result"]["net_blocked"], json!(true));
    assert_eq!(res.output["result"]["write_blocked"], json!(true));
    assert_eq!(res.output["result"]["bytecode_disabled"], json!(true));
    assert_eq!(res.output["result"]["safe_path"], json!("1"));
    assert_eq!(res.output["stdout_truncated"], json!(true));
    assert_eq!(res.output["stderr_truncated"], json!(true));
    assert_eq!(res.output["stdout"].as_str().unwrap().len(), 65_536);
    assert_eq!(res.output["stderr"].as_str().unwrap().len(), 65_536);
    assert!(!forbidden_path.exists());
}

#[tokio::test]
async fn script_python_kills_on_timeout_memory_limit_and_cancel() {
    let Some((py_exe, py_ro)) = find_host_python3() else {
        return;
    };
    let temp = tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let host = build_host_with_python(
        &base,
        &app_data,
        ScriptServiceOptions {
            python_resolver: Some(Arc::new(HostPythonTestResolver {
                executable: py_exe,
                read_only_paths: py_ro,
            })),
            ..Default::default()
        },
    )
    .await;
    let project = ProjectId::new("default");

    async fn approve_and_run(
        host: &WorkspaceHost,
        project: &ProjectId,
        input: serde_json::Value,
        cancel: CancelToken,
    ) -> mera_core::diagnostic::Diagnostic {
        let err = host
            .operation_service()
            .invoke(
                project,
                InvokeRequest {
                    name: "script.python".to_owned(),
                    input: input.clone(),
                    context: human_ctx(),
                    cancel: CancelToken::new(),
                    run: None,
                    input_sources: Default::default(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), "SCRIPT/APPROVAL_REQUIRED");
        let digest = extract_json_suffix(err.message())["exec_digest"]
            .as_str()
            .unwrap()
            .to_owned();
        host.script_service()
            .approve_execution(project, &digest)
            .await
            .unwrap();
        host.operation_service()
            .invoke(
                project,
                InvokeRequest {
                    name: "script.python".to_owned(),
                    input,
                    context: human_ctx(),
                    cancel,
                    run: None,
                    input_sources: Default::default(),
                },
            )
            .await
            .unwrap_err()
    }

    // 1. Timeout
    let timeout_err = approve_and_run(
        &host,
        &project,
        json!({
            "code": "import time\ntime.sleep(30)\n",
            "timeout_secs": 1,
        }),
        CancelToken::new(),
    )
    .await;
    assert_eq!(timeout_err.code().as_str(), "SCRIPT/TIMEOUT");

    // 2. Cancellation
    let cancel = CancelToken::new();
    let cancel_bg = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cancel_bg.cancel();
    });
    let cancel_err = approve_and_run(
        &host,
        &project,
        json!({
            "code": "import time\ntime.sleep(25)\n",
            "timeout_secs": 30,
        }),
        cancel,
    )
    .await;
    assert_eq!(cancel_err.code().as_str(), "SCRIPT/CANCELLED");

    // 3. Memory limit (on macOS `proc_pid_rusage` enforces physical memory).
    #[cfg(target_os = "macos")]
    {
        let mem_err = approve_and_run(
            &host,
            &project,
            json!({
                "code": "import time\nb = bytearray(96 * 1024 * 1024)\nfor i in range(0, len(b), 4096): b[i] = 1\ntime.sleep(1)\n",
                "memory_limit_bytes": 16_777_216u64,
                "timeout_secs": 10,
            }),
            CancelToken::new(),
        )
        .await;
        assert_eq!(mem_err.code().as_str(), "SCRIPT/MEMORY_LIMIT_EXCEEDED");
    }
}

#[tokio::test]
async fn script_python_enforces_online_prepare_hitl_before_uv_prepare() {
    let temp = tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let host = build_host_with_python(&base, &app_data, ScriptServiceOptions::default()).await;
    let project = ProjectId::new("default");

    let code = concat!(
        "# /// script\n",
        "# dependencies = [\"numpy>=2.0\"]\n",
        "# ///\n",
        "print('needs numpy')\n",
    );

    let err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({ "code": code }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect_err("uncached dependency script must require online prepare confirmation");
    assert_eq!(err.code().as_str(), "SCRIPT/ONLINE_PREPARE_REQUIRED");
    let payload = extract_json_suffix(err.message());
    let env_hash = payload["env_hash"].as_str().unwrap().to_owned();
    assert_eq!(payload["no_build"], json!(true));
    assert_eq!(payload["dependencies"], json!(["numpy>=2.0"]));

    let pending = host
        .script_service()
        .pending_online_prepare(&project, &env_hash)
        .expect("pending online prepare recorded");
    assert!(pending.no_build);

    // Approve online prepare and place a fake uv binary + stub python so prepare runs offline
    // and then transitions to SCRIPT/APPROVAL_REQUIRED.
    let mod_paths = ModStorePaths::new(&app_data);
    std::fs::create_dir_all(mod_paths.uv_dir()).unwrap();
    std::fs::create_dir_all(mod_paths.uv_python_dir()).unwrap();
    std::fs::write(mod_paths.uv_python_dir().join("cpython-3.12"), b"stub").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let uv_bin = mod_paths.uv_dir().join("uv");
        std::fs::write(
            &uv_bin,
            r#"#!/bin/sh
sub="$1"; shift
case "$sub" in
  lock)
    deps_file=""
    prev=""
    for a in "$@"; do
      if [ "$prev" = "--script" ]; then deps_file="$a"; fi
      prev="$a"
    done
    echo "version = 1" > "${deps_file}.lock"
    ;;
  venv)
    mkdir -p "$1/bin"
    printf '#!/bin/sh\nexit 0\n' > "$1/bin/python3"
    chmod 755 "$1/bin/python3"
    ;;
  export)
    out=""
    prev=""
    for a in "$@"; do
      if [ "$prev" = "--output-file" ]; then out="$a"; fi
      prev="$a"
    done
    echo "numpy==2.1.0 --hash=sha256:abcd" > "$out"
    ;;
  pip) exit 0 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&uv_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    host.script_service()
        .approve_online_prepare(&project, &env_hash)
        .unwrap();
    #[cfg(unix)]
    {
        let err_after = host
            .operation_service()
            .invoke(
                &project,
                InvokeRequest {
                    name: "script.python".to_owned(),
                    input: json!({ "code": code }),
                    context: human_ctx(),
                    cancel: CancelToken::new(),
                    run: None,
                    input_sources: Default::default(),
                },
            )
            .await
            .expect_err("after online prepare, code approval is still enforced");
        assert_eq!(err_after.code().as_str(), "SCRIPT/APPROVAL_REQUIRED");
    }
}

#[tokio::test]
async fn script_python_supports_env_protocol_without_main_and_rejects_output_escape() {
    let Some((py_exe, py_ro)) = find_host_python3() else {
        return;
    };
    let temp = tempdir().unwrap();
    let base = temp.path().join("workspace");
    let app_data = temp.path().join("app-data");
    let host = build_host_with_python(
        &base,
        &app_data,
        ScriptServiceOptions {
            python_resolver: Some(Arc::new(HostPythonTestResolver {
                executable: py_exe,
                read_only_paths: py_ro,
            })),
            ..Default::default()
        },
    )
    .await;
    let project = ProjectId::new("default");

    let env_code = r#"
import json, os, pathlib

out_dir = pathlib.Path(os.environ["MERA_OUT_DIR"])
out_file = out_dir / "env_mode.txt"
out_file.write_text("env-ok", encoding="utf-8")
with open(os.environ["MERA_RESULT_JSON"], "w", encoding="utf-8") as fh:
    json.dump({"result": {"mode": "env"}, "outputs": [{"name": "out", "path": "env_mode.txt"}]}, fh)
"#;
    let err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({ "code": env_code }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .unwrap_err();
    let d1 = extract_json_suffix(err.message())["exec_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    host.script_service()
        .approve_execution(&project, &d1)
        .await
        .unwrap();
    let ok = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({ "code": env_code }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect("env-only script succeeds");
    assert_eq!(ok.output["result"], json!({ "mode": "env" }));
    assert_eq!(ok.output["outputs"].as_array().unwrap().len(), 1);

    let escape_code = r#"
import json, os

with open(os.environ["MERA_RESULT_JSON"], "w", encoding="utf-8") as fh:
    json.dump({"result": None, "outputs": [{"name": "bad", "path": "../input.json"}]}, fh)
"#;
    let err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({ "code": escape_code }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .unwrap_err();
    let d2 = extract_json_suffix(err.message())["exec_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    host.script_service()
        .approve_execution(&project, &d2)
        .await
        .unwrap();
    let escape_err = host
        .operation_service()
        .invoke(
            &project,
            InvokeRequest {
                name: "script.python".to_owned(),
                input: json!({ "code": escape_code }),
                context: human_ctx(),
                cancel: CancelToken::new(),
                run: None,
                input_sources: Default::default(),
            },
        )
        .await
        .expect_err("path escaping _mera/out must be rejected");
    assert_eq!(escape_err.code().as_str(), "SCRIPT/ESCAPE_OUTPUT_DIR");
}
