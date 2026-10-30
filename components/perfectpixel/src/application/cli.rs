use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
};

use crate::{
    parse_resample_filter, parse_unit_score, parse_vector_detail, parse_vector_preset,
    parse_vector_profile, JpegQuality, Operation, PpError, PpResult, ScaleFactor, SvgProfile,
    VectorPresetSelection,
};

use super::asset_codec::parse_background;

pub(super) enum CliInput {
    Help,
    Operation(Operation),
    AgentSchema,
    AgentInspect {
        request: PathBuf,
    },
    AgentExtract {
        request: PathBuf,
        output_dir: PathBuf,
    },
    AgentRender {
        request: PathBuf,
        output_dir: PathBuf,
    },
    AgentCompare {
        request: PathBuf,
        output_dir: PathBuf,
    },
}

pub(super) fn parse(args: &[String]) -> PpResult<CliInput> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(CliInput::Help);
    };
    match command {
        "--help" | "-h" => {
            reject_extra(&args[1..], "help")?;
            Ok(CliInput::Help)
        }
        "schema" => {
            reject_extra(&args[1..], "schema")?;
            Ok(CliInput::Operation(Operation::Schema))
        }
        "agent-schema" => {
            reject_extra(&args[1..], "agent-schema")?;
            Ok(CliInput::AgentSchema)
        }
        "agent-inspect" => Ok(CliInput::AgentInspect {
            request: one_request_path(&args[1..])?,
        }),
        "agent-extract" => {
            let (request, output_dir) = request_directory(&args[1..])?;
            Ok(CliInput::AgentExtract { request, output_dir })
        }
        "agent-render" => {
            let (request, output_dir) = request_directory(&args[1..])?;
            Ok(CliInput::AgentRender { request, output_dir })
        }
        "agent-compare" => {
            let (request, output_dir) = request_directory(&args[1..])?;
            Ok(CliInput::AgentCompare { request, output_dir })
        }
        "inspect" => {
            if args.len() != 2 || args[1].starts_with("--") {
                return Err(PpError::InvalidOption(
                    "inspect requires exactly one <input>".to_string(),
                ));
            }
            Ok(CliInput::Operation(Operation::Inspect {
                input: PathBuf::from(&args[1]),
            }))
        }
        "convert" => parse_convert(&args[1..]),
        "upscale" => parse_upscale(&args[1..]),
        "edit" => one_request(&args[1..], |request| Operation::Edit { request }),
        "psd" => one_request(&args[1..], |request| Operation::ExportPsd { request }),
        "document-psd" => one_request(&args[1..], |request| Operation::CompileDocumentPsd { request }),
        "chroma-plan" => one_request(&args[1..], |request| Operation::ChromaPlan { request }),
        "normalize" => parse_request_directory(&args[1..], true),
        "bundle" => parse_request_directory(&args[1..], false),
        "texture-compile" => one_request(&args[1..], |request| Operation::CompileTexture { request }),
        "vector" => parse_vector(&args[1..]),
        "vector-analyze" => parse_vector_analyze(&args[1..]),
        "vision-foreground-instances" => one_request(&args[1..], |request| {
            Operation::AppleVisionForegroundInstances { request }
        }),
        "motion-scaffold" => parse_motion_scaffold(&args[1..]),
        "motion-build" => parse_motion_build(&args[1..]),
        other => Err(PpError::InvalidOption(format!(
            "unknown command '{other}'; use schema, agent-schema, agent-inspect, agent-extract, agent-render, agent-compare, inspect, convert, upscale, edit, psd, document-psd, chroma-plan, vector, vector-analyze, normalize, bundle, texture-compile, vision-foreground-instances, motion-scaffold, or motion-build"
        ))),
    }
}

fn one_request_path(args: &[String]) -> PpResult<PathBuf> {
    let values = options(args, &["--request"])?;
    required_path(&values, "--request")
}

fn request_directory(args: &[String]) -> PpResult<(PathBuf, PathBuf)> {
    let values = options(args, &["--request", "--out-dir"])?;
    Ok((
        required_path(&values, "--request")?,
        required_path(&values, "--out-dir")?,
    ))
}

