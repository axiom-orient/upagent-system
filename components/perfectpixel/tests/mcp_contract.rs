use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use perfectpixel::{PngEncoder, Raster};
use serde_json::{json, Value};

struct Wire {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl Wire {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(mcp_binary())
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn perfectpixel-mcp");
        Self {
            stdin: child.stdin.take().expect("MCP stdin"),
            reader: BufReader::new(child.stdout.take().expect("MCP stdout")),
            child,
        }
    }

    fn send(&mut self, message: Value) {
        let bytes = serde_json::to_vec(&message).expect("serialize MCP request");
        self.stdin.write_all(&bytes).expect("write MCP request");
        self.stdin.write_all(b"\n").expect("write MCP newline");
        self.stdin.flush().expect("flush MCP request");
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read MCP response");
        assert!(!line.is_empty(), "MCP closed stdout before a response");
        serde_json::from_str(&line).expect("MCP response is JSON")
    }

    fn reject_legacy_initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "perfectpixel-test", "version": "1"}
            }
        }));
        let response = self.receive();
        assert_eq!(response["error"]["code"], -32601);
    }

    fn discover(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": {"_meta": request_meta()}
        }));
        let response = self.receive();
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(
            response["result"]["supportedVersions"],
            json!(["2026-07-28"])
        );
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments, "_meta": request_meta()}
        }));
        self.receive()
    }

    fn finish(self) -> Output {
        drop(self.stdin);
        drop(self.reader);
        self.child.wait_with_output().expect("wait MCP process")
    }
}

