use crate::*;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

pub const PLUGIN_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCapability {
    pub kind: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub mime_types: Vec<String>,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePluginManifest {
    pub protocol: u32,
    pub name: String,
    pub version: String,
    pub capabilities: Vec<RuntimeCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRequest {
    pub protocol: u32,
    pub operation: String,
    pub workspace: PathBuf,
    pub source: Option<SourceRecord>,
    pub unit: Option<Unit>,
    pub graph: Option<DocumentGraph>,
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeResponse {
    pub protocol: u32,
    pub ok: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub units: Vec<Unit>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub source: Option<SourceRecord>,
    #[serde(default)]
    pub unit: Option<Unit>,
    #[serde(default)]
    pub graph: Option<DocumentGraph>,
    #[serde(default)]
    pub render_report: Option<RenderReport>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimePlugin {
    pub timeout: Duration,
    pub executable: PathBuf,
    pub manifest: RuntimePluginManifest,
}

/// Controls discovery and registered capabilities; plugins remain trusted native code.
#[derive(Debug, Clone)]
pub struct RuntimePluginPolicy {
    pub enabled: bool,
    pub timeout: Duration,
    pub allow_capabilities: Option<BTreeSet<String>>,
    pub deny_capabilities: BTreeSet<String>,
}

impl Default for RuntimePluginPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout: Duration::from_secs(60),
            allow_capabilities: None,
            deny_capabilities: BTreeSet::new(),
        }
    }
}

impl RuntimePluginPolicy {
    /// Whether this policy registers capabilities of `kind`.
    pub fn permits(&self, kind: &str) -> bool {
        !self.deny_capabilities.contains(kind)
            && self
                .allow_capabilities
                .as_ref()
                .is_none_or(|allowed| allowed.contains(kind))
    }
}

fn plugin_warnings(warnings: Vec<String>) -> Vec<String> {
    warnings
        .into_iter()
        .map(|w| Diagnostic::new(DiagnosticCode::PluginWarning, w).to_string())
        .collect()
}

pub fn read_manifest(executable: &Path) -> Result<RuntimePluginManifest> {
    read_manifest_with_timeout(executable, Duration::from_secs(60))
}

fn read_manifest_with_timeout(
    executable: &Path,
    timeout: Duration,
) -> Result<RuntimePluginManifest> {
    let out = Command::new(executable)
        .arg("--anytopdf-manifest")
        .bounded_output(timeout)
        .with_context(|| format!("run plugin manifest {}", executable.display()))?;
    if !out.status.success() {
        bail!(
            "plugin {} manifest failed: {}",
            executable.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let manifest: RuntimePluginManifest =
        serde_json::from_slice(&out.stdout).context("parse runtime plugin manifest")?;
    if manifest.protocol != PLUGIN_PROTOCOL_VERSION {
        bail!(
            "plugin {} protocol {} != core {}",
            manifest.name,
            manifest.protocol,
            PLUGIN_PROTOCOL_VERSION
        );
    }
    if manifest.name.trim().is_empty() || manifest.capabilities.is_empty() {
        bail!("plugin manifest must declare a name and capabilities");
    }
    Ok(manifest)
}

pub fn discover_runtime_plugins() -> (Vec<RuntimePlugin>, Vec<String>) {
    discover_with_policy(&RuntimePluginPolicy::default())
}

/// Discover runtime plugins under `policy`, returning ignored executables as warnings.
pub fn discover_runtime_plugins_with_policy(
    policy: &RuntimePluginPolicy,
) -> (Vec<RuntimePlugin>, Vec<String>) {
    discover_with_policy(policy)
}

fn discover_with_policy(policy: &RuntimePluginPolicy) -> (Vec<RuntimePlugin>, Vec<String>) {
    if !policy.enabled {
        return (Vec::new(), Vec::new());
    }
    let mut candidates = BTreeSet::new();
    let mut warnings = Vec::new();

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            scan_plugin_dir(&dir, &mut candidates);
        }
    }
    if let Some(path) = env::var_os("ANYTOPDF_PLUGIN_PATH") {
        for dir in env::split_paths(&path) {
            scan_plugin_dir(&dir, &mut candidates);
        }
    }

    let mut plugins = Vec::new();
    for executable in candidates {
        match read_manifest_with_timeout(&executable, policy.timeout) {
            Ok(manifest) => plugins.push(RuntimePlugin {
                executable,
                manifest,
                timeout: policy.timeout,
            }),
            Err(e) => warnings.push(format!(
                "runtime plugin {} ignored: {e:#}",
                executable.display()
            )),
        }
    }
    (plugins, warnings)
}

fn scan_plugin_dir(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|x| x.to_str()) else {
            continue;
        };
        let normalized = name.strip_suffix(".exe").unwrap_or(name);
        if normalized.starts_with("anytopdf-plugin-") && path.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if path
                    .metadata()
                    .map(|m| m.permissions().mode() & 0o111 == 0)
                    .unwrap_or(true)
                {
                    continue;
                }
            }
            if let Ok(path) = path.canonicalize() {
                out.insert(path);
            }
        }
    }
}