fn parse_convert(args: &[String]) -> PpResult<CliInput> {
    let input = positional_input(args, "convert")?;
    let values = options(
        &args[1..],
        &[
            "--out",
            "--width",
            "--height",
            "--filter",
            "--jpeg-quality",
            "--background",
        ],
    )?;
    Ok(CliInput::Operation(Operation::Convert {
        input,
        output: required_path(&values, "--out")?,
        width: optional_positive_u32(&values, "--width")?,
        height: optional_positive_u32(&values, "--height")?,
        filter: value(&values, "--filter")
            .map(parse_resample_filter)
            .transpose()
            .map_err(operation_input_error)?,
        jpeg_quality: optional_jpeg_quality(&values)?,
        background: value(&values, "--background")
            .map(parse_background)
            .transpose()?,
    }))
}

fn parse_upscale(args: &[String]) -> PpResult<CliInput> {
    let input = positional_input(args, "upscale")?;
    let values = options(
        &args[1..],
        &[
            "--out",
            "--scale",
            "--filter",
            "--jpeg-quality",
            "--background",
        ],
    )?;
    let raw_scale = required_parse::<u32>(
        &values,
        "--scale",
        "--scale must be an integer greater than or equal to 2",
    )?;
    Ok(CliInput::Operation(Operation::Upscale {
        input,
        output: required_path(&values, "--out")?,
        scale: ScaleFactor::new(raw_scale).map_err(operation_input_error)?,
        filter: value(&values, "--filter")
            .map(parse_resample_filter)
            .transpose()
            .map_err(operation_input_error)?,
        jpeg_quality: optional_jpeg_quality(&values)?,
        background: value(&values, "--background")
            .map(parse_background)
            .transpose()?,
    }))
}

fn parse_request_directory(args: &[String], normalize: bool) -> PpResult<CliInput> {
    let (request, output_dir) = request_directory(args)?;
    Ok(CliInput::Operation(if normalize {
        Operation::NormalizeSprite {
            request,
            output_dir,
        }
    } else {
        Operation::CompileSprite {
            request,
            output_dir,
        }
    }))
}

fn parse_vector(args: &[String]) -> PpResult<CliInput> {
    let has_input = args.first().is_some_and(|value| !value.starts_with("--"));
    if !has_input {
        return Err(PpError::InvalidOption(
            "vector requires <input> and --out <output.svg>".to_string(),
        ));
    }
    let values = options(
        &args[1..],
        &[
            "--out",
            "--preset",
            "--profile",
            "--detail",
            "--min-quality",
            "--max-quality-loss",
            "--max-paths",
            "--policy",
            "--report",
            "--diagnostics",
        ],
    )?;
    if value(&values, "--out").is_none() {
        return Err(PpError::InvalidOption(
            "vector requires <input> and --out <output.svg>".to_string(),
        ));
    }
    let input = PathBuf::from(&args[0]);

    let preset = value(&values, "--preset")
        .map(parse_vector_preset)
        .transpose()
        .map_err(|_| {
            PpError::InvalidOption(
                "--preset must be auto, pixel-art, exact-grid, flat-icon, line-art, or bounded-illustration"
                    .to_string(),
            )
        })?
        .unwrap_or(VectorPresetSelection::Auto);
    let profile = value(&values, "--profile")
        .map(parse_vector_profile)
        .transpose()
        .map_err(operation_input_error)?
        .unwrap_or(SvgProfile::Compact);
    let detail = value(&values, "--detail")
        .map(parse_vector_detail)
        .transpose()
        .map_err(operation_input_error)?
        .flatten();
    let quality_error = || {
        PpError::InvalidOption(
            "--min-quality and --max-quality-loss must be finite numbers from 0 through 1"
                .to_string(),
        )
    };
    let minimum_quality = value(&values, "--min-quality")
        .map(parse_unit_score)
        .transpose()
        .map_err(|_| quality_error())?;
    let maximum_quality_loss = value(&values, "--max-quality-loss")
        .map(parse_unit_score)
        .transpose()
        .map_err(|_| quality_error())?;
    let maximum_paths = optional_positive_usize(&values, "--max-paths")?;

    Ok(CliInput::Operation(Operation::CompileVector {
        input,
        output: required_path(&values, "--out")?,
        preset,
        profile,
        detail,
        minimum_quality,
        maximum_quality_loss,
        maximum_paths,
        policy: optional_path(&values, "--policy"),
        report: optional_path(&values, "--report"),
        diagnostics: optional_path(&values, "--diagnostics"),
    }))
}

