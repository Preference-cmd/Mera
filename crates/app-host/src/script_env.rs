//! PEP 723 metadata parsing and two-phase content-addressed Python script
//! environment management.
//!
//! Phase 1 (`prepare_script_env`) resolves and locks dependencies on the host
//! side outside the execution sandbox without running user script code. Phase 2
//! (`inspect_script_env`, `build_script_python_env`) inspects cached
//! environments offline and constructs read-only, scratch-redirected runtime
//! parameters for sandboxed execution.

mod pep723;
mod prepare;
#[cfg(test)]
mod tests;

pub use pep723::{
    DEFAULT_SCRIPT_PYTHON_VERSION, Pep723Metadata, ScriptEnvError, ScriptEnvSpec,
    format_pep723_stub, host_target_triple, parse_pep723_metadata,
};
pub use prepare::{
    SCRIPT_ENV_METADATA_FILE, ScriptEnvLookup, ScriptEnvMetadata, ScriptEnvReady,
    build_script_python_env, ensure_managed_python, ensure_script_scratch_dirs, inspect_script_env,
    prepare_script_env, touch_script_env_last_used,
};
