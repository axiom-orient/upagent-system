use std::{
    borrow::Cow,
    num::{NonZeroU32, NonZeroUsize},
    path::{Path, PathBuf},
    sync::Arc,
};

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ErrorCode, InitializeRequestParams, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{oneshot, Semaphore};

use crate::{
    adapters::motion::MotionRequest,
    adapters::sprite::{NormalizeRequest, SpriteBundleRequest},
    application::{execute_error, execute_operation_with_request_digest},
    parse_resample_filter, parse_srgb8_hex, parse_vector_detail, parse_vector_preset,
    parse_vector_profile, JpegQuality, Operation, PpError, ScaleFactor, SvgProfile, UnitScore,
    VectorPresetSelection,
};

use super::{
    params::{
        ConvertParams, InspectParams, MotionBuildParams, MotionScaffoldParams,
        RequestDirectoryParams, SchemaParams, UpscaleParams, VectorAnalyzeParams, VectorParams,
    },
    root::{Root, RootPathError},
};

pub(crate) const MCP_RESULT_SCHEMA: &str = "perfectpixel.mcp-tool-result/1";
const MAX_REQUEST_PREPARSE_BYTES: usize = 8 * 1024 * 1024;
pub const MCP_HELP: &str = concat!(
    "perfectpixel-mcp ",
    env!("CARGO_PKG_VERSION"),
    r#"

USAGE
  perfectpixel-mcp --root <ABSOLUTE_EXISTING_DIRECTORY>
  perfectpixel-mcp --help

The configured root is fixed for the process. MCP transport is stdio only.
"#
);

#[derive(Debug, Serialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct McpToolResultEnvelope {
    schema: &'static str,
    ok: bool,
    operation: String,
    exit_code: i32,
    result: Value,
}

#[derive(Debug)]
enum PreparationError {
    Root(RootPathError),
    Semantic(PpError),
    Invalid(String),
}

impl PreparationError {
    fn transport_message(&self) -> Option<String> {
        match self {
            Self::Root(error) => Some(error.message().to_string()),
            Self::Invalid(message) => Some(message.clone()),
            Self::Semantic(_) => None,
        }
    }
}

#[derive(Debug)]
pub enum Startup {
    Help,
    Server(PerfectPixelMcp),
}

#[derive(Debug)]
pub struct StartupError {
    message: String,
}

impl StartupError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StartupError {}

pub fn startup(args: Vec<String>) -> Result<Startup, StartupError> {
    match args.as_slice() {
        [help] if help == "--help" => Ok(Startup::Help),
        [flag, root] if flag == "--root" => {
            let root = Root::new(PathBuf::from(root)).map_err(|error| {
                StartupError::new(format!("invalid --root: {}", error.message()))
            })?;
            Ok(Startup::Server(PerfectPixelMcp::new(root)))
        }
        _ => Err(StartupError::new(
            "usage: perfectpixel-mcp --root <ABSOLUTE_EXISTING_DIRECTORY> | --help",
        )),
    }
}

#[derive(Clone)]
pub struct PerfectPixelMcp {
    root: Root,
    active_operation: Arc<Semaphore>,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for PerfectPixelMcp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PerfectPixelMcp")
            .field("root", &self.root.canonical())
            .finish_non_exhaustive()
    }
}

impl PerfectPixelMcp {
    pub(crate) fn new(root: Root) -> Self {
        Self {
            root,
            active_operation: Arc::new(Semaphore::new(1)),
            tool_router: Self::tool_router(),
        }
    }

    async fn run_operation<F>(
        &self,
        tool_name: &'static str,
        operation_name: &'static str,
        prepare: F,
    ) -> CallToolResult
    where
        F: FnOnce(&Root) -> Result<Operation, PreparationError> + Send + 'static,
    {
        let permit = match Arc::clone(&self.active_operation).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return render_result(error_envelope(
                    tool_name,
                    2,
                    json!({
                        "ok": false,
                        "code": "busy",
                        "message": "another operation is active; no queue is available",
                    }),
                ));
            }
        };

        let root = self.root.clone();
        let (sender, receiver) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let result = match prepare(&root) {
                Ok(operation) => execute_revalidated(&root, tool_name, operation, || {}),
                Err(PreparationError::Semantic(error)) => {
                    let output = execute_error(error, operation_name);
                    application_result(tool_name, output.exit_code, &output.stdout)
                }
                Err(error) => transport_preparation_result(tool_name, error),
            };
            let _ = sender.send(result);
            drop(permit);
        });

        match receiver.await {
            Ok(result) => render_result(result),
            Err(_) => render_result(error_envelope(
                tool_name,
                2,
                json!({
                    "ok": false,
                    "code": "internal",
                    "message": "internal MCP error: operation worker terminated",
                }),
            )),
        }
    }
}

