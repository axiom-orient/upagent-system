use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "perfectpixel-chroma-plan-{}-{stamp}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_plan(request: &serde_json::Value) -> std::process::Output {
    let root = TempDir::new();
    let request_path = root.path().join("request.json");
    fs::write(&request_path, request.to_string()).expect("request");
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["chroma-plan", "--request"])
        .arg(request_path)
        .output()
        .expect("run chroma-plan")
}

#[test]
fn valid_plan_is_strict_bounded_and_deterministic() {
    let request = serde_json::json!({
        "schemaVersion": 1,
        "operation": "chroma_plan",
        "subjectRgbColors": [[255, 255, 255], [32, 32, 32], [180, 80, 20]]
    });
    let first = run_plan(&request);
    let second = run_plan(&request);
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);

    let payload: serde_json::Value = serde_json::from_slice(&first.stdout).expect("response JSON");
    assert_eq!(payload["schema"], "perfectpixel.chroma-plan/1");
    assert_eq!(payload["schemaVersion"], 1);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["operation"], "chroma_plan");
    assert_eq!(payload["metric"], "oklab-euclidean-maximin/1");
    assert!(payload["selectedRgb"].is_array());
    assert!(payload["selectedHex"].as_str().unwrap().starts_with('#'));
    assert!(payload["minDistance"].as_f64().unwrap().is_finite());
    assert_eq!(payload["candidateScores"].as_array().unwrap().len(), 8);
    for candidate in payload["candidateScores"].as_array().unwrap() {
        assert_eq!(candidate.as_object().unwrap().len(), 3);
        assert!(candidate["rgb"].is_array());
        assert!(candidate["hex"].as_str().unwrap().starts_with('#'));
        assert!(candidate["score"].as_f64().unwrap().is_finite());
    }
}

#[test]
fn plan_rejects_unknown_empty_duplicate_and_excess_colors() {
    let cases = [
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "chroma_plan",
            "subjectRgbColors": [],
        }),
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "chroma_plan",
            "subjectRgbColors": [[1, 2, 3], [1, 2, 3]],
        }),
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "chroma_plan",
            "subjectRgbColors": vec![[0, 0, 0]; 33],
        }),
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "chroma_plan",
            "subjectRgbColors": [[1, 2, 3]],
            "unexpected": true,
        }),
    ];
    for request in cases {
        let output = run_plan(&request);
        assert_eq!(output.status.code(), Some(2));
        let payload: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("error JSON");
        assert_eq!(payload["ok"], false);
    }
}

#[test]
fn schema_and_help_advertise_chroma_plan_contract() {
    let schema = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("schema")
        .output()
        .expect("schema");
    assert!(schema.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&schema.stdout).expect("schema JSON");
    assert_eq!(payload["chromaPlanSchema"], "perfectpixel.chroma-plan/1");
    assert_eq!(
        payload["chromaPlanCommand"]["requestSchema"],
        "perfectpixel.chroma-plan/1"
    );
    assert_eq!(
        payload["chromaPlanCommand"]["metric"],
        "oklab-euclidean-maximin/1"
    );
    assert_eq!(
        payload["chromaPlanCommand"]["candidatePalette"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let help = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("--help")
        .output()
        .expect("help");
    let help = String::from_utf8(help.stdout).expect("UTF-8 help");
    assert!(help.contains("perfectpixel chroma-plan --request <chroma-plan-request.json>"));
}
