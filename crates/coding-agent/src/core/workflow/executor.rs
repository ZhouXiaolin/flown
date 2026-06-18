use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use flown_agent::AgentHarness;
use flown_ai::{AssistantContent, AssistantMessage};

use crate::core::types::WorkflowSource;
use crate::core::workflow::metadata::{WorkflowDescriptor, extract_workflow_metadata};
use crate::core::workflow::{runtime, storage};

pub type WorkflowHarnessFactory = Arc<dyn Fn(String) -> Arc<AgentHarness> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct WorkflowResolvedSource {
    pub descriptor: WorkflowDescriptor,
    pub source: String,
    pub result_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WorkflowCompletedRun {
    pub descriptor: WorkflowDescriptor,
    pub result: serde_json::Value,
    pub result_path: PathBuf,
}

pub fn resolve_workflow_source(
    workflow: WorkflowSource,
    workflows_dir: &Path,
) -> Result<WorkflowResolvedSource, String> {
    let (source, result_path) = match workflow {
        WorkflowSource::Path { path } => {
            let path = PathBuf::from(path);
            let source = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
            (source, storage::result_path_for_draft(&path))
        }
        WorkflowSource::Inline { code } => {
            let draft_path = storage::write_workflow_draft(
                workflows_dir,
                "inline-workflow",
                "Inline workflow",
                &code,
            )
            .map_err(|error| error.to_string())?;
            (code, storage::result_path_for_draft(&draft_path))
        }
        WorkflowSource::Named { name } => {
            let path = find_named_workflow(workflows_dir, &name)?;
            let source = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
            (source, storage::result_path_for_draft(&path))
        }
    };

    let descriptor = extract_workflow_metadata(&source).map_err(|error| error.to_string())?;

    Ok(WorkflowResolvedSource {
        descriptor,
        source,
        result_path,
    })
}

pub async fn execute_workflow(
    resolved: WorkflowResolvedSource,
    args: serde_json::Value,
    harness_factory: WorkflowHarnessFactory,
) -> Result<WorkflowCompletedRun, String> {
    let descriptor = resolved.descriptor.clone();
    let source = resolved.source;
    let result_path = resolved.result_path.clone();

    let run = tokio::task::spawn_blocking(move || {
        let executor = WorkflowHarnessExecutor::new(harness_factory);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime
            .block_on(runtime::run_workflow_source_with_executor(
                &source,
                args,
                Arc::new(executor),
            ))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;

    storage::write_workflow_result(&result_path, &run.result).map_err(|error| error.to_string())?;

    Ok(WorkflowCompletedRun {
        descriptor,
        result: run.result,
        result_path,
    })
}

fn find_named_workflow(workflows_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let exact_js = workflows_dir.join(format!("{name}.js"));
    if exact_js.exists() {
        return Ok(exact_js);
    }

    let wanted_slug = storage::workflow_slug(name);
    let entries = std::fs::read_dir(workflows_dir).map_err(|error| error.to_string())?;
    let mut matches = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("js"))
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    let stem_slug = storage::workflow_slug(stem);
                    stem == name
                        || stem == wanted_slug
                        || stem_slug == wanted_slug
                        || stem.ends_with(&format!("-{wanted_slug}"))
                        || stem_slug.ends_with(&format!("-{wanted_slug}"))
                })
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
        .pop()
        .ok_or_else(|| format!("workflow not found: {name}"))
}

struct WorkflowHarnessExecutor {
    harness_factory: WorkflowHarnessFactory,
    next_id: AtomicU64,
}

impl WorkflowHarnessExecutor {
    fn new(harness_factory: WorkflowHarnessFactory) -> Self {
        Self {
            harness_factory,
            next_id: AtomicU64::new(1),
        }
    }

    fn next_session_id(&self, label: &str) -> String {
        let index = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("workflow-{}-{index}", storage::workflow_slug(label))
    }
}

#[async_trait::async_trait(?Send)]
impl runtime::WorkflowAgentExecutor for WorkflowHarnessExecutor {
    async fn run_agent(
        &self,
        request: runtime::WorkflowAgentRequest,
    ) -> runtime::WorkflowAgentResult {
        let harness = (self.harness_factory)(self.next_session_id(&request.options.label));
        run_workflow_harness_agent(harness, request).await
    }
}

async fn run_workflow_harness_agent(
    harness: Arc<AgentHarness>,
    request: runtime::WorkflowAgentRequest,
) -> runtime::WorkflowAgentResult {
    let prompt = build_agent_prompt(&request);
    let result = harness.prompt(&prompt, None).await;
    match result {
        Ok(message) => {
            let text = assistant_text(&message);
            let output = match &request.options.schema {
                Some(schema) => extract_json_output(&text, schema),
                None => serde_json::json!({
                    "label": request.options.label,
                    "text": text,
                }),
            };
            runtime::WorkflowAgentResult { output }
        }
        Err(error) => runtime::WorkflowAgentResult {
            output: serde_json::json!({
                "label": request.options.label,
                "error": error.to_string(),
            }),
        },
    }
}

fn build_agent_prompt(request: &runtime::WorkflowAgentRequest) -> String {
    let mut prompt = request.prompt.clone();
    if let Some(schema) = &request.options.schema {
        let schema_json = serde_json::to_string_pretty(schema).unwrap_or_default();
        prompt.push_str(&format!(
            "\n\nYou MUST respond with ONLY a valid JSON object matching this schema (no markdown, no explanation, just raw JSON):\n```json\n{schema_json}\n```"
        ));
    }
    prompt
}

fn extract_json_output(text: &str, _schema: &serde_json::Value) -> serde_json::Value {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return value;
    }
    if let Some(json) = extract_json_from_fence(trimmed) {
        return json;
    }
    serde_json::json!({ "text": text })
}

fn extract_json_from_fence(text: &str) -> Option<serde_json::Value> {
    let start = text.find("```json")? + 7;
    let end = text.rfind("```")?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(text[start..end].trim()).ok()
}

fn assistant_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
