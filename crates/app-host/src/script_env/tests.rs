use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

use tempfile::tempdir;

use super::{
    SCRIPT_ENV_METADATA_FILE, ScriptEnvError, ScriptEnvLookup, ScriptEnvSpec,
    build_script_python_env, ensure_script_scratch_dirs, format_pep723_stub, inspect_script_env,
    parse_pep723_metadata, prepare_script_env,
};
use crate::mod_manager::inventory::ModStorePaths;

#[test]
fn pep723_parses_valid_block_and_ignores_other_types() {
    let code = concat!(
        "# /// tool\r\n",
        "# name = \"ignored\"\r\n",
        "# ///\r\n",
        "# /// script\r\n",
        "# requires-python = \">=3.12,<3.13\"\r\n",
        "#\r\n",
        "# dependencies = [\r\n",
        "#   \"numpy>=2.0\",\r\n",
        "#   \"pillow>=11.0\",\r\n",
        "# ]\r\n",
        "# ///\r\n",
        "print('hello')\r\n",
    );
    let parsed = parse_pep723_metadata(code)
        .expect("parse valid PEP 723")
        .expect("script block present");
    assert_eq!(parsed.requires_python.as_deref(), Some(">=3.12,<3.13"));
    assert_eq!(parsed.dependencies, vec!["numpy>=2.0", "pillow>=11.0"]);
}

#[test]
fn pep723_rejects_multiple_or_unclosed_blocks() {
    let multi = concat!(
        "# /// script\n",
        "# dependencies = [\"numpy\"]\n",
        "# ///\n",
        "# /// script\n",
        "# dependencies = [\"pillow\"]\n",
        "# ///\n",
    );
    let err = parse_pep723_metadata(multi).expect_err("must reject multiple script blocks");
    assert!(matches!(err, ScriptEnvError::InvalidPep723 { .. }));

    let unclosed = "# /// script\n# dependencies = [\"numpy\"]\nprint('x')\n";
    let err = parse_pep723_metadata(unclosed).expect_err("must reject unclosed block");
    assert!(matches!(err, ScriptEnvError::InvalidPep723 { .. }));
}

#[test]
fn spec_resolution_merges_detects_conflicts_and_hashes_deterministically() {
    let code_a = concat!(
        "# /// script\n",
        "# requires-python = \">=3.12,<3.13\"\n",
        "# dependencies = [\"Pillow>=11.0\", \"numpy >= 2.0\"]\n",
        "# ///\n",
        "print('a')\n",
    );
    let code_b = concat!(
        "# /// script\n",
        "# dependencies = [\"NumPy>=2.0\"]\n",
        "# ///\n",
        "print('b')\n",
    );

    let spec_a = ScriptEnvSpec::resolve(code_a, Some("3.12"), &[], false).unwrap();
    let spec_b =
        ScriptEnvSpec::resolve(code_b, Some("3.12"), &["pillow>=11.0".to_owned()], false).unwrap();
    assert_eq!(spec_a.dependencies.len(), 2);
    assert_eq!(spec_b.dependencies, vec!["numpy>=2.0", "pillow>=11.0"]);
    assert_ne!(spec_a.env_hash(), "");

    let spec_c = ScriptEnvSpec::resolve(
        "# /// script\n# dependencies = [\"pillow>=11.0\", \"numpy>=2.0\"]\n# ///\n",
        Some("3.12"),
        &[],
        false,
    )
    .unwrap();
    let spec_d = ScriptEnvSpec::resolve(
        "print('no header')",
        Some("3.12"),
        &["NumPy>=2.0".to_owned(), "pillow>=11.0".to_owned()],
        false,
    )
    .unwrap();
    assert_eq!(spec_c.dependencies, spec_d.dependencies);
    assert_eq!(spec_c.env_hash(), spec_d.env_hash());

    let spec_src = ScriptEnvSpec::resolve(
        "print('no header')",
        Some("3.12"),
        &["numpy>=2.0".to_owned(), "pillow>=11.0".to_owned()],
        true,
    )
    .unwrap();
    assert_ne!(spec_c.env_hash(), spec_src.env_hash());

    let dep_conflict = ScriptEnvSpec::resolve(
        "# /// script\n# dependencies = [\"numpy>=2.0\"]\n# ///\n",
        None,
        &["numpy<2.0".to_owned()],
        false,
    )
    .expect_err("must reject conflicting dependency pins");
    assert!(matches!(
        dep_conflict,
        ScriptEnvError::DependencyConflict { .. }
    ));

    let py_conflict = ScriptEnvSpec::resolve(
        "# /// script\n# requires-python = \">=3.13\"\n# ///\n",
        Some("3.12"),
        &[],
        false,
    )
    .expect_err("must reject incompatible python version");
    assert!(matches!(
        py_conflict,
        ScriptEnvError::DependencyConflict { .. }
    ));
}

