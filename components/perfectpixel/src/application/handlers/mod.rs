use std::path::Path;

use crate::PpResult;

mod agent;
mod asset;
mod chroma;
mod document;
mod edit;
mod motion;
mod schema;
mod sprite;
mod texture;
mod vector;
mod vision;

pub(crate) use agent::agent_schema;
pub(crate) use asset::{convert, inspect, upscale};

pub(crate) fn agent_inspect(request: &Path) -> PpResult<String> {
    agent::agent_inspect(&args_for_request(request))
}

pub(crate) fn agent_extract(request: &Path, output_dir: &Path) -> PpResult<String> {
    agent::agent_extract(&args_for_request_and_output(request, output_dir))
}

pub(crate) fn agent_render(request: &Path, output_dir: &Path) -> PpResult<String> {
    agent::agent_render(&args_for_request_and_output(request, output_dir))
}

pub(crate) fn agent_compare(request: &Path, output_dir: &Path) -> PpResult<String> {
    agent::agent_compare(&args_for_request_and_output(request, output_dir))
}

fn args_for_request(request: &Path) -> Vec<String> {
    vec!["--request".to_owned(), request.display().to_string()]
}

fn args_for_request_and_output(request: &Path, output_dir: &Path) -> Vec<String> {
    vec![
        "--request".to_owned(),
        request.display().to_string(),
        "--out-dir".to_owned(),
        output_dir.display().to_string(),
    ]
}
pub(crate) use chroma::plan as chroma_plan;
pub(crate) use document::{
    compile_layered_psd as compile_document_psd, export_flattened_psd as export_psd,
};
pub(crate) use edit::execute as edit;
pub(crate) use motion::{build as motion_build, scaffold as motion_scaffold};
pub(crate) use schema::schema;
pub(crate) use sprite::{bundle, normalize};
pub(crate) use texture::compile as texture_compile;
pub(crate) use vector::{analyze as vector_analyze, compile as vector_compile};
pub(crate) use vision::foreground_instances as vision_foreground_instances;