pub fn register_runtime_plugins(registry: &mut Registry) -> Vec<Diagnostic> {
    register_runtime_plugins_with_policy(registry, &RuntimePluginPolicy::default())
}

pub fn register_runtime_plugins_with_policy(
    registry: &mut Registry,
    policy: &RuntimePluginPolicy,
) -> Vec<Diagnostic> {
    let (plugins, mut warnings) = discover_with_policy(policy);
    for plugin in plugins {
        for capability in plugin.manifest.capabilities.clone() {
            if !policy.permits(&capability.kind) {
                continue;
            }
            match capability.kind.as_str() {
                "importer" => registry.register_importer(Arc::new(RuntimeImporter {
                    plugin: plugin.clone(),
                    capability,
                })),
                "source-enricher" => {
                    registry.register_source_enricher(Arc::new(RuntimeSourceEnricher {
                        plugin: plugin.clone(),
                        capability,
                    }))
                }
                "graph-enricher" => {
                    registry.register_graph_enricher(Arc::new(RuntimeGraphEnricher {
                        plugin: plugin.clone(),
                        capability,
                    }))
                }
                "unit-enricher" => registry.register_unit_enricher(Arc::new(RuntimeUnitEnricher {
                    plugin: plugin.clone(),
                    capability,
                })),
                "renderer" => registry.register_renderer(Arc::new(RuntimeRenderer {
                    plugin: plugin.clone(),
                    capability,
                })),
                kind => warnings.push(format!(
                    "plugin {} declares unknown capability {kind}",
                    plugin.manifest.name
                )),
            }
        }
    }
    warnings
        .into_iter()
        .map(|w| Diagnostic::new(DiagnosticCode::PluginDiscovery, w))
        .collect()
}

fn descriptor(plugin: &RuntimePlugin, cap: &RuntimeCapability) -> PluginDescriptor {
    PluginDescriptor {
        name: format!("runtime:{}", plugin.manifest.name),
        version: plugin.manifest.version.clone(),
        kind: cap.kind.clone(),
        extensions: cap.extensions.clone(),
        mime_types: cap.mime_types.clone(),
        priority: cap.priority,
    }
}

fn match_source(cap: &RuntimeCapability, source: &SourceRecord) -> ProbeScore {
    if let Some(mime) = source.detected_type.as_deref() {
        if cap.mime_types.iter().any(|m| mime_match(m, mime)) {
            return ProbeScore::MIME;
        }
    }
    let ext = source
        .path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if cap.extensions.iter().any(|x| x.eq_ignore_ascii_case(&ext)) {
        return ProbeScore::EXTENSION;
    }
    ProbeScore::NONE
}

fn mime_match(pattern: &str, mime: &str) -> bool {
    if pattern == "*/*" || pattern == mime {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return mime.starts_with(&format!("{prefix}/"));
    }
    false
}