fn execute_revalidated(
    root: &Root,
    tool_name: &str,
    operation: Operation,
    after_check: impl FnOnce(),
) -> McpToolResultEnvelope {
    match revalidate_operation(root, &operation) {
        Ok(request_sha256) => {
            after_check();
            let output = execute_operation_with_request_digest(operation, request_sha256);
            application_result(tool_name, output.exit_code, &output.stdout)
        }
        Err(error) => transport_preparation_result(tool_name, error),
    }
}

#[tool_router(router = tool_router)]
impl PerfectPixelMcp {
    #[tool(
        name = "perfectpixel_schema",
        description = "Return the perfectpixel operation and artifact schema.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn perfectpixel_schema(
        &self,
        Parameters(params): Parameters<SchemaParams>,
    ) -> CallToolResult {
        let _ = params;
        self.run_operation("perfectpixel_schema", "system.schema", |_| {
            Ok(Operation::Schema)
        })
        .await
    }

    #[tool(
        name = "perfectpixel_inspect",
        description = "Inspect one root-bounded raster file without writing.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn perfectpixel_inspect(
        &self,
        Parameters(params): Parameters<InspectParams>,
    ) -> CallToolResult {
        self.run_operation("perfectpixel_inspect", "image.inspect", move |root| {
            let input = root
                .input_file(&params.input_path)
                .map_err(PreparationError::Root)?;
            Ok(Operation::Inspect { input })
        })
        .await
    }

    #[tool(
        name = "perfectpixel_convert",
        description = "Convert or resize one root-bounded raster file.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_convert(
        &self,
        Parameters(params): Parameters<ConvertParams>,
    ) -> CallToolResult {
        self.run_operation("perfectpixel_convert", "image.convert", move |root| {
            prepare_convert(root, params)
        })
        .await
    }

    #[tool(
        name = "perfectpixel_upscale",
        description = "Upscale one root-bounded raster file.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_upscale(
        &self,
        Parameters(params): Parameters<UpscaleParams>,
    ) -> CallToolResult {
        self.run_operation("perfectpixel_upscale", "image.upscale", move |root| {
            prepare_upscale(root, params)
        })
        .await
    }

    #[tool(
        name = "perfectpixel_normalize",
        description = "Normalize a typed sprite request and publish its generated set under root.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_normalize(
        &self,
        Parameters(params): Parameters<RequestDirectoryParams>,
    ) -> CallToolResult {
        self.run_operation("perfectpixel_normalize", "sprite.normalize", move |root| {
            prepare_normalize(root, params)
        })
        .await
    }

    #[tool(
        name = "perfectpixel_bundle",
        description = "Build a typed sprite bundle and publish its generated set under root.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_bundle(
        &self,
        Parameters(params): Parameters<RequestDirectoryParams>,
    ) -> CallToolResult {
        self.run_operation("perfectpixel_bundle", "sprite.compile", move |root| {
            prepare_bundle(root, params)
        })
        .await
    }

    #[tool(
        name = "perfectpixel_vector",
        description = "Run the quality-gated raster-to-SVG vector operation under root.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_vector(
        &self,
        Parameters(params): Parameters<VectorParams>,
    ) -> CallToolResult {
        self.run_operation("perfectpixel_vector", "vector.compile", move |root| {
            prepare_vector(root, params)
        })
        .await
    }

    #[tool(
        name = "perfectpixel_vector_analyze",
        description = "Analyze one raster under root; reportPath, when supplied, is a managed write.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_vector_analyze(
        &self,
        Parameters(params): Parameters<VectorAnalyzeParams>,
    ) -> CallToolResult {
        self.run_operation(
            "perfectpixel_vector_analyze",
            "vector.analyze",
            move |root| prepare_vector_analyze(root, params),
        )
        .await
    }

    #[tool(
        name = "perfectpixel_motion_scaffold",
        description = "Scaffold a raster-free SVG into a generated motion workspace under root.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_motion_scaffold(
        &self,
        Parameters(params): Parameters<MotionScaffoldParams>,
    ) -> CallToolResult {
        self.run_operation(
            "perfectpixel_motion_scaffold",
            "motion.scaffold",
            move |root| {
                let input = root
                    .input_file(&params.input_path)
                    .map_err(PreparationError::Root)?;
                let output_dir = root
                    .output_dir(&params.output_dir)
                    .map_err(PreparationError::Root)?;
                Ok(Operation::ScaffoldMotion { input, output_dir })
            },
        )
        .await
    }

    #[tool(
        name = "perfectpixel_motion_build",
        description = "Build an accepted motion request and publish its generated set under root.",
        output_schema = rmcp::handler::server::common::schema_for_output::<McpToolResultEnvelope>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false)
    )]
    pub async fn perfectpixel_motion_build(
        &self,
        Parameters(params): Parameters<MotionBuildParams>,
    ) -> CallToolResult {
        self.run_operation("perfectpixel_motion_build", "motion.compile", move |root| {
            prepare_motion_build(root, params)
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PerfectPixelMcp {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[ProtocolVersion::V_2026_07_28])
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "initialize is not available in MCP 2026-07-28",
            None,
        ))
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info.with_server_info(rmcp::model::Implementation::new(
            "perfectpixel-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
    }
}

fn prepare_convert(root: &Root, params: ConvertParams) -> Result<Operation, PreparationError> {
    let input = root
        .input_file(&params.input_path)
        .map_err(PreparationError::Root)?;
    let output = root
        .output_file(&params.output_path)
        .map_err(PreparationError::Root)?;
    Ok(Operation::Convert {
        input,
        output,
        width: optional_nonzero_u32(params.width, "width")?,
        height: optional_nonzero_u32(params.height, "height")?,
        filter: params
            .filter
            .map(|value| parse_resample_filter(value.as_str()))
            .transpose()
            .map_err(semantic_input)?,
        jpeg_quality: params
            .jpeg_quality
            .map(JpegQuality::new)
            .transpose()
            .map_err(semantic_input)?,
        background: params
            .background
            .as_deref()
            .map(|raw| {
                parse_srgb8_hex(raw).ok_or_else(|| {
                    PreparationError::Semantic(PpError::InvalidOption(
                        "--background must be a #RRGGBB color".to_string(),
                    ))
                })
            })
            .transpose()?,
    })
}

fn prepare_upscale(root: &Root, params: UpscaleParams) -> Result<Operation, PreparationError> {
    let input = root
        .input_file(&params.input_path)
        .map_err(PreparationError::Root)?;
    let output = root
        .output_file(&params.output_path)
        .map_err(PreparationError::Root)?;
    Ok(Operation::Upscale {
        input,
        output,
        scale: ScaleFactor::new(params.scale).map_err(semantic_input)?,
        filter: params
            .filter
            .map(|value| parse_resample_filter(value.as_str()))
            .transpose()
            .map_err(semantic_input)?,
        jpeg_quality: params
            .jpeg_quality
            .map(JpegQuality::new)
            .transpose()
            .map_err(semantic_input)?,
        background: params
            .background
            .as_deref()
            .map(|raw| {
                parse_srgb8_hex(raw).ok_or_else(|| {
                    PreparationError::Semantic(PpError::InvalidOption(
                        "--background must be a #RRGGBB color".to_string(),
                    ))
                })
            })
            .transpose()?,
    })
}

fn prepare_normalize(
    root: &Root,
    params: RequestDirectoryParams,
) -> Result<Operation, PreparationError> {
    let request = root
        .input_file(&params.request_path)
        .map_err(PreparationError::Root)?;
    let output_dir = root
        .output_dir(&params.output_dir)
        .map_err(PreparationError::Root)?;
    Ok(Operation::NormalizeSprite {
        request,
        output_dir,
    })
}

fn prepare_bundle(
    root: &Root,
    params: RequestDirectoryParams,
) -> Result<Operation, PreparationError> {
    let request = root
        .input_file(&params.request_path)
        .map_err(PreparationError::Root)?;
    let output_dir = root
        .output_dir(&params.output_dir)
        .map_err(PreparationError::Root)?;
    Ok(Operation::CompileSprite {
        request,
        output_dir,
    })
}

fn prepare_vector(root: &Root, params: VectorParams) -> Result<Operation, PreparationError> {
    let input = root
        .input_file(&params.input_path)
        .map_err(PreparationError::Root)?;
    let output = root
        .output_file(&params.output_path)
        .map_err(PreparationError::Root)?;
    let policy = params
        .policy_path
        .as_deref()
        .map(|path| root.input_file(path).map_err(PreparationError::Root))
        .transpose()?;
    let report = params
        .report_path
        .as_deref()
        .map(|path| root.output_file(path).map_err(PreparationError::Root))
        .transpose()?;
    let diagnostics = params
        .diagnostics_dir
        .as_deref()
        .map(|path| root.output_dir(path).map_err(PreparationError::Root))
        .transpose()?;

    let preset = params
        .preset
        .map(|value| parse_vector_preset(value.as_str()))
        .transpose()
        .map_err(semantic_input)?
        .unwrap_or(VectorPresetSelection::Auto);
    let profile = params
        .profile
        .map(|value| parse_vector_profile(value.as_str()))
        .transpose()
        .map_err(semantic_input)?
        .unwrap_or(SvgProfile::Compact);
    let detail = params
        .detail
        .and_then(|value| value.as_cli_value())
        .map(parse_vector_detail)
        .transpose()
        .map_err(semantic_input)?
        .flatten();
    let minimum_quality = params
        .min_quality
        .map(UnitScore::new)
        .transpose()
        .map_err(semantic_input)?;
    let maximum_quality_loss = params
        .max_quality_loss
        .map(UnitScore::new)
        .transpose()
        .map_err(semantic_input)?;
    let maximum_paths = params
        .max_paths
        .map(|value| {
            NonZeroUsize::new(value).ok_or_else(|| {
                PreparationError::Semantic(PpError::InvalidOption(
                    "--max-paths must be a positive integer".to_string(),
                ))
            })
        })
        .transpose()?;

    Ok(Operation::CompileVector {
        input,
        output,
        preset,
        profile,
        detail,
        minimum_quality,
        maximum_quality_loss,
        maximum_paths,
        policy,
        report,
        diagnostics,
    })
}

fn prepare_vector_analyze(
    root: &Root,
    params: VectorAnalyzeParams,
) -> Result<Operation, PreparationError> {
    let input = root
        .input_file(&params.input_path)
        .map_err(PreparationError::Root)?;
    let policy = params
        .policy_path
        .as_deref()
        .map(|path| root.input_file(path).map_err(PreparationError::Root))
        .transpose()?;
    let report = params
        .report_path
        .as_deref()
        .map(|path| root.output_file(path).map_err(PreparationError::Root))
        .transpose()?;
    let preset = params
        .preset
        .map(|value| parse_vector_preset(value.as_str()))
        .transpose()
        .map_err(semantic_input)?
        .unwrap_or(VectorPresetSelection::Auto);
    let profile = params
        .profile
        .map(|value| parse_vector_profile(value.as_str()))
        .transpose()
        .map_err(semantic_input)?
        .unwrap_or(SvgProfile::Compact);
    Ok(Operation::AnalyzeVector {
        input,
        preset,
        profile,
        policy,
        report,
    })
}

fn prepare_motion_build(
    root: &Root,
    params: MotionBuildParams,
) -> Result<Operation, PreparationError> {
    let request = root
        .input_file(&params.request_path)
        .map_err(PreparationError::Root)?;
    let output_dir = root
        .output_dir(&params.output_dir)
        .map_err(PreparationError::Root)?;
    Ok(Operation::CompileMotion {
        request,
        output_dir,
    })
}

fn optional_nonzero_u32(
    value: Option<u32>,
    label: &str,
) -> Result<Option<NonZeroU32>, PreparationError> {
    value
        .map(|value| {
            NonZeroU32::new(value).ok_or_else(|| {
                PreparationError::Semantic(PpError::InvalidOption(format!(
                    "{label} must be a positive integer"
                )))
            })
        })
        .transpose()
}

fn semantic_input(error: impl std::fmt::Display) -> PreparationError {
    PreparationError::Semantic(PpError::InvalidOption(error.to_string()))
}

enum RequestPreparse<T> {
    Parsed(T),
    RejectedByTypedParser,
}

fn preparse_request<T: for<'de> Deserialize<'de>>(
    root: &Root,
    path: &Path,
) -> Result<(RequestPreparse<T>, String), PreparationError> {
    let bytes = root
        .read_bounded_regular_file(path, MAX_REQUEST_PREPARSE_BYTES)
        .map_err(PreparationError::Root)?;
    let parsed = match serde_json::from_slice(&bytes) {
        Ok(request) => Ok(RequestPreparse::Parsed(request)),
        Err(error) if error.classify() != serde_json::error::Category::Io => {
            Ok(RequestPreparse::RejectedByTypedParser)
        }
        Err(error) => Err(PreparationError::Invalid(format!(
            "request preparse failed: {error}"
        ))),
    }?;
    Ok((parsed, crate::sha256_hex(&bytes)))
}

fn revalidate_operation(
    root: &Root,
    operation: &Operation,
) -> Result<Option<String>, PreparationError> {
    let input = |path: &Path| {
        root.revalidate_input_file(path)
            .map_err(PreparationError::Root)
    };
    let output_file = |path: &Path| {
        root.revalidate_output_file(path)
            .map_err(PreparationError::Root)
    };
    let output_dir = |path: &Path| {
        root.revalidate_output_dir(path)
            .map_err(PreparationError::Root)
    };

    match operation {
        Operation::Schema => Ok(()),
        Operation::Inspect { input: path } => input(path),
        Operation::Convert {
            input: source,
            output,
            ..
        }
        | Operation::Upscale {
            input: source,
            output,
            ..
        } => {
            input(source)?;
            output_file(output)
        }
        Operation::NormalizeSprite {
            request,
            output_dir: destination,
        } => {
            let digest = revalidate_normalize_request(root, request)?;
            output_dir(destination)?;
            return Ok(Some(digest));
        }
        Operation::CompileSprite {
            request,
            output_dir: destination,
        } => {
            let digest = revalidate_bundle_request(root, request)?;
            output_dir(destination)?;
            return Ok(Some(digest));
        }
        Operation::CompileVector {
            input: source,
            output,
            policy,
            report,
            diagnostics,
            ..
        } => {
            input(source)?;
            output_file(output)?;
            if let Some(path) = policy {
                input(path)?;
            }
            if let Some(path) = report {
                output_file(path)?;
            }
            if let Some(path) = diagnostics {
                output_dir(path)?;
            }
            Ok(())
        }
        Operation::AnalyzeVector {
            input: source,
            policy,
            report,
            ..
        } => {
            input(source)?;
            if let Some(path) = policy {
                input(path)?;
            }
            if let Some(path) = report {
                output_file(path)?;
            }
            Ok(())
        }
        Operation::ScaffoldMotion {
            input: source,
            output_dir: destination,
        } => {
            input(source)?;
            output_dir(destination)
        }
        Operation::CompileMotion {
            request,
            output_dir: destination,
        } => {
            let digest = revalidate_motion_request(root, request)?;
            output_dir(destination)?;
            return Ok(Some(digest));
        }
        Operation::Edit { .. }
        | Operation::ExportPsd { .. }
        | Operation::CompileDocumentPsd { .. }
        | Operation::ChromaPlan { .. }
        | Operation::CompileTexture { .. }
        | Operation::AppleVisionForegroundInstances { .. } => Err(PreparationError::Invalid(
            "operation is not exposed by the MCP transport".to_string(),
        )),
    }?;
    Ok(None)
}

fn revalidate_normalize_request(root: &Root, request: &Path) -> Result<String, PreparationError> {
    let (parsed, digest) = preparse_request::<NormalizeRequest>(root, request)?;
    if let RequestPreparse::Parsed(request_data) = parsed {
        for state in request_data.states {
            for frame in state.frames {
                root.nested_input(request, &frame)
                    .map_err(PreparationError::Root)?;
            }
            if let Some(strip) = state.strip {
                root.nested_input(request, &strip)
                    .map_err(PreparationError::Root)?;
            }
        }
    }
    Ok(digest)
}

fn revalidate_bundle_request(root: &Root, request: &Path) -> Result<String, PreparationError> {
    let (parsed, digest) = preparse_request::<SpriteBundleRequest>(root, request)?;
    if let RequestPreparse::Parsed(request_data) = parsed {
        for state in request_data.states {
            for frame in state.frames {
                root.nested_input(request, &frame)
                    .map_err(PreparationError::Root)?;
            }
        }
    }
    Ok(digest)
}

fn revalidate_motion_request(root: &Root, request: &Path) -> Result<String, PreparationError> {
    let (parsed, digest) = preparse_request::<MotionRequest>(root, request)?;
    if let RequestPreparse::Parsed(request_data) = parsed {
        root.nested_input(request, &request_data.source_svg)
            .map_err(PreparationError::Root)?;
    }
    Ok(digest)
}

fn transport_preparation_result(tool_name: &str, error: PreparationError) -> McpToolResultEnvelope {
    let message = error
        .transport_message()
        .unwrap_or_else(|| "internal MCP preparation error".to_string());
    error_envelope(
        tool_name,
        2,
        json!({
            "ok": false,
            "code": "rootOrPathRejected",
            "message": message,
        }),
    )
}

fn application_result(tool_name: &str, exit_code: i32, stdout: &str) -> McpToolResultEnvelope {
    let result = match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(result) => result,
        Err(_) => {
            return error_envelope(
                tool_name,
                2,
                json!({
                    "ok": false,
                    "code": "internal",
                    "message": "internal MCP error: application output was not valid JSON",
                }),
            );
        }
    };
    McpToolResultEnvelope {
        schema: MCP_RESULT_SCHEMA,
        ok: exit_code == 0,
        operation: tool_name.to_string(),
        exit_code,
        result,
    }
}