fn parse_vector_analyze(args: &[String]) -> PpResult<CliInput> {
    if args.get(1..).unwrap_or_default().iter().any(|value| {
        matches!(
            value.as_str(),
            "--out"
                | "--detail"
                | "--min-quality"
                | "--max-quality-loss"
                | "--max-paths"
                | "--diagnostics"
        )
    }) {
        return Err(PpError::InvalidOption(
            "vector-analyze does not accept --out, candidate-detail, quality-limit, path-limit, or diagnostics options".to_string(),
        ));
    }
    let input = positional_input(args, "vector-analyze")?;
    let values = options(
        &args[1..],
        &["--preset", "--profile", "--policy", "--report"],
    )?;
    let preset = value(&values, "--preset")
        .map(parse_vector_preset)
        .transpose()
        .map_err(operation_input_error)?
        .unwrap_or(VectorPresetSelection::Auto);
    let profile = value(&values, "--profile")
        .map(parse_vector_profile)
        .transpose()
        .map_err(operation_input_error)?
        .unwrap_or(SvgProfile::Compact);
    Ok(CliInput::Operation(Operation::AnalyzeVector {
        input,
        preset,
        profile,
        policy: optional_path(&values, "--policy"),
        report: optional_path(&values, "--report"),
    }))
}

fn parse_motion_scaffold(args: &[String]) -> PpResult<CliInput> {
    let input = positional_input(args, "motion-scaffold")?;
    let values = options(&args[1..], &["--out-dir"])?;
    Ok(CliInput::Operation(Operation::ScaffoldMotion {
        input,
        output_dir: required_path(&values, "--out-dir")?,
    }))
}

fn parse_motion_build(args: &[String]) -> PpResult<CliInput> {
    let values = options(args, &["--request", "--out-dir"])?;
    Ok(CliInput::Operation(Operation::CompileMotion {
        request: required_path(&values, "--request")?,
        output_dir: required_path(&values, "--out-dir")?,
    }))
}

fn one_request(args: &[String], make: impl FnOnce(PathBuf) -> Operation) -> PpResult<CliInput> {
    let values = options(args, &["--request"])?;
    Ok(CliInput::Operation(make(required_path(
        &values,
        "--request",
    )?)))
}

fn positional_input(args: &[String], command: &str) -> PpResult<PathBuf> {
    let Some(input) = args.first() else {
        return Err(PpError::InvalidOption(format!(
            "{command} requires <input>"
        )));
    };
    if input.starts_with("--") {
        return Err(PpError::InvalidOption(format!(
            "{command} requires <input>"
        )));
    }
    Ok(PathBuf::from(input))
}

#[derive(Debug)]
struct OptionValue {
    key: String,
    value: String,
}

fn options(args: &[String], allowed: &[&str]) -> PpResult<Vec<OptionValue>> {
    if !args.len().is_multiple_of(2) {
        let key = args.last().map(String::as_str).unwrap_or("option");
        return Err(PpError::InvalidOption(format!(
            "missing value for option '{key}'"
        )));
    }
    let mut result = Vec::with_capacity(args.len() / 2);
    for pair in args.as_chunks::<2>().0 {
        let key = pair[0].as_str();
        if !key.starts_with("--") {
            return Err(PpError::InvalidOption(format!(
                "unexpected positional argument '{key}'"
            )));
        }
        if !allowed.contains(&key) {
            return Err(PpError::InvalidOption(format!("unknown option '{key}'")));
        }
        if result.iter().any(|entry: &OptionValue| entry.key == key) {
            return Err(PpError::InvalidOption(format!("duplicate option '{key}'")));
        }
        if pair[1].starts_with("--") {
            return Err(PpError::InvalidOption(format!(
                "missing value for option '{key}'"
            )));
        }
        result.push(OptionValue {
            key: key.to_string(),
            value: pair[1].clone(),
        });
    }
    Ok(result)
}

