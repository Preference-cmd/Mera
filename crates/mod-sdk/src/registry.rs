use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use mera_core::diagnostic::Diagnostic;
use mera_core::event::EventBus;
use mera_core::health::Health;
use semver::VersionReq;
use serde_json::Value;

use crate::context::ModContext;
use crate::diagnostic::caller_error;
use crate::interface::{Invocation, Mod};
use crate::manifest::{API_VERSION, ModManifest};
use crate::schema;

/// Namespaces the kernel keeps for itself; no mod may take one as its id.
pub const RESERVED_NAMESPACES: &[&str] = &[
    "project",
    "board",
    "collection",
    "dynamic_workflow",
    "recipe",
    "job",
    "asset",
    "mod",
];

/// A registered mod as the kernel sees it.
#[derive(Debug, Clone)]
pub struct ModEntry {
    pub manifest: Arc<ModManifest>,
    /// Unavailable when the manifest was rejected or `start` failed.
    pub health: Health,
}

struct Registered {
    entry: ModEntry,
    /// Present only while the mod is available; dropping it releases whatever
    /// the instance owns (for a process mod, the child process).
    instance: Option<Arc<dyn Mod>>,
}

const REGISTRY_POISONED: &str = "mod registry lock poisoned";

/// The registered mods of one application. A rejected mod stays listed as
/// unavailable and never affects the others.
///
/// Stopping and dynamic lifecycle mutations share `lifecycle_lock`, while reads
/// (`list`, `invoke` dispatch lookup) hold only a short `RwLock` read guard.
pub struct ModRegistry {
    mods: RwLock<Vec<Registered>>,
    stopped: AtomicBool,
    lifecycle_lock: tokio::sync::Mutex<()>,
}