#[test]
fn format_pep723_stub_contains_no_user_code() {
    let stub = format_pep723_stub(Some(">=3.12,<3.13"), &["numpy>=2.0".to_owned()]);
    assert!(stub.starts_with("# /// script\n"));
    assert!(stub.ends_with("# ///\n"));
    let parsed = parse_pep723_metadata(&stub)
        .expect("stub parses cleanly")
        .expect("script block present");
    assert_eq!(parsed.requires_python.as_deref(), Some(">=3.12,<3.13"));
    assert_eq!(parsed.dependencies, vec!["numpy>=2.0"]);
}

#[test]
fn build_script_python_env_redirects_all_writable_and_thread_vars() {
    let scratch = Path::new("/tmp/mera-scratch-step-1");
    let venv = Path::new("/tmp/mera-runtime/script-envs/abc/venv");
    let py_dir = Path::new("/tmp/mera-runtime/python");

    let env: BTreeMap<OsString, OsString> = build_script_python_env(scratch, venv, Some(py_dir))
        .into_iter()
        .collect();

    assert_eq!(
        env.get(&OsString::from("PYTHONDONTWRITEBYTECODE")).unwrap(),
        "1"
    );
    assert_eq!(env.get(&OsString::from("PYTHONNOUSERSITE")).unwrap(), "1");
    assert_eq!(env.get(&OsString::from("PYTHONSAFEPATH")).unwrap(), "1");
    assert_eq!(env.get(&OsString::from("PYTHONUNBUFFERED")).unwrap(), "1");
    assert_eq!(env.get(&OsString::from("PYTHONUTF8")).unwrap(), "1");
    assert_eq!(
        env.get(&OsString::from("VIRTUAL_ENV")).unwrap(),
        venv.as_os_str()
    );
    assert_eq!(env.get(&OsString::from("OMP_NUM_THREADS")).unwrap(), "1");
    assert_eq!(
        env.get(&OsString::from("OPENBLAS_NUM_THREADS")).unwrap(),
        "1"
    );
    assert_eq!(env.get(&OsString::from("MKL_NUM_THREADS")).unwrap(), "1");
    assert_eq!(
        env.get(&OsString::from("NUMEXPR_NUM_THREADS")).unwrap(),
        "1"
    );
    assert_eq!(
        env.get(&OsString::from("UV_PYTHON_DOWNLOADS")).unwrap(),
        "never"
    );
    assert_eq!(
        env.get(&OsString::from("UV_PYTHON_INSTALL_DIR")).unwrap(),
        py_dir.as_os_str()
    );

    let expected_tmp = scratch.join("_mera").join("tmp");
    assert_eq!(
        env.get(&OsString::from("TMPDIR")).unwrap(),
        expected_tmp.as_os_str()
    );
    assert_eq!(
        env.get(&OsString::from("TEMP")).unwrap(),
        expected_tmp.as_os_str()
    );
    assert_eq!(
        env.get(&OsString::from("TMP")).unwrap(),
        expected_tmp.as_os_str()
    );
    assert_eq!(
        env.get(&OsString::from("HOME")).unwrap(),
        expected_tmp.join("home").as_os_str()
    );
    assert_eq!(
        env.get(&OsString::from("XDG_CACHE_HOME")).unwrap(),
        expected_tmp.join("cache").as_os_str()
    );
    assert_eq!(
        env.get(&OsString::from("MPLCONFIGDIR")).unwrap(),
        expected_tmp.join("mpl").as_os_str()
    );

    let tmp_dir = tempdir().unwrap();
    ensure_script_scratch_dirs(tmp_dir.path()).unwrap();
    assert!(tmp_dir.path().join("_mera/tmp/home").is_dir());
    assert!(tmp_dir.path().join("_mera/tmp/cache").is_dir());
    assert!(tmp_dir.path().join("_mera/tmp/mpl").is_dir());
    assert!(tmp_dir.path().join("_mera/out").is_dir());
}

#[cfg(unix)]
fn install_fake_uv_recorder(paths: &ModStorePaths, log_path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(paths.uv_dir()).unwrap();
    std::fs::create_dir_all(paths.uv_python_dir()).unwrap();
    // Simulate a managed Python tree existing under <runtime>/python.
    std::fs::write(paths.uv_python_dir().join("cpython-3.12"), b"stub").unwrap();

    let uv_bin = paths.uv_dir().join("uv");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf 'ARGS=%s|INSTALL_DIR=%s|CACHE_DIR=%s|DOWNLOADS=%s\n' "$*" "${{UV_PYTHON_INSTALL_DIR:-}}" "${{UV_CACHE_DIR:-}}" "${{UV_PYTHON_DOWNLOADS:-}}" >> "{log}"
sub="$1"
shift
case "$sub" in
  lock)
    # Verify stub deps.py contains no user code marker
    deps_file=""
    prev=""
    for a in "$@"; do
      if [ "$prev" = "--script" ]; then deps_file="$a"; fi
      prev="$a"
    done
    if grep -q "USER_SECRET_BODY" "$deps_file"; then
      echo "user script leaked into deps.py" >&2
      exit 99
    fi
    printf 'version = 1\n' > "${{deps_file}}.lock"
    ;;
  venv)
    venv_dir="$1"
    mkdir -p "$venv_dir/bin"
    printf '#!/bin/sh\necho fake-python\n' > "$venv_dir/bin/python3"
    chmod 755 "$venv_dir/bin/python3"
    ;;
  export)
    out_file=""
    prev=""
    for a in "$@"; do
      if [ "$prev" = "--output-file" ]; then out_file="$a"; fi
      prev="$a"
    done
    printf 'numpy==2.1.0 --hash=sha256:deadbeef\n' > "$out_file"
    ;;
  pip)
    exit 0
    ;;
  *)
    echo "unexpected subcommand: $sub" >&2
    exit 1
    ;;