fn value<'a>(values: &'a [OptionValue], key: &str) -> Option<&'a str> {
    values
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.as_str())
}

fn required_path(values: &[OptionValue], key: &str) -> PpResult<PathBuf> {
    value(values, key)
        .map(PathBuf::from)
        .ok_or_else(|| PpError::InvalidOption(format!("{key} is required")))
}

fn optional_path(values: &[OptionValue], key: &str) -> Option<PathBuf> {
    value(values, key).map(PathBuf::from)
}

fn optional_parse<T: std::str::FromStr>(
    values: &[OptionValue],
    key: &str,
    message: &str,
) -> PpResult<Option<T>> {
    value(values, key)
        .map(|raw| {
            raw.parse::<T>()
                .map_err(|_| PpError::InvalidOption(message.to_string()))
        })
        .transpose()
}

fn required_parse<T: std::str::FromStr>(
    values: &[OptionValue],
    key: &str,
    message: &str,
) -> PpResult<T> {
    optional_parse(values, key, message)?
        .ok_or_else(|| PpError::InvalidOption(format!("{key} is required")))
}

fn optional_positive_u32(values: &[OptionValue], key: &str) -> PpResult<Option<NonZeroU32>> {
    optional_parse::<u32>(values, key, &format!("{key} must be a positive integer"))?
        .map(|raw| {
            NonZeroU32::new(raw)
                .ok_or_else(|| PpError::InvalidOption(format!("{key} must be a positive integer")))
        })
        .transpose()
}

fn optional_positive_usize(values: &[OptionValue], key: &str) -> PpResult<Option<NonZeroUsize>> {
    optional_parse::<usize>(values, key, &format!("{key} must be a positive integer"))?
        .map(|raw| {
            NonZeroUsize::new(raw)
                .ok_or_else(|| PpError::InvalidOption(format!("{key} must be a positive integer")))
        })
        .transpose()
}

fn optional_jpeg_quality(values: &[OptionValue]) -> PpResult<Option<JpegQuality>> {
    optional_parse::<u8>(
        values,
        "--jpeg-quality",
        "--jpeg-quality must be from 1 through 100",
    )?
    .map(JpegQuality::new)
    .transpose()
    .map_err(operation_input_error)
}

fn operation_input_error(error: impl std::fmt::Display) -> PpError {
    PpError::InvalidOption(error.to_string())
}

fn reject_extra(args: &[String], command: &str) -> PpResult<()> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(PpError::InvalidOption(format!(
            "{command} does not accept extra arguments"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn duplicate_options_fail_before_operation() {
        assert!(parse(&args(&[
            "convert", "a.png", "--out", "b.png", "--out", "c.png"
        ]))
        .is_err());
    }

    #[test]
    fn semantic_scale_validation_uses_canonical_newtype() {
        assert!(parse(&args(&[
            "upscale", "a.png", "--out", "b.png", "--scale", "1"
        ]))
        .is_err());
    }

    #[test]
    fn cli_parses_directly_to_canonical_operation() -> PpResult<()> {
        let parsed = parse(&args(&[
            "convert", "a.png", "--out", "b.png", "--width", "64", "--filter", "nearest",
        ]))?;
        let CliInput::Operation(Operation::Convert { width, filter, .. }) = parsed else {
            panic!("expected convert operation");
        };
        assert_eq!(width.map(NonZeroU32::get), Some(64));
        assert_eq!(filter, Some(crate::ResampleFilter::Nearest));
        Ok(())
    }

    #[test]
    fn request_driven_compilers_are_thin_operation_adapters() -> PpResult<()> {
        let cases = [
            ("document-psd", "document.compile_psd"),
            ("texture-compile", "texture.compile"),
            (
                "vision-foreground-instances",
                "vision.apple.foreground_instances",
            ),
        ];
        for (command, expected) in cases {
            let parsed = parse(&args(&[command, "--request", "request.json"]))?;
            let CliInput::Operation(operation) = parsed else {
                panic!("expected operation");
            };
            assert_eq!(operation.spec().name, expected);
        }
        Ok(())
    }
}
