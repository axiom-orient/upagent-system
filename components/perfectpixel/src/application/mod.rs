mod asset_codec;
mod cli;
mod generation;
mod generation_adapter;
mod handlers;
mod path;
mod shared;

pub use crate::{PpError, PpResult};
pub use shared::ApplicationOutput;

use crate::{Operation, OperationSpec};
use cli::CliInput;
use shared::{operation_phase, render_result};

const HELP: &str = concat!(
    "perfectpixel ",
    env!("CARGO_PKG_VERSION"),
    r#"

PRODUCT
  deterministic asset compiler for AI-generated or authored local assets.
  It does not generate images or run AI models; external inference is only a candidate-producing Effect.
  No network is required. Successful managed outputs are evaluated before atomic publication.

USAGE
  perfectpixel schema
  perfectpixel agent-schema
  perfectpixel agent-inspect --request <agent-inspect-request.json>
  perfectpixel agent-extract --request <agent-extract-request.json> --out-dir <dir>
  perfectpixel agent-render --request <agent-render-request.json> --out-dir <dir>
  perfectpixel agent-compare --request <agent-compare-request.json> --out-dir <dir>
  perfectpixel inspect <input.png|jpg|jpeg|webp>
  perfectpixel convert <input> --out <output> [--width <n>] [--height <n>] [--filter nearest|lanczos3] [--jpeg-quality <1..100>] [--background <#RRGGBB>]
  perfectpixel upscale <input> --out <output> --scale <n>=2 [--filter nearest|lanczos3] [--jpeg-quality <1..100>] [--background <#RRGGBB>]
  perfectpixel edit --request <edit-request.json>
  perfectpixel psd --request <psd-export-request.json>
  perfectpixel document-psd --request <document-psd-request.json>
  perfectpixel chroma-plan --request <chroma-plan-request.json>
  perfectpixel normalize --request <normalize-request.json> --out-dir <dir>
  perfectpixel bundle --request <sprite-request.json> --out-dir <dir>
  perfectpixel texture-compile --request <texture-request.json>
  perfectpixel vector <input.png|jpg|jpeg|webp> --out <output.svg> [--preset auto|pixel-art|exact-grid|flat-icon|line-art|bounded-illustration] [--profile compact|motion-structure-ready] [--detail auto|1|2|3|4|5] [--min-quality <0..1>] [--max-quality-loss <0..1>] [--max-paths <positive integer>] [--policy <vector-policy.json>] [--report <evaluation.json>] [--diagnostics <dir>]
  perfectpixel vector-analyze <input.png|jpg|jpeg|webp> [--preset auto|pixel-art|exact-grid|flat-icon|line-art|bounded-illustration] [--profile compact|motion-structure-ready] [--policy <vector-policy.json>] [--report <analysis.json>]
  perfectpixel vision-foreground-instances --request <vision-request.json>
  perfectpixel motion-scaffold <input.svg> --out-dir <dir>
  perfectpixel motion-build --request <motion-request.json> --out-dir <dir>
  vector is the sole raster-to-SVG quality-gated publication command
  motion-build may publish derived animated.svg from an accepted raster-free SVG source
"#
);

/// Human CLI adapter. Parsing owns syntax only; semantic execution starts from `Operation`.
pub fn execute_cli(args: Vec<String>) -> ApplicationOutput {
    match cli::parse(&args) {
        Ok(CliInput::Help) => ApplicationOutput::from_text(HELP.to_string(), 0),
        Ok(CliInput::AgentSchema) => {
            render_result(handlers::agent_schema(), "image.agent_schema", "asset")
        }
        Ok(CliInput::AgentInspect { request }) => render_result(
            handlers::agent_inspect(&request),
            "image.agent_inspect",
            "asset",
        ),
        Ok(CliInput::AgentExtract {
            request,
            output_dir,
        }) => render_result(
            handlers::agent_extract(&request, &output_dir),
            "image.agent_extract",
            "asset",
        ),
        Ok(CliInput::AgentRender {
            request,
            output_dir,
        }) => render_result(
            handlers::agent_render(&request, &output_dir),
            "image.agent_render",
            "asset",
        ),
        Ok(CliInput::AgentCompare {
            request,
            output_dir,
        }) => render_result(
            handlers::agent_compare(&request, &output_dir),
            "image.agent_compare",
            "asset",
        ),
        Ok(CliInput::Operation(operation)) => execute_operation(operation),
        Err(error) => render_result(Err(error), "cli.parse", "cli"),
    }
}

/// Single application dispatcher shared by every transport.
pub(crate) fn execute_operation(operation: Operation) -> ApplicationOutput {
    execute_operation_with_request_digest(operation, None)
}

pub(crate) fn execute_operation_with_request_digest(
    operation: Operation,
    request_sha256: Option<String>,
) -> ApplicationOutput {
    let spec = operation.spec();
    let phase = operation_phase(spec.name);
    render_result(
        dispatch(operation, spec, request_sha256.as_deref()),
        spec.name,
        phase,
    )
}

/// Renders a transport-normalized semantic input error through the same product error taxonomy as
/// an executing Operation. Transport-specific root/protocol failures stay outside this boundary.
pub(crate) fn execute_error(error: PpError, operation: &'static str) -> ApplicationOutput {
    render_result(Err(error), operation, operation_phase(operation))
}

fn dispatch(
    operation: Operation,
    spec: OperationSpec,
    request_sha256: Option<&str>,
) -> PpResult<String> {
    match operation {
        Operation::Schema => handlers::schema(),
        Operation::Inspect { input } => handlers::inspect(input),
        Operation::Convert {
            input,
            output,
            width,
            height,
            filter,
            jpeg_quality,
            background,
        } => handlers::convert(
            input,
            output,
            width,
            height,
            filter,
            jpeg_quality,
            background,
        ),
        Operation::Upscale {
            input,
            output,
            scale,
            filter,
            jpeg_quality,
            background,
        } => handlers::upscale(input, output, scale, filter, jpeg_quality, background),
        Operation::Edit { request } => handlers::edit(request),
        Operation::ExportPsd { request } => handlers::export_psd(request),
        Operation::CompileDocumentPsd { request } => handlers::compile_document_psd(request),
        Operation::ChromaPlan { request } => handlers::chroma_plan(request),
        Operation::NormalizeSprite {
            request,
            output_dir,
        } => handlers::normalize(request, output_dir, request_sha256),
        Operation::CompileSprite {
            request,
            output_dir,
        } => handlers::bundle(request, output_dir, request_sha256),
        Operation::CompileTexture { request } => {
            handlers::texture_compile(request, external_timeout(spec))
        }
        Operation::CompileVector {
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
        } => handlers::vector_compile(
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
        ),
        Operation::AnalyzeVector {
            input,
            preset,
            profile,
            policy,
            report,
        } => handlers::vector_analyze(input, preset, profile, policy, report),
        Operation::AppleVisionForegroundInstances { request } => {
            handlers::vision_foreground_instances(request, external_timeout(spec))
        }
        Operation::ScaffoldMotion { input, output_dir } => {
            handlers::motion_scaffold(input, output_dir)
        }
        Operation::CompileMotion {
            request,
            output_dir,
        } => handlers::motion_build(request, output_dir, request_sha256),
    }
}

fn external_timeout(spec: OperationSpec) -> std::time::Duration {
    spec.timeout
        .expect("external process operation must declare a timeout in operation metadata")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn schema_rejects_extra_cli_args() {
        assert_ne!(execute_cli(args(&["schema", "extra"])).exit_code, 0);
    }

    #[test]
    fn unknown_legacy_command_is_not_reachable() {
        assert_ne!(
            execute_cli(args(&[
                "pack-views",
                "--request",
                "a.json",
                "--out-dir",
                "out"
            ]))
            .exit_code,
            0
        );
    }

    #[test]
    fn operation_executes_without_transport_roundtrip() {
        let output = execute_operation(Operation::Schema);
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("deterministic-asset-compiler"));
    }

    #[test]
    fn semantic_error_rendering_preserves_operation() {
        let output = execute_error(
            PpError::InvalidOption("bad scalar".to_string()),
            "image.convert",
        );
        let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).expect("json");
        assert_eq!(value["failure"]["operation"], "image.convert");
        assert_eq!(value["failure"]["code"], "invalid_argument");
    }

    #[test]
    fn cli_parse_failure_has_structured_operation_failure() {
        let output = execute_cli(args(&["convert"]));
        assert_ne!(output.exit_code, 0);
        let value: serde_json::Value = serde_json::from_str(output.stdout.trim()).expect("json");
        assert_eq!(value["failure"]["operation"], "cli.parse");
        assert_eq!(value["failure"]["context"][0]["key"], "phase");
    }
}