fn invoke(plugin: &RuntimePlugin, request: &RuntimeRequest) -> Result<RuntimeResponse> {
    let token = uuid::Uuid::new_v4();
    let request_path = request
        .workspace
        .join(format!("plugin-request-{token}.json"));
    let response_path = request
        .workspace
        .join(format!("plugin-response-{token}.json"));
    fs::write(&request_path, serde_json::to_vec_pretty(request)?)?;

    let out = Command::new(&plugin.executable)
        .arg("--anytopdf-request")
        .arg(&request_path)
        .arg("--anytopdf-response")
        .arg(&response_path)
        .bounded_output(plugin.timeout)
        .with_context(|| format!("invoke plugin {}", plugin.executable.display()))?;

    if !out.status.success() {
        bail!(
            "plugin {} failed: {}",
            plugin.executable.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let metadata = fs::symlink_metadata(&response_path).context("inspect plugin response")?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PROCESS_OUTPUT {
        bail!("plugin response must be a regular file within the size limit");
    }
    let response_file = fs::File::open(&response_path).context("open plugin response")?;
    let mut bytes = Vec::new();
    response_file
        .take(MAX_PROCESS_OUTPUT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROCESS_OUTPUT {
        bail!("plugin response exceeds size limit");
    }
    let response: RuntimeResponse =
        serde_json::from_slice(&bytes).context("parse plugin response")?;
    if response.protocol != PLUGIN_PROTOCOL_VERSION {
        bail!("runtime plugin response protocol mismatch");
    }
    if !response.ok {
        bail!(
            "runtime plugin error: {}",
            response.error.unwrap_or_else(|| "unknown error".into())
        );
    }
    Ok(response)
}

#[derive(Clone)]
struct RuntimeImporter {
    plugin: RuntimePlugin,
    capability: RuntimeCapability,
}
impl Plugin for RuntimeImporter {
    fn descriptor(&self) -> PluginDescriptor {
        descriptor(&self.plugin, &self.capability)
    }
}
impl Importer for RuntimeImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        match_source(&self.capability, source)
    }
    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let response = invoke(
            &self.plugin,
            &RuntimeRequest {
                protocol: PLUGIN_PROTOCOL_VERSION,
                operation: "import".into(),
                workspace: ctx.workspace.clone(),
                source: Some(source.clone()),
                unit: None,
                graph: None,
                output: None,
            },
        )?;
        let updated = response.source.unwrap_or_else(|| source.clone());
        validate_source_identity(&source, &updated)?;
        let mut units = response.units;
        for unit in &mut units {
            if unit.source_id.is_nil() {
                unit.source_id = source.id;
            }
            if unit.source_id != source.id {
                bail!("plugin unit references another source");
            }
            validate_visual_path(ctx, &source, unit)?;
        }
        Ok(ImportOutcome {
            source: updated,
            units,
            warnings: plugin_warnings(response.warnings),
        })
    }
}

#[derive(Clone)]
struct RuntimeSourceEnricher {
    plugin: RuntimePlugin,
    capability: RuntimeCapability,
}
impl Plugin for RuntimeSourceEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        descriptor(&self.plugin, &self.capability)
    }
}
impl SourceEnricher for RuntimeSourceEnricher {
    fn supports(&self, source: &SourceRecord) -> bool {
        match_source(&self.capability, source) > ProbeScore::NONE
            || (self.capability.extensions.is_empty() && self.capability.mime_types.is_empty())
    }
    fn enrich_source(&self, ctx: &JobContext, source: &mut SourceRecord) -> Result<Vec<String>> {
        let response = invoke(
            &self.plugin,
            &RuntimeRequest {
                protocol: PLUGIN_PROTOCOL_VERSION,
                operation: "source-enrich".into(),
                workspace: ctx.workspace.clone(),
                source: Some(source.clone()),
                unit: None,
                graph: None,
                output: None,
            },
        )?;
        if let Some(updated) = response.source {
            validate_source_identity(source, &updated)?;
            *source = updated;
        }
        Ok(plugin_warnings(response.warnings))
    }
}

