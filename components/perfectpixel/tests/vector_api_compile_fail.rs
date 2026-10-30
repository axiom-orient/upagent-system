use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempProject(PathBuf);

impl TempProject {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "perfectpixel-api-consumer-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).expect("create consumer crate");
        let package = Path::new(env!("CARGO_MANIFEST_DIR"));
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"perfectpixel_api_consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\nperfectpixel = {{ path = {:?} }}\nserde_json = \"1\"\n\n[workspace]\n",
                package
            ),
        )
        .expect("write consumer manifest");
        Self(root)
    }

    fn check(&self, source: &str) -> Output {
        fs::write(self.0.join("src/main.rs"), source).expect("write consumer source");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        Command::new(cargo)
            .args(["check", "--offline", "--quiet"])
            .current_dir(&self.0)
            .env("CARGO_TARGET_DIR", self.0.join("target"))
            .output()
            .expect("run cargo check for consumer")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn current_capability_api_compiles_and_removed_facade_does_not() {
    let project = TempProject::new();
    let current = project.check(
        r#"use perfectpixel::{
    ApprovedVectorOutput, EvaluationReport, VectorAnalysis, VectorAnalysisRequest, VectorOutcome,
    VectorRequest, Vectorizer,
};

fn accepts_current_api(
    _: Option<Vectorizer>,
    _: Option<VectorRequest>,
    _: Option<VectorOutcome>,
    _: Option<ApprovedVectorOutput>,
    _: Option<EvaluationReport>,
    _: Option<VectorAnalysisRequest>,
    _: Option<VectorAnalysis>,
) {}

fn main() {}
"#,
    );
    assert!(
        current.status.success(),
        "current consumer API failed: {}",
        String::from_utf8_lossy(&current.stderr)
    );

    let removed = project.check(
        r#"use perfectpixel::{
    ContentClass, ContentProfile, NativeVectorizer, VectorCandidateReport, VectorOptions,
    VectorOutput, VectorPreset, VectorReport,
};

fn main() {}
"#,
    );
    assert!(
        !removed.status.success(),
        "removed facade unexpectedly compiled"
    );
    let stderr = String::from_utf8_lossy(&removed.stderr);
    for symbol in [
        "ContentClass",
        "ContentProfile",
        "NativeVectorizer",
        "VectorCandidateReport",
        "VectorOptions",
        "VectorOutput",
        "VectorPreset",
        "VectorReport",
    ] {
        assert!(
            stderr.contains(symbol),
            "compiler output omitted {symbol}: {stderr}"
        );
    }

    let external_authority = project.check(
        r#"use perfectpixel::Vectorizer;

fn main() {
    let _ = Vectorizer::new("authority.json");
}
"#,
    );
    assert!(
        !external_authority.status.success(),
        "Vectorizer unexpectedly accepted a caller-supplied authority source"
    );
    let stderr = String::from_utf8_lossy(&external_authority.stderr);
    assert!(
        stderr.contains("this function takes 0 arguments"),
        "compiler output did not prove the zero-argument embedded-authority boundary: {stderr}"
    );

    let deserializable_motion_request = project.check(
        r##"use perfectpixel::MotionAssessmentRequest;

fn main() {
    let _: MotionAssessmentRequest =
        serde_json::from_str(r#"{"requested":true}"#).expect("deserialize request");
}
"##,
    );
    assert!(
        !deserializable_motion_request.status.success(),
        "MotionAssessmentRequest unexpectedly implemented Deserialize"
    );
    let stderr = String::from_utf8_lossy(&deserializable_motion_request.stderr);
    assert!(
        stderr.contains("Deserialize"),
        "compiler output did not prove the non-deserializable request boundary: {stderr}"
    );
}