impl ModRegistry {
    /// Create an empty mod registry.
    pub fn empty() -> Self {
        Self {
            mods: RwLock::new(Vec::new()),
            stopped: AtomicBool::new(false),
            lifecycle_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Validate every manifest, then start each accepted mod in order.
    pub async fn start(mods: Vec<Arc<dyn Mod>>, event_bus: &EventBus) -> Self {
        let mut registered: Vec<Registered> = Vec::with_capacity(mods.len());
        for instance in mods {
            let manifest = Arc::new(instance.manifest().clone());
            let taken = registered
                .iter()
                .any(|other| other.entry.manifest.id == manifest.id);
            registered.push(start_single(instance, manifest, taken, event_bus).await);
        }
        Self {
            mods: RwLock::new(registered),
            stopped: AtomicBool::new(false),
            lifecycle_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Validate and start one mod at runtime, adding it to the registry.
    ///
    /// Fails with `DUPLICATE_ID` (without touching the existing entry) if a mod
    /// with the same `id` is already registered. If validation or `start` fails
    /// for a fresh id, the mod is recorded as `Health::Unavailable` and its
    /// [`ModEntry`] is returned so callers can surface the diagnostic.
    pub async fn register_and_start(
        &self,
        instance: Arc<dyn Mod>,
        event_bus: &EventBus,
    ) -> Result<ModEntry, Diagnostic> {
        let _guard = self.lifecycle_lock.lock().await;
        let manifest = Arc::new(instance.manifest().clone());
        let id = manifest.id.as_str();
        if self
            .mods
            .read()
            .expect(REGISTRY_POISONED)
            .iter()
            .any(|other| other.entry.manifest.id == id)
        {
            return Err(caller_error(
                id,
                "DUPLICATE_ID",
                format!("mod id `{id}` is already registered"),
            ));
        }

        let item = start_single(instance, manifest, false, event_bus).await;
        let entry = item.entry.clone();
        self.mods.write().expect(REGISTRY_POISONED).push(item);
        if entry.health.is_available() {
            self.stopped.store(false, Ordering::SeqCst);
        }
        Ok(entry)
    }

    /// Stop any existing registration for `mod_id` and start `new_instance` in its place.
    ///
    /// If `mod_id` is not currently registered, behaves like [`Self::register_and_start`].
    pub async fn replace_and_restart(
        &self,
        mod_id: &str,
        new_instance: Arc<dyn Mod>,
        event_bus: &EventBus,
    ) -> Result<ModEntry, Diagnostic> {
        let _guard = self.lifecycle_lock.lock().await;
        let manifest = Arc::new(new_instance.manifest().clone());
        if manifest.id != mod_id {
            return Err(caller_error(
                mod_id,
                "INVALID_ID",
                format!(
                    "replacement mod id `{}` does not match `{mod_id}`",
                    manifest.id
                ),
            ));
        }

        let previous = {
            let mut guard = self.mods.write().expect(REGISTRY_POISONED);
            guard
                .iter()
                .position(|r| r.entry.manifest.id == mod_id)
                .map(|idx| guard.remove(idx))
        };
        if let Some(old) = previous
            && let Some(old_instance) = old.instance
            && let Err(diagnostic) = old_instance.stop().await
        {
            tracing::warn!(
                mod_id = %mod_id,
                message = diagnostic.message(),
                "previous mod failed to stop during replace"
            );
        }

        let item = start_single(new_instance, manifest, false, event_bus).await;
        let entry = item.entry.clone();
        self.mods.write().expect(REGISTRY_POISONED).push(item);
        if entry.health.is_available() {
            self.stopped.store(false, Ordering::SeqCst);
        }
        Ok(entry)
    }

    /// Remove `mod_id` from the registry and stop its running instance if present.
    pub async fn unregister_and_stop(&self, mod_id: &str) -> Result<(), Diagnostic> {
        let _guard = self.lifecycle_lock.lock().await;
        let removed = {
            let mut guard = self.mods.write().expect(REGISTRY_POISONED);
            let idx = guard
                .iter()
                .position(|r| r.entry.manifest.id == mod_id)
                .ok_or_else(|| {
                    caller_error(
                        mod_id,
                        "UNKNOWN_MOD",
                        format!("no mod `{mod_id}` is registered"),
                    )
                })?;
            guard.remove(idx)
        };
        if let Some(instance) = removed.instance {
            instance.stop().await?;
        }
        Ok(())
    }

    /// Stop every mod that started.
    pub async fn stop(&self) {
        let _guard = self.lifecycle_lock.lock().await;
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let running: Vec<(String, Arc<dyn Mod>)> = {
            let guard = self.mods.read().expect(REGISTRY_POISONED);
            guard
                .iter()
                .filter(|r| r.entry.health.is_available())
                .filter_map(|r| {
                    r.instance
                        .as_ref()
                        .map(|inst| (r.entry.manifest.id.clone(), Arc::clone(inst)))
                })
                .collect()
        };
        for (mod_id, instance) in running {
            if let Err(diagnostic) = instance.stop().await {
                tracing::warn!(
                    mod_id = %mod_id,
                    message = diagnostic.message(),
                    "mod failed to stop"
                );
            }
        }
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Call an operation on mod `mod_id`. Fails before reaching the mod when
    /// the mod is unknown, unavailable, or does not declare the operation.
    /// Input validation is owned by the caller (the operation service).
    pub async fn invoke(&self, mod_id: &str, invocation: Invocation) -> Result<Value, Diagnostic> {
        let instance = {
            let guard = self.mods.read().expect(REGISTRY_POISONED);
            let registered = guard
                .iter()
                .find(|registered| registered.entry.manifest.id == mod_id)
                .ok_or_else(|| {
                    caller_error(
                        mod_id,
                        "UNKNOWN_MOD",
                        format!("no mod `{mod_id}` is registered"),
                    )
                })?;
            if let Health::Unavailable(diagnostic) = &registered.entry.health {
                return Err(diagnostic.clone());
            }
            let manifest = &registered.entry.manifest;
            if !manifest
                .operations
                .iter()
                .any(|op| op.name == invocation.operation)
            {
                return Err(caller_error(
                    mod_id,
                    "UNKNOWN_OPERATION",
                    format!(
                        "mod `{mod_id}` declares no operation `{}`",
                        invocation.operation
                    ),
                ));
            }
            registered
                .instance
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| {
                    caller_error(
                        mod_id,
                        "NOT_RUNNING",
                        format!("mod `{mod_id}` has no running instance"),
                    )
                })?
        };
        instance.invoke(invocation).await
    }

    /// Every registered mod, in registration order.
    pub fn list(&self) -> Vec<ModEntry> {
        self.mods
            .read()
            .expect(REGISTRY_POISONED)
            .iter()
            .map(|registered| registered.entry.clone())
            .collect()
    }
}

async fn start_single(
    instance: Arc<dyn Mod>,
    manifest: Arc<ModManifest>,
    id_taken: bool,
    event_bus: &EventBus,
) -> Registered {
    let (health, keep) = match validate_and_compile(&manifest, id_taken) {
        Err(diagnostic) => (Health::Unavailable(diagnostic), false),
        Ok(()) => {
            let ctx = ModContext::new(Arc::clone(&manifest), event_bus.clone());
            match instance.start(ctx).await {
                Ok(()) => (Health::Available, true),
                Err(diagnostic) => (Health::Unavailable(diagnostic), false),
            }
        }
    };
    let instance = if keep { Some(instance) } else { None };
    if let Health::Unavailable(diagnostic) = &health {
        tracing::warn!(
            mod_id = %manifest.id,
            message = diagnostic.message(),
            "mod unavailable"
        );
    }
    Registered {
        entry: ModEntry { manifest, health },
        instance,
    }
}

/// Check `manifest` and compile its schemas to verify validity.
pub(crate) fn validate_and_compile(
    manifest: &ModManifest,
    id_taken: bool,
) -> Result<(), Diagnostic> {
    validate_manifest_structure(manifest, id_taken)?;
    compile_schemas(manifest)
}

fn validate_manifest_structure(manifest: &ModManifest, id_taken: bool) -> Result<(), Diagnostic> {
    let id = manifest.id.as_str();
    let reject = |code: &str, message: String| Err(caller_error(id, code, message));

    if RESERVED_NAMESPACES.contains(&id) {
        return reject(
            "RESERVED_ID",
            format!("mod id `{id}` is reserved by the kernel"),
        );
    }
    if !is_snake_case(id) {
        return reject(
            "INVALID_ID",
            format!("mod id `{id}` is not lowercase snake_case"),
        );
    }
    if id_taken {
        return reject(
            "DUPLICATE_ID",
            format!("mod id `{id}` is already registered"),
        );
    }
    match VersionReq::parse(&manifest.api) {
        Ok(requirement) if requirement.matches(&API_VERSION) => {}
        Ok(_) => {
            return reject(
                "INCOMPATIBLE_API",
                format!(
                    "mod `{id}` requires api `{}`; this kernel implements {API_VERSION}",
                    manifest.api
                ),
            );
        }
        Err(error) => {
            return reject(
                "INVALID_API",
                format!(
                    "mod `{id}` api `{}` is not a semver requirement: {error}",
                    manifest.api
                ),
            );
        }
    }

    let in_namespace = |name: &str| {
        name.strip_prefix(id)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|rest| !rest.is_empty())
    };
    let mut names = HashSet::new();
    for operation in &manifest.operations {
        if !in_namespace(&operation.name) {
            return reject(
                "OPERATION_OUTSIDE_NAMESPACE",
                format!("operation `{}` is not under `{id}.`", operation.name),
            );
        }
        if !names.insert(operation.name.as_str()) {
            return reject(
                "DUPLICATE_OPERATION",
                format!("operation `{}` is declared twice", operation.name),
            );
        }
        if let Err(why) = schema::check_asset_marker_locations(&operation.input_schema) {
            return reject(
                "INVALID_SCHEMA",
                format!("operation `{}` input_schema: {why}", operation.name),
            );
        }
        if let Err(why) = schema::check_inline_text_marker_locations(&operation.input_schema) {
            return reject(
                "INVALID_SCHEMA",
                format!("operation `{}` input_schema: {why}", operation.name),
            );
        }
        if let Err(why) = schema::check_host_path_marker_locations(&operation.input_schema) {
            return reject(
                "INVALID_SCHEMA",
                format!("operation `{}` input_schema: {why}", operation.name),
            );
        }
        if let Err(why) = schema::check_asset_marker_locations(&operation.output_schema) {
            return reject(
                "INVALID_SCHEMA",
                format!("operation `{}` output_schema: {why}", operation.name),
            );
        }
        if let Err(why) = schema::check_inline_text_marker_locations(&operation.output_schema) {
            return reject(
                "INVALID_SCHEMA",
                format!("operation `{}` output_schema: {why}", operation.name),
            );
        }
    }
    let mut kinds = HashSet::new();
    for event in &manifest.events {
        if !in_namespace(&event.kind) {
            return reject(
                "EVENT_OUTSIDE_NAMESPACE",
                format!("event kind `{}` is not under `{id}.`", event.kind),
            );
        }
        if !kinds.insert(event.kind.as_str()) {
            return reject(
                "DUPLICATE_EVENT",
                format!("event kind `{}` is declared twice", event.kind),
            );
        }
    }
    Ok(())
}

fn compile_schemas(manifest: &ModManifest) -> Result<(), Diagnostic> {
    let id = manifest.id.as_str();
    let compile = |schema: &Value, what: String| {
        schema::compile(schema).map_err(|error| {
            caller_error(
                id,
                "INVALID_SCHEMA",
                format!("{what} is not valid JSON Schema 2020-12: {error}"),
            )
        })
    };

    for operation in &manifest.operations {
        let name = &operation.name;
        if !schema::is_object(&operation.input_schema) {
            return Err(caller_error(
                id,
                "INVALID_SCHEMA",
                format!("operation `{name}` input_schema root must be `\"type\": \"object\"`"),
            ));
        }
        compile(
            &operation.input_schema,
            format!("operation `{name}` input_schema"),
        )?;
        compile(
            &operation.output_schema,
            format!("operation `{name}` output_schema"),
        )?;
    }
    for event in &manifest.events {
        compile(&event.schema, format!("event `{}` schema", event.kind))?;
    }
    Ok(())
}

fn is_snake_case(id: &str) -> bool {
    id.starts_with(|c: char| c.is_ascii_lowercase())
        && !id.ends_with('_')
        && !id.contains("__")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Default for ModRegistry {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use mera_core::task::CancelToken;
    use serde_json::json;

    use super::*;
    use crate::diagnostic::internal_error;
    use crate::manifest::{ModEvent, OperationAnnotations, OperationSpec};

    struct Fake {
        manifest: ModManifest,
        fail_start: bool,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Mod for Fake {
        fn manifest(&self) -> &ModManifest {
            &self.manifest
        }

        async fn start(&self, _ctx: ModContext) -> Result<(), Diagnostic> {
            if self.fail_start {
                return Err(internal_error(&self.manifest.id, "START", "no backend"));
            }
            Ok(())
        }

        async fn invoke(&self, invocation: Invocation) -> Result<Value, Diagnostic> {
            self.calls.lock().unwrap().push(invocation.operation);
            Ok(json!({ "echo": invocation.input }))
        }

        async fn stop(&self) -> Result<(), Diagnostic> {
            self.calls.lock().unwrap().push("stop".to_owned());
            Ok(())
        }
    }

    fn manifest(id: &str) -> ModManifest {
        ModManifest {
            id: id.to_owned(),
            name: id.to_owned(),
            version: "0.1.0".to_owned(),
            api: "0.1".to_owned(),
            operations: vec![operation(&format!("{id}.ping"))],
            events: vec![ModEvent {
                kind: format!("{id}.pinged"),
                schema: json!({ "type": "object" }),
            }],
        }
    }

    fn operation(name: &str) -> OperationSpec {
        OperationSpec {
            name: name.to_owned(),
            description: "ping".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            annotations: OperationAnnotations::default(),
        }
    }

    fn fake(manifest: ModManifest) -> Arc<Fake> {
        Arc::new(Fake {
            manifest,
            fail_start: false,
            calls: Mutex::new(Vec::new()),
        })
    }

    async fn registry(mods: Vec<Arc<Fake>>) -> ModRegistry {
        let mods = mods.into_iter().map(|m| m as Arc<dyn Mod>).collect();
        ModRegistry::start(mods, &EventBus::new()).await
    }

    fn rejection(registry: &ModRegistry, index: usize) -> String {
        match &registry.list()[index].health {
            Health::Unavailable(diagnostic) => diagnostic.code().as_str().to_owned(),
            Health::Available => panic!("mod {index} was accepted"),
        }
    }

    fn rejection_message(registry: &ModRegistry, index: usize) -> String {
        match &registry.list()[index].health {
            Health::Unavailable(diagnostic) => diagnostic.message().to_owned(),
            Health::Available => panic!("mod {index} was accepted"),
        }
    }

    fn test_invocation(op: &str, input: Value) -> Invocation {
        Invocation::new(op, input, PathBuf::from("/tmp"), CancelToken::new())
    }

    #[tokio::test]
    async fn invalid_manifests_are_unavailable_with_a_reason() {
        let mut outside_op = manifest("outside_op");
        outside_op.operations.push(operation("other.ping"));
        let mut prefix_only = manifest("prefix_only");
        prefix_only.operations.push(operation("prefix_onlyx.ping"));
        let mut twice_op = manifest("twice_op");
        twice_op.operations.push(operation("twice_op.ping"));
        let mut outside_event = manifest("outside_event");
        outside_event.events[0].kind = "board.changed".to_owned();
        let mut twice_event = manifest("twice_event");
        twice_event.events.push(twice_event.events[0].clone());
        let mut newer_api = manifest("newer_api");
        newer_api.api = "0.2".to_owned();
        let mut bad_api = manifest("bad_api");
        bad_api.api = "latest".to_owned();

        let cases = [
            (manifest("board"), "MOD/CALLER_RESERVED_ID"),
            (manifest("Bad-Id"), "MOD/CALLER_INVALID_ID"),
            (outside_op, "MOD/CALLER_OPERATION_OUTSIDE_NAMESPACE"),
            (prefix_only, "MOD/CALLER_OPERATION_OUTSIDE_NAMESPACE"),
            (twice_op, "MOD/CALLER_DUPLICATE_OPERATION"),
            (outside_event, "MOD/CALLER_EVENT_OUTSIDE_NAMESPACE"),
            (twice_event, "MOD/CALLER_DUPLICATE_EVENT"),
            (newer_api, "MOD/CALLER_INCOMPATIBLE_API"),
            (bad_api, "MOD/CALLER_INVALID_API"),
        ];
        for (manifest, code) in cases {
            let id = manifest.id.clone();
            let registry = registry(vec![fake(manifest), fake(self::manifest("good"))]).await;
            assert_eq!(rejection(&registry, 0), code, "{id}");
            assert!(registry.list()[1].health.is_available(), "{id} spared good");
            registry
                .invoke("good", test_invocation("good.ping", json!({})))
                .await
                .expect("the other mod still works");
        }
    }

    #[tokio::test]
    async fn invalid_schemas_are_rejected_with_invalid_schema_code() {
        // Non-object input root
        let mut non_object_input = manifest("bad_input_root");
        non_object_input.operations[0].input_schema = json!({ "type": "string" });

        // Uncompilable input schema
        let mut uncompilable_input = manifest("bad_input_schema");
        uncompilable_input.operations[0].input_schema = json!({
            "type": "object",
            "properties": { "foo": { "type": 123 } }
        });

        // Uncompilable output schema
        let mut uncompilable_output = manifest("bad_output_schema");
        uncompilable_output.operations[0].output_schema = json!({
            "type": 456
        });

        // Uncompilable event schema
        let mut uncompilable_event = manifest("bad_event_schema");
        uncompilable_event.events[0].schema = json!({
            "type": 789
        });

        // Asset marker under oneOf
        let mut oneof_marker = manifest("bad_oneof");
        oneof_marker.operations[0].input_schema = json!({
            "type": "object",
            "properties": {
                "doc": {
                    "oneOf": [
                        { "type": "string", "format": "asset" },
                        { "type": "null" }
                    ]
                }
            }
        });

        // Inline-text marker under oneOf
        let mut inline_marker = manifest("bad_inline");
        inline_marker.operations[0].input_schema = json!({
            "type": "object",
            "properties": {
                "text": {
                    "oneOf": [
                        { "type": "string", "format": "inline-text" },
                        { "type": "null" }
                    ]
                }
            }
        });

        // Inline-text marker in an output schema, outside properties
        let mut inline_output = manifest("bad_inline_output");
        inline_output.operations[0].output_schema = json!({
            "type": "object",
            "additionalProperties": { "type": "string", "format": "inline-text" }
        });

        let cases = [
            (non_object_input, "bad_input_root.ping"),
            (uncompilable_input, "bad_input_schema.ping"),
            (uncompilable_output, "bad_output_schema.ping"),
            (uncompilable_event, "bad_event_schema.pinged"),
            (oneof_marker, "bad_oneof.ping"),
            (inline_marker, "bad_inline.ping"),
            (inline_output, "bad_inline_output.ping"),
        ];

        for (manifest, target_name) in cases {
            let id = manifest.id.clone();
            let registry = registry(vec![fake(manifest), fake(self::manifest("good"))]).await;
            assert_eq!(rejection(&registry, 0), "MOD/CALLER_INVALID_SCHEMA", "{id}");
            let msg = rejection_message(&registry, 0);
            assert!(
                msg.contains(target_name),
                "rejection message for {id} ({msg}) must name `{target_name}`"
            );
            assert!(registry.list()[1].health.is_available(), "{id} spared good");
            registry
                .invoke("good", test_invocation("good.ping", json!({})))
                .await
                .expect("good mod still works");
        }
    }

    #[tokio::test]
    async fn a_second_mod_with_the_same_id_is_rejected() {
        let first = fake(manifest("demo"));
        let registry = registry(vec![Arc::clone(&first), fake(manifest("demo"))]).await;

        assert!(registry.list()[0].health.is_available());
        assert_eq!(rejection(&registry, 1), "MOD/CALLER_DUPLICATE_ID");
        registry
            .invoke("demo", test_invocation("demo.ping", json!({})))
            .await
            .expect("first registration serves");
        assert_eq!(first.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn invoke_reaches_only_available_mods_and_declared_operations() {
        let broken = Arc::new(Fake {
            manifest: manifest("broken"),
            fail_start: true,
            calls: Mutex::new(Vec::new()),
        });
        let demo = fake(manifest("demo"));
        let registry = registry(vec![Arc::clone(&broken), Arc::clone(&demo)]).await;

        let output = registry
            .invoke("demo", test_invocation("demo.ping", json!({ "n": 1 })))
            .await
            .expect("declared operation");
        assert_eq!(output, json!({ "echo": { "n": 1 } }));

        let code = |result: Result<Value, Diagnostic>| {
            result.expect_err("rejected").code().as_str().to_owned()
        };
        assert_eq!(
            code(
                registry
                    .invoke("broken", test_invocation("broken.ping", json!({})))
                    .await
            ),
            "MOD/INTERNAL_START"
        );
        assert_eq!(
            code(
                registry
                    .invoke("demo", test_invocation("demo.pong", json!({})))
                    .await
            ),
            "MOD/CALLER_UNKNOWN_OPERATION"
        );
        assert_eq!(
            code(
                registry
                    .invoke("missing", test_invocation("missing.ping", json!({})))
                    .await
            ),
            "MOD/CALLER_UNKNOWN_MOD"
        );
        assert!(broken.calls.lock().unwrap().is_empty());
        assert_eq!(*demo.calls.lock().unwrap(), ["demo.ping"]);
    }

    #[tokio::test]
    async fn stop_reaches_only_started_mods() {
        let broken = Arc::new(Fake {
            manifest: manifest("broken"),
            fail_start: true,
            calls: Mutex::new(Vec::new()),
        });
        let demo = fake(manifest("demo"));
        let rejected = fake(manifest("board"));
        let registry = registry(vec![
            Arc::clone(&broken),
            Arc::clone(&demo),
            Arc::clone(&rejected),
        ])
        .await;

        registry.stop().await;

        assert_eq!(*demo.calls.lock().unwrap(), ["stop"]);
        assert!(broken.calls.lock().unwrap().is_empty());
        assert!(rejected.calls.lock().unwrap().is_empty());

        registry.stop().await;
        assert_eq!(
            *demo.calls.lock().unwrap(),
            ["stop"],
            "a repeated stop must not stop the mod twice"
        );
    }

    /// A mod whose `stop` waits for a permit, so a test can cancel the stop
    /// future in the middle of the loop.
    struct SlowStop {
        manifest: ModManifest,
        entered: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
        stops: AtomicUsize,
    }

    #[async_trait]
    impl Mod for SlowStop {
        fn manifest(&self) -> &ModManifest {
            &self.manifest
        }

        async fn start(&self, _ctx: ModContext) -> Result<(), Diagnostic> {
            Ok(())
        }

        async fn invoke(&self, _invocation: Invocation) -> Result<Value, Diagnostic> {
            Ok(json!({}))
        }

        async fn stop(&self) -> Result<(), Diagnostic> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_waiters();
            let _permit = self
                .release
                .acquire()
                .await
                .map_err(|_| internal_error("slow_stop", "STOP", "release closed"))?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_cancelled_stop_restarts_the_loop_and_then_marks_the_registry_stopped() {
        let slow = Arc::new(SlowStop {
            manifest: manifest("slow_stop"),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            stops: AtomicUsize::new(0),
        });
        let after = fake(manifest("after"));
        let registry = ModRegistry::start(
            vec![
                Arc::clone(&slow) as Arc<dyn Mod>,
                Arc::clone(&after) as Arc<dyn Mod>,
            ],
            &EventBus::new(),
        )
        .await;

        let cancelled = tokio::time::timeout(Duration::from_millis(50), registry.stop()).await;
        assert!(cancelled.is_err(), "the first stop must be cancelled");
        assert_eq!(slow.stops.load(Ordering::SeqCst), 1);
        assert!(
            after.calls.lock().unwrap().is_empty(),
            "the cancelled loop never reached the second mod"
        );

        slow.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), registry.stop())
            .await
            .expect("a cancelled stop is retried and completes");
        assert_eq!(
            slow.stops.load(Ordering::SeqCst),
            2,
            "the retry restarts at the first mod, so `stop` must tolerate repeats"
        );
        assert_eq!(
            *after.calls.lock().unwrap(),
            ["stop"],
            "the restarted loop still reaches the remaining mods"
        );

        registry.stop().await;
        assert_eq!(
            slow.stops.load(Ordering::SeqCst),
            2,
            "a completed stop is not repeated"
        );
        assert_eq!(*after.calls.lock().unwrap(), ["stop"]);
    }

    #[tokio::test]
    async fn runtime_register_replace_and_unregister_work_without_restart() {
        let bus = EventBus::new();
        let registry = ModRegistry::empty();

        let first = fake(manifest("alpha"));
        let entry = registry
            .register_and_start(Arc::clone(&first) as Arc<dyn Mod>, &bus)
            .await
            .expect("register alpha");
        assert!(entry.health.is_available());
        assert_eq!(registry.list().len(), 1);

        let dup = fake(manifest("alpha"));
        let err = registry
            .register_and_start(dup as Arc<dyn Mod>, &bus)
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), "MOD/CALLER_DUPLICATE_ID");

        let replacement = fake(manifest("alpha"));
        let entry = registry
            .replace_and_restart("alpha", Arc::clone(&replacement) as Arc<dyn Mod>, &bus)
            .await
            .expect("replace alpha");
        assert!(entry.health.is_available());
        assert_eq!(*first.calls.lock().unwrap(), ["stop"]);

        let reply = registry
            .invoke(
                "alpha",
                test_invocation("alpha.ping", json!({ "ok": true })),
            )
            .await
            .expect("invoke replacement");
        assert_eq!(reply, json!({ "echo": { "ok": true } }));
        assert_eq!(*replacement.calls.lock().unwrap(), ["alpha.ping"]);

        registry
            .unregister_and_stop("alpha")
            .await
            .expect("unregister alpha");
        assert!(registry.list().is_empty());
        assert_eq!(*replacement.calls.lock().unwrap(), ["alpha.ping", "stop"]);
    }
}
