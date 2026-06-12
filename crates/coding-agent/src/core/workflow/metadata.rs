use rquickjs::{Context, Runtime, context::intrinsic};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPhaseMeta {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDescriptor {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub phases: Vec<WorkflowPhaseMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mermaid: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowMetadataError {
    #[error("workflow meta export not found")]
    MissingMeta,
    #[error("invalid workflow meta: {0}")]
    InvalidMetaScript(String),
    #[error("invalid workflow descriptor: {0}")]
    InvalidDescriptor(String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWorkflowDescriptor {
    name: Option<String>,
    description: Option<String>,
    when_to_use: Option<String>,
    #[serde(default)]
    phases: Vec<WorkflowPhaseMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mermaid: Option<String>,
}

pub fn extract_workflow_metadata(
    source: &str,
) -> Result<WorkflowDescriptor, WorkflowMetadataError> {
    let runtime =
        Runtime::new().map_err(|e| WorkflowMetadataError::InvalidMetaScript(e.to_string()))?;
    runtime.set_memory_limit(META_MEMORY_LIMIT_BYTES);
    runtime.set_max_stack_size(META_STACK_LIMIT_BYTES);

    let context = Context::builder()
        .with::<(intrinsic::Eval, intrinsic::Json, intrinsic::Promise)>()
        .build(&runtime)
        .map_err(|e| WorkflowMetadataError::InvalidMetaScript(e.to_string()))?;

    let wrapped = wrap_for_extraction(source);

    context.with(|ctx| {
        ctx.eval::<(), _>(STUB_DEFINITIONS)
            .map_err(|e| WorkflowMetadataError::InvalidMetaScript(format!("{e:?}")))?;

        ctx.eval::<(), _>(wrapped.as_str())
            .map_err(|e| WorkflowMetadataError::InvalidMetaScript(format!("{e:?}")))?;

        let meta_json: String = ctx
            .eval("typeof globalThis.__flown_meta__ === 'undefined' ? 'undefined' : JSON.stringify(globalThis.__flown_meta__)")
            .map_err(|e| WorkflowMetadataError::InvalidMetaScript(format!("{e:?}")))?;

        if meta_json == "undefined" || meta_json.is_empty() {
            return Err(WorkflowMetadataError::MissingMeta);
        }

        let raw: RawWorkflowDescriptor = serde_json::from_str(&meta_json)
            .map_err(|e| WorkflowMetadataError::InvalidDescriptor(e.to_string()))?;

        let name = required_field(raw.name, "name")?;
        let description = required_field(raw.description, "description")?;
        if raw.phases.iter().any(|p| p.title.trim().is_empty()) {
            return Err(WorkflowMetadataError::InvalidDescriptor(
                "phase title cannot be empty".to_string(),
            ));
        }

        Ok(WorkflowDescriptor {
            name,
            description,
            when_to_use: raw.when_to_use,
            phases: raw.phases,
            mermaid: raw.mermaid,
        })
    })
}

fn wrap_for_extraction(source: &str) -> String {
    let body = source.replace(
        "export const meta = ",
        "const meta = globalThis.__flown_meta__ = ",
    );
    format!("(async () => {{\n{body}\n}})();")
}

fn required_field(value: Option<String>, name: &str) -> Result<String, WorkflowMetadataError> {
    let value = value.ok_or_else(|| {
        WorkflowMetadataError::InvalidDescriptor(format!("missing required field `{name}`"))
    })?;
    if value.trim().is_empty() {
        return Err(WorkflowMetadataError::InvalidDescriptor(format!(
            "`{name}` cannot be empty"
        )));
    }
    Ok(value)
}

const META_MEMORY_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const META_STACK_LIMIT_BYTES: usize = 256 * 1024;

const STUB_DEFINITIONS: &str = r#"
function phase() {}
function agent() { return Promise.resolve({}); }
function log() {}
function pipeline() { return Promise.resolve({}); }
function parallel(thunks) { return Promise.resolve(thunks.map(() => ({}))); }
"#;