fn error_envelope(tool_name: &str, exit_code: i32, result: Value) -> McpToolResultEnvelope {
    McpToolResultEnvelope {
        schema: MCP_RESULT_SCHEMA,
        ok: false,
        operation: tool_name.to_string(),
        exit_code,
        result,
    }
}

fn render_result(result: McpToolResultEnvelope) -> CallToolResult {
    let value = serde_json::to_value(result).unwrap_or_else(|_| {
        json!({
            "schema": MCP_RESULT_SCHEMA,
            "ok": false,
            "operation": "internal",
            "exitCode": 2,
            "result": {
                "ok": false,
                "code": "internal",
                "message": "internal MCP error: envelope serialization failed",
            }
        })
    });
    if value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        CallToolResult::structured(value)
    } else {
        CallToolResult::structured_error(value)
    }
}

pub async fn serve(server: PerfectPixelMcp) -> Result<(), String> {
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| error.to_string())?;
    running
        .waiting()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn temp_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "perfectpixel-mcp-server-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create root");
        path
    }

    fn envelope(result: CallToolResult) -> Value {
        result.structured_content.expect("structured MCP result")
    }

    #[test]
    fn request_swap_after_admission_cannot_read_an_unchecked_nested_input() {
        use std::os::unix::fs::symlink;
        let root_path = fs::canonicalize(temp_root("request-swap")).unwrap();
        let outside = fs::canonicalize(temp_root("request-swap-outside")).unwrap();
        let png = crate::PngEncoder::encode_rgba(
            &crate::Raster::new(1, 1, vec![255, 0, 0, 255]).unwrap(),
        )
        .unwrap();
        fs::write(root_path.join("safe.png"), &png).unwrap();
        fs::write(outside.join("secret.png"), &png).unwrap();
        symlink(outside.join("secret.png"), root_path.join("escape.png")).unwrap();
        let mut request = json!({"character": "hero", "cellWidth": 1, "cellHeight": 1,
            "states": [{"name": "idle", "fps": 8, "loop": true, "frames": ["safe.png"]}]});
        let path = root_path.join("request.json");
        fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
        let root = Root::new(root_path.clone()).unwrap();
        let output_dir = root_path.join("output");
        revalidate_operation(
            &root,
            &Operation::CompileSprite {
                request: path.clone(),
                output_dir: output_dir.clone(),
            },
        )
        .expect("original request must pass admission");
        let stable = execute_revalidated(
            &root,
            "perfectpixel_bundle",
            Operation::CompileSprite {
                request: path.clone(),
                output_dir: root_path.join("stable-output"),
            },
            || {},
        );
        assert!(
            stable.ok,
            "unchanged request must still execute: {}",
            stable.result
        );
        let result = execute_revalidated(
            &root,
            "perfectpixel_bundle",
            Operation::CompileSprite {
                request: path.clone(),
                output_dir: output_dir.clone(),
            },
            || {
                request["states"][0]["frames"][0] = json!("escape.png");
                fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
            },
        );
        assert!(!result.ok, "unchecked request executed: {}", result.result);
        assert_eq!(result.result["failure"]["code"], "precondition_failed");
        assert!(
            !output_dir.exists(),
            "unchecked input must not publish output"
        );
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn every_mcp_request_operation_enforces_the_admitted_digest() {
        let root_path = fs::canonicalize(temp_root("request-digest")).unwrap();
        let root = Root::new(root_path.clone()).unwrap();
        let request = root_path.join("request.json");
        let output_dir = root_path.join("output");
        for operation in [
            Operation::NormalizeSprite {
                request: request.clone(),
                output_dir: output_dir.clone(),
            },
            Operation::CompileSprite {
                request: request.clone(),
                output_dir: output_dir.clone(),
            },
            Operation::CompileMotion {
                request: request.clone(),
                output_dir: output_dir.clone(),
            },
        ] {
            // Even an invalid admitted document must not become a different,
            // valid request between transport validation and domain execution.
            fs::write(&request, b"{}").unwrap();
            let replacement = if matches!(operation, Operation::CompileMotion { .. }) {
                json!({"schema": "perfectpixel.motion/1", "name": "motion", "sourceSvg": "scene.svg", "sourceSvgSha256": "0".repeat(64), "fps": 30, "durationMs": 100, "loop": false, "parts": [], "tracks": []})
            } else {
                json!({"character": "hero", "cellWidth": 1, "cellHeight": 1, "states": []})
            };
            let result = execute_revalidated(&root, "test", operation, || {
                fs::write(&request, serde_json::to_vec(&replacement).unwrap()).unwrap();
            });
            assert!(!result.ok);
            assert_eq!(
                result.result["failure"]["code"], "precondition_failed",
                "{}",
                result.result
            );
            assert!(!output_dir.exists());
        }
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn startup_accepts_only_help_or_an_absolute_root() {
        assert!(matches!(
            startup(vec!["--help".to_string()]),
            Ok(Startup::Help)
        ));
        assert!(startup(vec!["--root".to_string(), "relative".to_string()]).is_err());
        assert!(startup(vec!["--root".to_string()]).is_err());
        assert!(startup(vec!["--help".to_string(), "extra".to_string()]).is_err());
    }

    #[test]
    fn revalidation_explicitly_rejects_non_mcp_operations() {
        let root_path = temp_root("not-exposed");
        let root = Root::new(root_path.clone()).expect("root");
        for operation in [
            Operation::CompileDocumentPsd {
                request: root_path.join("a.json"),
            },
            Operation::CompileTexture {
                request: root_path.join("a.json"),
            },
            Operation::AppleVisionForegroundInstances {
                request: root_path.join("a.json"),
            },
        ] {
            assert!(matches!(
                revalidate_operation(&root, &operation),
                Err(PreparationError::Invalid(_))
            ));
        }
        let _ = fs::remove_dir_all(root_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_call_is_busy_and_cancellation_keeps_the_permit() {
        let root_path = temp_root("busy");
        let root = Root::new(root_path.clone()).expect("root");
        let server = Arc::new(PerfectPixelMcp::new(root));
        let started = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));

        let first_server = Arc::clone(&server);
        let first_started = Arc::clone(&started);
        let first_finished = Arc::clone(&finished);
        let first = tokio::spawn(async move {
            first_server
                .run_operation("test", "system.schema", move |_| {
                    first_started.store(true, std::sync::atomic::Ordering::Release);
                    std::thread::sleep(Duration::from_millis(150));
                    first_finished.store(true, std::sync::atomic::Ordering::Release);
                    Ok(Operation::Schema)
                })
                .await
        });

        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        let busy = server
            .run_operation("test", "system.schema", |_| Ok(Operation::Schema))
            .await;
        let busy_value = envelope(busy);
        assert_eq!(busy_value["result"]["code"], "busy");
        assert_eq!(busy_value["ok"], false);

        first.abort();
        while !finished.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        while server.active_operation.available_permits() == 0 {
            tokio::task::yield_now().await;
        }

        let after = server
            .run_operation("test", "system.schema", |_| Ok(Operation::Schema))
            .await;
        let after_value = envelope(after);
        assert_eq!(after_value["ok"], true);

        let _ = first.await;
        let _ = fs::remove_dir_all(root_path);
    }
}