#[derive(Clone)]
struct RuntimeGraphEnricher {
    plugin: RuntimePlugin,
    capability: RuntimeCapability,
}
impl Plugin for RuntimeGraphEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        descriptor(&self.plugin, &self.capability)
    }
}
impl GraphEnricher for RuntimeGraphEnricher {
    fn enrich_graph(&self, ctx: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>> {
        let response = invoke(
            &self.plugin,
            &RuntimeRequest {
                protocol: PLUGIN_PROTOCOL_VERSION,
                operation: "graph-enrich".into(),
                workspace: ctx.workspace.clone(),
                source: None,
                unit: None,
                graph: Some(graph.clone()),
                output: None,
            },
        )?;
        if let Some(updated) = response.graph {
            updated.validate()?;
            for source in &graph.sources {
                let retained = updated
                    .source(source.id)
                    .context("graph enricher removed a source")?;
                validate_source_identity(source, retained)?;
            }
            for unit in &updated.units {
                let source = updated
                    .source(unit.source_id)
                    .context("graph unit has unknown source")?;
                validate_visual_path(ctx, source, unit)?;
            }
            *graph = updated;
        }
        Ok(plugin_warnings(response.warnings))
    }
}

#[derive(Clone)]
struct RuntimeUnitEnricher {
    plugin: RuntimePlugin,
    capability: RuntimeCapability,
}
impl Plugin for RuntimeUnitEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        descriptor(&self.plugin, &self.capability)
    }
}
impl UnitEnricher for RuntimeUnitEnricher {
    fn supports(&self, graph: &DocumentGraph, unit: &Unit) -> bool {
        (self.capability.extensions.is_empty() && self.capability.mime_types.is_empty())
            || graph
                .source(unit.source_id)
                .is_some_and(|s| match_source(&self.capability, s) > ProbeScore::NONE)
    }
    fn enrich_unit(
        &self,
        ctx: &JobContext,
        graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>> {
        let source = graph.source(unit.source_id).cloned();
        let response = invoke(
            &self.plugin,
            &RuntimeRequest {
                protocol: PLUGIN_PROTOCOL_VERSION,
                operation: "unit-enrich".into(),
                workspace: ctx.workspace.clone(),
                source,
                unit: Some(unit.clone()),
                graph: None,
                output: None,
            },
        )?;
        if let Some(updated) = response.unit {
            if updated.id != unit.id || updated.source_id != unit.source_id {
                bail!("unit enricher must preserve unit and source identities");
            }
            if let Some(source) = graph.source(unit.source_id) {
                validate_visual_path(ctx, source, &updated)?;
            }
            *unit = updated;
        } else {
            unit.annotations.extend(response.annotations);
        }
        Ok(plugin_warnings(response.warnings))
    }
}

#[derive(Clone)]
struct RuntimeRenderer {
    plugin: RuntimePlugin,
    capability: RuntimeCapability,
}
impl Plugin for RuntimeRenderer {
    fn descriptor(&self) -> PluginDescriptor {
        descriptor(&self.plugin, &self.capability)
    }
}
impl Renderer for RuntimeRenderer {
    fn render(
        &self,
        ctx: &JobContext,
        graph: &DocumentGraph,
        output: &Path,
    ) -> Result<RenderReport> {
        let response = invoke(
            &self.plugin,
            &RuntimeRequest {
                protocol: PLUGIN_PROTOCOL_VERSION,
                operation: "render".into(),
                workspace: ctx.workspace.clone(),
                source: None,
                unit: None,
                graph: Some(graph.clone()),
                output: Some(output.to_path_buf()),
            },
        )?;
        response.render_report.ok_or_else(|| {
            anyhow::anyhow!(
                "runtime renderer {} returned no render_report",
                self.plugin.manifest.name
            )
        })
    }
}

fn validate_source_identity(original: &SourceRecord, updated: &SourceRecord) -> Result<()> {
    if updated.id != original.id || updated.path != original.path {
        bail!("plugin must preserve source identity and path");
    }
    Ok(())
}

fn validate_visual_path(ctx: &JobContext, source: &SourceRecord, unit: &Unit) -> Result<()> {
    if let Some(path) = &unit.visual_path {
        let canonical = path
            .canonicalize()
            .context("plugin visual path does not exist")?;
        let workspace = ctx.workspace.canonicalize()?;
        let original = source.path.canonicalize()?;
        if canonical != original && !canonical.starts_with(workspace) {
            bail!("plugin visual path must be the source or inside the job workspace");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_warning_strings_cannot_claim_builtin_codes() {
        let wrapped = plugin_warnings(vec!["[provider.missing] spoof".into(), "plain".into()]);
        let parsed: Vec<Diagnostic> = wrapped.iter().map(|w| Diagnostic::from_wire(w)).collect();
        assert_eq!(parsed.len(), 2);
        assert!(
            parsed
                .iter()
                .all(|d| d.code == DiagnosticCode::PluginWarning)
        );
        assert!(parsed[0].message.contains("[provider.missing] spoof"));
        assert!(parsed[1].message.contains("plain"));
    }

    #[test]
    fn protocol_example_can_omit_unit_ids() {
        let response: RuntimeResponse = serde_json::from_str(
            r#"{
            "protocol":1,"ok":true,"units":[{"kind":"text","visible_text":"example"}]
        }"#,
        )
        .unwrap();
        assert!(!response.units[0].id.is_nil());
        assert!(response.units[0].source_id.is_nil());
    }

    #[test]
    fn deny_takes_precedence_over_allow() {
        let policy = RuntimePluginPolicy {
            allow_capabilities: Some(["importer".into()].into()),
            deny_capabilities: ["importer".into()].into(),
            ..Default::default()
        };
        assert!(!policy.permits("importer"));
        assert!(!policy.permits("renderer"));
    }

    #[test]
    fn disabled_discovery_does_not_execute_plugins() {
        let policy = RuntimePluginPolicy {
            enabled: false,
            ..Default::default()
        };
        let (plugins, warnings) = discover_with_policy(&policy);
        assert!(plugins.is_empty() && warnings.is_empty());
    }

    #[test]
    fn mime_wildcards_respect_type_boundaries() {
        assert!(mime_match("image/*", "image/png"));
        assert!(!mime_match("image/*", "imagex/png"));
        assert!(!mime_match("audio/*", "video/mp4"));
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(directory: &Path, body: &str) -> PathBuf {
        let path = directory.join("anytopdf-plugin-test");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn runtime_importer_assigns_source_and_unit_ids() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("input.example");
        fs::write(&source_path, "input").unwrap();
        let executable = script(
            dir.path(),
            r#"printf '%s' '{"protocol":1,"ok":true,"units":[{"kind":"text","visible_text":"plugin content"}]}' > "$4""#,
        );
        let capability = RuntimeCapability {
            kind: "importer".into(),
            extensions: vec!["example".into()],
            mime_types: vec![],
            priority: 1,
        };
        let plugin = RuntimePlugin {
            executable,
            timeout: Duration::from_secs(2),
            manifest: RuntimePluginManifest {
                protocol: 1,
                name: "test".into(),
                version: "1".into(),
                capabilities: vec![capability.clone()],
            },
        };
        let source = SourceRecord::new(source_path);
        let id = source.id;
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let outcome = RuntimeImporter { plugin, capability }
            .import(&ctx, source)
            .unwrap();
        assert_eq!(outcome.units[0].source_id, id);
        assert!(!outcome.units[0].id.is_nil());
        assert_eq!(
            outcome.units[0].visible_text.as_deref(),
            Some("plugin content")
        );
    }

    #[test]
    fn manifest_timeout_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let path = script(dir.path(), "exec sleep 10");
        assert!(read_manifest_with_timeout(&path, Duration::from_millis(50)).is_err());
    }

    #[test]
    fn visual_paths_cannot_escape_workspace_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.txt");
        fs::write(&source_path, "source").unwrap();
        let secret = outside.path().join("private.png");
        fs::write(&secret, "private").unwrap();
        let alias = dir.path().join("alias.png");
        std::os::unix::fs::symlink(secret, &alias).unwrap();
        let source = SourceRecord::new(source_path);
        let unit = Unit::visual(source.id, alias);
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        assert!(validate_visual_path(&ctx, &source, &unit).is_err());
    }
}