#[test]
fn stdio_discovery_inspect_convert_and_unsafe_paths_are_bounded() {
    let root = temp_case("wire");
    let input = root.join("input.png");
    let cli_output = root.join("cli-output.png");
    let mcp_output = root.join("mcp-output.png");
    write_png(
        &input,
        Raster::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 128]).unwrap(),
    );

    let cli_inspect = run_cli(&["inspect", input.to_str().unwrap()]);
    assert!(cli_inspect.status.success(), "CLI inspect must succeed");
    let cli_inspect_json: Value =
        serde_json::from_slice(&cli_inspect.stdout).expect("CLI inspect JSON");

    let mut legacy_wire = Wire::start(&root);
    legacy_wire.reject_legacy_initialize();
    let legacy_output = legacy_wire.finish();
    assert!(!legacy_output.status.success());
    assert!(String::from_utf8_lossy(&legacy_output.stderr).contains("initialize failed"));

    let mut wire = Wire::start(&root);
    wire.discover();

    wire.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {"_meta": request_meta()}
    }));
    let list = wire.receive();
    let mut names = list["result"]["tools"]
        .as_array()
        .expect("tools/list tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "perfectpixel_bundle",
            "perfectpixel_convert",
            "perfectpixel_inspect",
            "perfectpixel_motion_build",
            "perfectpixel_motion_scaffold",
            "perfectpixel_normalize",
            "perfectpixel_schema",
            "perfectpixel_upscale",
            "perfectpixel_vector",
            "perfectpixel_vector_analyze",
        ]
    );
    for tool in list["result"]["tools"].as_array().expect("tools") {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["annotations"].is_object());
    }

    let inspect = wire.call(3, "perfectpixel_inspect", json!({"inputPath": "input.png"}));
    let inspect_envelope = structured_envelope(&inspect);
    assert_eq!(inspect_envelope["schema"], "perfectpixel.mcp-tool-result/1");
    assert_eq!(inspect_envelope["ok"], true);
    assert_eq!(inspect_envelope["operation"], "perfectpixel_inspect");
    assert_eq!(inspect_envelope["exitCode"], 0);
    assert_eq!(
        inspect_envelope["result"]["inputSha256"],
        cli_inspect_json["inputSha256"]
    );
    assert_eq!(
        inspect_envelope["result"]["width"],
        cli_inspect_json["width"]
    );
    assert_eq!(
        inspect_envelope["result"]["height"],
        cli_inspect_json["height"]
    );

    let converted = wire.call(
        4,
        "perfectpixel_convert",
        json!({
            "inputPath": "input.png",
            "outputPath": "mcp-output.png",
            "width": 4,
            "filter": "nearest"
        }),
    );
    let converted_envelope = structured_envelope(&converted);
    assert_eq!(converted_envelope["ok"], true, "converted={converted:?}");
    assert!(mcp_output.is_file());

    let cli_convert = run_cli(&[
        "convert",
        input.to_str().unwrap(),
        "--out",
        cli_output.to_str().unwrap(),
        "--width",
        "4",
        "--filter",
        "nearest",
    ]);
    assert!(cli_convert.status.success(), "CLI convert must succeed");
    let cli_convert_json: Value =
        serde_json::from_slice(&cli_convert.stdout).expect("CLI convert JSON");
    for key in [
        "inputSha256",
        "inputByteCount",
        "outputSha256",
        "outputByteCount",
        "inputWidth",
        "inputHeight",
        "outputWidth",
        "outputHeight",
        "format",
        "filter",
    ] {
        assert_eq!(
            converted_envelope["result"][key], cli_convert_json[key],
            "MCP and CLI convert field differs: {key}"
        );
    }
    assert_eq!(
        fs::read(&mcp_output).expect("MCP output bytes"),
        fs::read(&cli_output).expect("CLI output bytes")
    );

    let traversal = wire.call(
        5,
        "perfectpixel_inspect",
        json!({"inputPath": "../input.png"}),
    );
    assert_eq!(traversal["result"]["isError"], true);
    assert_eq!(
        structured_envelope(&traversal)["result"]["code"],
        "rootOrPathRejected"
    );

    let absolute = wire.call(
        6,
        "perfectpixel_inspect",
        json!({"inputPath": input.to_string_lossy()}),
    );
    assert_eq!(absolute["result"]["isError"], true);

    let unknown = wire.call(
        7,
        "perfectpixel_inspect",
        json!({"inputPath": "input.png", "unexpected": true}),
    );
    assert_eq!(unknown["result"]["isError"], true);
    assert!(unknown["result"]["content"][0]["text"]
        .as_str()
        .expect("invalid params text")
        .contains("unknown field"));

    let root_output = wire.call(
        8,
        "perfectpixel_motion_scaffold",
        json!({"inputPath": "input.png", "outputDir": "."}),
    );
    assert_eq!(root_output["result"]["isError"], true);

    {
        use std::os::unix::fs::symlink;
        let outside = temp_case("wire-outside");
        let outside_input = outside.join("outside.png");
        fs::write(&outside_input, fs::read(&input).expect("input bytes")).expect("outside file");
        symlink(&outside_input, root.join("escape.png")).expect("escape symlink");
        let escaped = wire.call(
            9,
            "perfectpixel_inspect",
            json!({"inputPath": "escape.png"}),
        );
        assert_eq!(escaped["result"]["isError"], true);
        let _ = fs::remove_dir_all(outside);
    }

    let output = wire.finish();
    assert!(
        output.status.success(),
        "MCP process failed: stdout={:?}, stderr={:?}",
        output.stdout,
        output.stderr
    );
    assert!(
        output.stderr.is_empty(),
        "MCP diagnostics must stay on stderr only when empty: {:?}",
        output.stderr
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn oversized_request_is_rejected_by_bounded_preparse() {
    let root = temp_case("oversized-preparse");
    let request = root.join("oversized.json");
    let file = fs::File::create(&request).expect("create oversized request");
    file.set_len(8 * 1024 * 1024 + 1)
        .expect("size oversized request");

    let mut wire = Wire::start(&root);
    wire.discover();
    let response = wire.call(
        20,
        "perfectpixel_normalize",
        json!({"requestPath": "oversized.json", "outputDir": "normalized"}),
    );
    let envelope = structured_envelope(&response);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(envelope["result"]["code"], "rootOrPathRejected");
    assert!(envelope["result"]["message"]
        .as_str()
        .expect("preparse error message")
        .contains("preparse limit"));
    assert!(!root.join("normalized").exists());

    fs::write(root.join("malformed.json"), b"{").expect("write malformed request");
    let malformed = wire.call(
        21,
        "perfectpixel_normalize",
        json!({"requestPath": "malformed.json", "outputDir": "malformed-out"}),
    );
    let malformed_envelope = structured_envelope(&malformed);
    assert_eq!(malformed["result"]["isError"], true);
    assert!(malformed_envelope["exitCode"].as_i64().unwrap_or_default() > 0);
    assert_eq!(malformed_envelope["result"]["ok"], false);
    assert!(malformed_envelope["result"]["message"].is_string());
    assert!(malformed_envelope["result"]["code"].is_null());
    assert!(!root.join("malformed-out").exists());

    let output = wire.finish();
    assert!(output.status.success(), "MCP process must exit cleanly");
    assert!(output.stderr.is_empty(), "normal MCP errors stay on stdout");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn nested_request_and_output_parent_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;

    let root = temp_case("nested-symlinks");
    let outside = temp_case("nested-symlinks-outside");
    let outside_png = outside.join("outside.png");
    let outside_svg = outside.join("outside.svg");
    write_png(
        &outside_png,
        Raster::new(1, 1, vec![255, 0, 0, 255]).expect("raster"),
    );
    fs::write(
        &outside_svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1" viewBox="0 0 1 1"><path id="p" fill="#ff0000" d="M0 0L1 0L1 1Z"/></svg>"##,
    )
    .expect("outside SVG");
    symlink(&outside_png, root.join("escape.png")).expect("nested raster symlink");
    symlink(&outside_svg, root.join("escape.svg")).expect("nested SVG symlink");
    symlink(&outside, root.join("output-link")).expect("output parent symlink");
    write_png(
        &root.join("input.png"),
        Raster::new(1, 1, vec![0, 255, 0, 255]).expect("input raster"),
    );

    fs::write(
        root.join("normalize.json"),
        serde_json::to_vec(&json!({
            "character": "hero",
            "cellWidth": 1,
            "cellHeight": 1,
            "states": [{"name": "idle", "frames": ["escape.png"]}]
        }))
        .expect("normalize request"),
    )
    .expect("write normalize request");
    fs::write(
        root.join("bundle.json"),
        serde_json::to_vec(&json!({
            "character": "hero",
            "cellWidth": 1,
            "cellHeight": 1,
            "states": [{
                "name": "idle",
                "fps": 8,
                "loop": true,
                "frames": ["escape.png"]
            }]
        }))
        .expect("bundle request"),
    )
    .expect("write bundle request");
    fs::write(
        root.join("motion.json"),
        serde_json::to_vec(&json!({
            "schema": "perfectpixel.motion/1",
            "name": "motion",
            "sourceSvg": "escape.svg",
            "sourceSvgSha256": "0".repeat(64),
            "fps": 30,
            "durationMs": 100,
            "loop": false,
            "parts": [],
            "tracks": []
        }))
        .expect("motion request"),
    )
    .expect("write motion request");

    let mut wire = Wire::start(&root);
    wire.discover();
    let calls = [
        (
            "perfectpixel_normalize",
            json!({"requestPath": "normalize.json", "outputDir": "normalized"}),
        ),
        (
            "perfectpixel_bundle",
            json!({"requestPath": "bundle.json", "outputDir": "bundle-out"}),
        ),
        (
            "perfectpixel_motion_build",
            json!({"requestPath": "motion.json", "outputDir": "motion-out"}),
        ),
        (
            "perfectpixel_convert",
            json!({"inputPath": "input.png", "outputPath": "output-link/output.png"}),
        ),
    ];
    for (offset, (tool, arguments)) in calls.into_iter().enumerate() {
        let response = wire.call(30 + offset as u64, tool, arguments);
        let envelope = structured_envelope(&response);
        assert_eq!(response["result"]["isError"], true, "tool={tool}");
        assert_eq!(
            envelope["result"]["code"], "rootOrPathRejected",
            "tool={tool}, response={response:?}"
        );
        assert!(
            envelope["result"]["message"]
                .as_str()
                .expect("path error message")
                .contains("symlink"),
            "tool={tool}, response={response:?}"
        );
    }
    assert!(!outside.join("output.png").exists());

    let output = wire.finish();
    assert!(output.status.success(), "MCP process must exit cleanly");
    assert!(output.stderr.is_empty(), "normal MCP errors stay on stdout");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}

fn structured_envelope(response: &Value) -> Value {
    response["result"]["structuredContent"].clone()
}

fn request_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "perfectpixel-test",
            "version": "1"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(args)
        .output()
        .expect("run perfectpixel")
}

fn mcp_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_perfectpixel_mcp") {
        return PathBuf::from(path);
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    target.join("debug/perfectpixel-mcp")
}

fn write_png(path: &Path, image: Raster) {
    fs::write(path, PngEncoder::encode_rgba(&image).expect("encode PNG")).expect("write PNG");
}

fn temp_case(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "perfectpixel-mcp-contract-{label}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create temp root");
    path
}
