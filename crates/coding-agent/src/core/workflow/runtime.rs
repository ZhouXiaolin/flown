use std::sync::Arc;

use async_trait::async_trait;
use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Ctx, Exception, Promise, Value,
    context::intrinsic, function::Async, prelude::Func,
};
use serde_json::json;

use crate::core::workflow::metadata::{
    WorkflowDescriptor, WorkflowMetadataError, extract_workflow_metadata,
};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowAgentOptions {
    pub label: String,
    pub phase: Option<String>,
    pub schema: Option<serde_json::Value>,
    pub model: Option<String>,
    pub isolation: Option<String>,
    pub agent_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowAgentRequest {
    pub prompt: String,
    pub options: WorkflowAgentOptions,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowAgentResult {
    pub output: serde_json::Value,
}

#[async_trait(?Send)]
pub trait WorkflowAgentExecutor {
    async fn run_agent(&self, request: WorkflowAgentRequest) -> WorkflowAgentResult;
}

pub type SharedWorkflowAgentExecutor = Arc<dyn WorkflowAgentExecutor>;

pub struct PlaceholderWorkflowAgentExecutor;

#[async_trait(?Send)]
impl WorkflowAgentExecutor for PlaceholderWorkflowAgentExecutor {
    async fn run_agent(&self, request: WorkflowAgentRequest) -> WorkflowAgentResult {
        WorkflowAgentResult {
            output: json!({
                "label": request.options.label,
                "summary": format!("placeholder agent result for `{}`", request.options.label),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRun {
    pub descriptor: WorkflowDescriptor,
    pub result: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowRuntimeError {
    #[error(transparent)]
    Metadata(#[from] WorkflowMetadataError),
    #[error("invalid workflow script: {0}")]
    Script(String),
    #[error("workflow result is not JSON serializable")]
    NonJsonResult,
    #[error("invalid workflow result: {0}")]
    InvalidResult(#[from] serde_json::Error),
}

pub async fn run_workflow_source(
    source: &str,
    args: serde_json::Value,
) -> Result<WorkflowRun, WorkflowRuntimeError> {
    run_workflow_source_with_executor(source, args, Arc::new(PlaceholderWorkflowAgentExecutor))
        .await
}

pub async fn run_workflow_source_with_executor(
    source: &str,
    args: serde_json::Value,
    executor: SharedWorkflowAgentExecutor,
) -> Result<WorkflowRun, WorkflowRuntimeError> {
    let descriptor = extract_workflow_metadata(source)?;
    let runtime =
        AsyncRuntime::new().map_err(|error| WorkflowRuntimeError::Script(error.to_string()))?;
    runtime.set_memory_limit(RUNTIME_MEMORY_LIMIT_BYTES).await;
    runtime.set_max_stack_size(RUNTIME_STACK_LIMIT_BYTES).await;

    let context = AsyncContext::builder()
        .with::<(intrinsic::Eval, intrinsic::Json, intrinsic::Promise)>()
        .build_async(&runtime)
        .await
        .map_err(|error| WorkflowRuntimeError::Script(error.to_string()))?;

    let wrapped = wrap_for_execution(source, args)?;

    let result = context
        .async_with(async |ctx| {
            let globals = ctx.globals();

            let executor = executor.clone();
            globals
                .set(
                    "__flown_agent__",
                    Func::from(Async(move |prompt: String, opts: String| {
                        let executor = executor.clone();
                        async move {
                            let request = parse_agent_request(prompt, opts);
                            let result = executor.run_agent(request).await;
                            serde_json::to_string(&result.output).unwrap_or_else(|_| "null".into())
                        }
                    })),
                )
                .map_err(|e| WorkflowRuntimeError::Script(e.to_string()))?;

            globals
                .set("pipeline", Func::from(|value: String| value))
                .map_err(|e| WorkflowRuntimeError::Script(e.to_string()))?;
            globals
                .set("parallel", Func::from(|value: String| value))
                .map_err(|e| WorkflowRuntimeError::Script(e.to_string()))?;

            let promise: Promise = ctx.eval(wrapped.as_str()).catch(&ctx).map_err(|error| {
                WorkflowRuntimeError::Script(format_caught_js_error(&ctx, error))
            })?;
            let result_json: String = promise.into_future().await.catch(&ctx).map_err(|error| {
                WorkflowRuntimeError::Script(format_caught_js_error(&ctx, error))
            })?;

            if result_json == "undefined" {
                return Err(WorkflowRuntimeError::NonJsonResult);
            }
            serde_json::from_str(&result_json).map_err(WorkflowRuntimeError::InvalidResult)
        })
        .await?;

    Ok(WorkflowRun { descriptor, result })
}

fn format_caught_js_error<'js>(ctx: &Ctx<'js>, error: CaughtError<'js>) -> String {
    match error {
        CaughtError::Exception(exception) => format_js_exception(&exception),
        CaughtError::Value(value) => {
            format!(
                "JavaScript threw a non-Error value: {}",
                format_js_value(ctx, value)
            )
        }
        CaughtError::Error(error) => error.to_string(),
    }
}

fn format_js_exception(exception: &Exception<'_>) -> String {
    let message = exception
        .message()
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| "JavaScript exception".to_string());
    match exception.stack().filter(|stack| !stack.trim().is_empty()) {
        Some(stack) => format!("{message}\nstack:\n{stack}"),
        None => message,
    }
}

fn format_js_value<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> String {
    ctx.json_stringify(value.clone())
        .ok()
        .and_then(|value| value.and_then(|value| value.to_string().ok()))
        .unwrap_or_else(|| format!("{value:?}"))
}

fn parse_agent_request(prompt: String, options_json: String) -> WorkflowAgentRequest {
    let options_json: serde_json::Value = serde_json::from_str(&options_json).unwrap_or_default();
    let label = options_json
        .get("label")
        .and_then(|value| value.as_str())
        .unwrap_or("agent")
        .to_string();
    let options = WorkflowAgentOptions {
        label,
        phase: options_json
            .get("phase")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        schema: options_json.get("schema").cloned(),
        model: options_json
            .get("model")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        isolation: options_json
            .get("isolation")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        agent_type: options_json
            .get("agentType")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
    };
    WorkflowAgentRequest { prompt, options }
}

fn wrap_for_execution(
    source: &str,
    args: serde_json::Value,
) -> Result<String, WorkflowRuntimeError> {
    let body = source.replace("export const meta = ", "const meta = ");
    let args = serde_json::to_string(&args)?;
    Ok(format!(
        r#"(async () => {{
const args = JSON.parse({args:?});
{RUNTIME_HELPERS}
const __flown_result__ = await (async () => {{
{body}
}})();
return JSON.stringify(__flown_result__);
}})();"#
    ))
}

const RUNTIME_HELPERS: &str = r#"
const agent = async (prompt, opts = {}) =>
    JSON.parse(await __flown_agent__(prompt, JSON.stringify(opts || {})));
const phase = () => {};
const log = () => {};
const budget = {
  total: null,
  spent: () => 0,
  remaining: () => Infinity,
};
const workflow = async () => {
  throw new Error('nested workflow is unsupported in this runtime');
};
const pipeline = async (items, ...stages) => {
  if (!Array.isArray(items) || !stages.length) {
    const legacySteps = items || [];
    let result = undefined;
    for (const step of legacySteps) {
      try {
        result = typeof step === 'function' ? await step(result) : await step;
      } catch (_) {
        return null;
      }
    }
    return result;
  }
  return Promise.all(items.map(async (item, index) => {
    let result = item;
    try {
      for (const stage of stages) {
        result = typeof stage === 'function' ? await stage(result, item, index) : await stage;
      }
      return result;
    } catch (_) {
      return null;
    }
  }));
};
const parallel = async (steps) => Promise.all(steps.map(async (step) => {
  try {
    return typeof step === 'function' ? await step() : await step;
  } catch (_) {
    return null;
  }
}));
"#;

const RUNTIME_MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const RUNTIME_STACK_LIMIT_BYTES: usize = 1024 * 1024;
