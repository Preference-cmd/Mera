//! Cross-platform child-process mod binary fixture used by `mod_manager` tests.
//!
//! Dispatches behavior by executable file stem so tests work under
//! `ProcessMod::spawn` with `cmd.env_clear()` and empty argv on every OS.

use async_trait::async_trait;
use mera_core::diagnostic::Diagnostic;
use mod_sdk::diagnostic::internal_error;
use mod_sdk::sdk::serve;
use mod_sdk::{Invocation, Mod, ModContext, ModManifest, OperationAnnotations, OperationSpec};
use serde_json::{Value, json};

fn echo_op(name: &str) -> OperationSpec {
    OperationSpec {
        name: name.to_owned(),
        description: "Echo input text".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": { "type": "string" }
            }
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "echo": { "type": "string" }
            }
        }),
        annotations: OperationAnnotations {
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
            deterministic: true,
        },
    }
}

fn manifest_v1() -> ModManifest {
    ModManifest {
        id: "ext_demo".to_owned(),
        name: "External Demo".to_owned(),
        version: "0.1.0".to_owned(),
        api: "0.1".to_owned(),
        operations: vec![echo_op("ext_demo.echo")],
        events: Vec::new(),
    }
}

fn manifest_v2() -> ModManifest {
    let mut m = manifest_v1();
    m.version = "0.2.0".to_owned();
    m.operations.push(OperationSpec {
        name: "ext_demo.read_file".to_owned(),
        description: "Inspect a workspace file path".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "format": "host-path" }
            }
        }),
        output_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
        annotations: OperationAnnotations {
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
            deterministic: true,
        },
    });
    m
}

struct FixtureMod {
    manifest: ModManifest,
    fail_start: bool,
}

#[async_trait]
impl Mod for FixtureMod {
    fn manifest(&self) -> &ModManifest {
        &self.manifest
    }

    async fn start(&self, _ctx: ModContext) -> Result<(), Diagnostic> {
        if self.fail_start {
            return Err(internal_error(
                &self.manifest.id,
                "BROKEN_START",
                "intentional startup failure",
            ));
        }
        Ok(())
    }

    async fn invoke(&self, invocation: Invocation) -> Result<Value, Diagnostic> {
        let text = invocation
            .input
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("ok");
        Ok(json!({ "echo": format!("{}:{text}", self.manifest.version) }))
    }

    async fn stop(&self) -> Result<(), Diagnostic> {
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let stem = exe.file_stem().and_then(|s| s.to_str()).unwrap_or_default();

    let instance = match stem {
        "fixture-mod-v1" => FixtureMod {
            manifest: manifest_v1(),
            fail_start: false,
        },
        "fixture-mod-v2" => FixtureMod {
            manifest: manifest_v2(),
            fail_start: false,
        },
        "fixture-mod-fail-start" => FixtureMod {
            manifest: ModManifest {
                id: "ext_fail".to_owned(),
                name: "Broken Mod".to_owned(),
                version: "0.1.0".to_owned(),
                api: "0.1".to_owned(),
                operations: vec![echo_op("ext_fail.ping")],
                events: Vec::new(),
            },
            fail_start: true,
        },
        "fixture-mod-mismatch" => FixtureMod {
            manifest: ModManifest {
                version: "9.9.9".to_owned(),
                ..manifest_v1()
            },
            fail_start: false,
        },
        "fixture-mod-bad-init" => {
            eprintln!("intentional initialize failure");
            std::process::exit(2);
        }
        other => {
            eprintln!("unexpected fixture stem: {other}");
            std::process::exit(3);
        }
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move { serve(instance).await })?;
    Ok(())
}