esac
"#,
        log = log_path.display()
    );
    std::fs::write(&uv_bin, script).unwrap();
    std::fs::set_permissions(&uv_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn two_phase_prepare_and_offline_inspect_enforce_no_build_and_env_pins() {
    let temp = tempdir().unwrap();
    let paths = ModStorePaths::from_mods_root(temp.path().join("mods"));
    let log_path = temp.path().join("uv-invocations.log");
    install_fake_uv_recorder(&paths, &log_path);

    let user_code = concat!(
        "# /// script\n",
        "# requires-python = \">=3.12,<3.13\"\n",
        "# dependencies = [\"numpy>=2.0\"]\n",
        "# ///\n",
        "# USER_SECRET_BODY\n",
        "import os; print('should never run in phase 1')\n",
    );

    // 1. Offline inspection misses before preparation.
    let spec_no_build = ScriptEnvSpec::resolve(user_code, None, &[], false).unwrap();
    let lookup_before = inspect_script_env(&paths, &spec_no_build).unwrap();
    assert!(matches!(
        lookup_before,
        ScriptEnvLookup::NeedsPrepare {
            missing_uv: false,
            missing_managed_python: false,
            ..
        }
    ));

    // 2. Phase 1 prepare builds via staging -> atomic promote, enforcing --no-build and fixed uv env vars.
    let ready = prepare_script_env(&paths, &spec_no_build)
        .await
        .expect("prepare with fake uv");
    assert!(ready.python_executable.is_file());
    assert!(ready.lock_sha256.is_some());
    assert!(ready.env_dir.join(SCRIPT_ENV_METADATA_FILE).is_file());
    assert!(ready.read_only_paths.contains(&ready.env_dir));
    assert!(ready.read_only_paths.contains(&paths.uv_python_dir()));

    let log = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "expected lock, venv, export, pip install: {log}"
    );
    assert!(lines[0].contains("ARGS=lock --script ") && lines[0].contains("--no-build"));
    assert!(
        lines[1].contains("ARGS=venv ") && lines[1].contains("--python-preference only-managed")
    );
    assert!(lines[2].contains("ARGS=export --script ") && lines[2].contains("--frozen"));
    assert!(
        lines[3].contains("ARGS=pip install ")
            && lines[3].contains("--require-hashes")
            && lines[3].contains("--no-build")
    );
    let expected_py_dir = paths.uv_python_dir().display().to_string();
    let expected_cache_dir = paths.uv_cache_dir().display().to_string();
    for line in &lines {
        assert!(
            line.contains(&format!("INSTALL_DIR={expected_py_dir}")),
            "{line}"
        );
        assert!(
            line.contains(&format!("CACHE_DIR={expected_cache_dir}")),
            "{line}"
        );
        assert!(line.contains("DOWNLOADS=manual"), "{line}");
    }

    // 3. Offline inspect hits now.
    let lookup_after = inspect_script_env(&paths, &spec_no_build).unwrap();
    assert!(matches!(lookup_after, ScriptEnvLookup::Ready(_)));

    // 4. `allow_source_build = true` produces a separate hash and omits `--no-build`.
    std::fs::remove_file(&log_path).unwrap();
    let spec_allow_build = ScriptEnvSpec::resolve(user_code, None, &[], true).unwrap();
    assert_ne!(spec_no_build.env_hash(), spec_allow_build.env_hash());
    let ready_build = prepare_script_env(&paths, &spec_allow_build)
        .await
        .expect("prepare allow_source_build");
    assert!(ready_build.python_executable.is_file());
    let log_build = std::fs::read_to_string(&log_path).unwrap();
    assert!(!log_build.contains("--no-build"), "{log_build}");
    assert!(log_build.contains("--require-hashes"), "{log_build}");

    // 5. Zero-dependency script only runs `uv venv` and stores `lock_sha256 == None`.
    std::fs::remove_file(&log_path).unwrap();
    let spec_stdlib = ScriptEnvSpec::resolve("print('stdlib')", None, &[], false).unwrap();
    let ready_stdlib = prepare_script_env(&paths, &spec_stdlib).await.unwrap();
    assert_eq!(ready_stdlib.lock_sha256, None);
    let log_stdlib = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(log_stdlib.lines().count(), 1);
    assert!(log_stdlib.contains("ARGS=venv "));
}
