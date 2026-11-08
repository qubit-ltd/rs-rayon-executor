// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

const TIMEOUT: Duration = Duration::from_secs(120);
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
struct Block {
    language: String,
    code: String,
    line: usize,
    heading: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Role {
    ReadmeMinimum,
    GuideAggregate,
    StopHelper,
    CapacityRetry,
    TrackedCancellation,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::ReadmeMinimum => "README minimum synchronous program",
            Self::GuideAggregate => "guide synchronous aggregate program",
            Self::StopHelper => "async stop helper",
            Self::CapacityRetry => "capacity retry program",
            Self::TrackedCancellation => "tracked cancellation program",
        }
    }
}

#[derive(Debug)]
struct Document {
    path: &'static str,
    blocks: Vec<Block>,
    rust: BTreeMap<Role, Block>,
}

fn parse_document(path: &'static str, source: &str) -> Result<Document, String> {
    let mut blocks = Vec::new();
    let mut headings = Vec::<String>::new();
    let mut active: Option<(String, usize, String, String)> = None;
    for (offset, line) in source.lines().enumerate() {
        let line_number = offset + 1;
        let trimmed = line.trim_start();
        if active.is_none()
            && let Some(rest) = trimmed.strip_prefix('#')
        {
            let level = trimmed.chars().take_while(|character| *character == '#').count();
            if level > 0 && trimmed[level..].starts_with(' ') {
                headings.truncate(level - 1);
                headings.push(rest.trim().to_owned());
            }
        }
        if let Some(rest) = trimmed.strip_prefix("```") {
            if let Some((language, start, heading, code)) = active.take() {
                blocks.push(Block {
                    language,
                    code,
                    line: start,
                    heading,
                });
            } else {
                active = Some((
                    rest.trim().to_ascii_lowercase(),
                    line_number,
                    headings.join(" / "),
                    String::new(),
                ));
            }
        } else if let Some((_, _, _, code)) = active.as_mut() {
            code.push_str(line);
            code.push('\n');
        }
    }
    if let Some((language, start, heading, _)) = active {
        return Err(format!("{path}:{start}: unclosed {language} fence under {heading}"));
    }
    let rust_blocks: Vec<Block> = blocks
        .iter()
        .filter(|block| is_rust(&block.language))
        .cloned()
        .collect();
    let (expected, indexed_roles): (usize, &[(usize, Role, &str)]) = match path {
        "README.md" => (1, &[(0, Role::ReadmeMinimum, "Quick Start")]),
        "README.zh_CN.md" => (1, &[(0, Role::ReadmeMinimum, "快速开始")]),
        "doc/user_guide.md" => (
            4,
            &[
                (0, Role::GuideAggregate, "Core Workflow"),
                (1, Role::StopHelper, "Async shutdown"),
                (2, Role::CapacityRetry, "Async shutdown"),
                (3, Role::TrackedCancellation, "Advanced Usage"),
            ],
        ),
        "doc/user_guide.zh_CN.md" => (
            4,
            &[
                (0, Role::GuideAggregate, "核心工作流"),
                (1, Role::StopHelper, "异步关闭与容量等待"),
                (2, Role::CapacityRetry, "异步关闭与容量等待"),
                (3, Role::TrackedCancellation, "进阶用法"),
            ],
        ),
        _ => return Err(format!("{path}: no role inventory is configured")),
    };
    let rust = classify_roles(path, &rust_blocks, expected, indexed_roles)?;
    Ok(Document { path, blocks, rust })
}

fn classify_roles(
    path: &'static str,
    rust_blocks: &[Block],
    expected: usize,
    indexed_roles: &[(usize, Role, &str)],
) -> Result<BTreeMap<Role, Block>, String> {
    if rust_blocks.len() != expected {
        let block = rust_blocks.get(expected);
        let (line, heading) = block.map_or((1, "document end"), |block| (block.line, block.heading.as_str()));
        return Err(format!(
            "{path}:{line}: Rust example inventory mismatch under {heading:?}; found {} blocks but requires exactly {expected}",
            rust_blocks.len()
        ));
    }
    let mut rust = BTreeMap::new();
    for (index, role, heading) in indexed_roles {
        let block = rust_blocks[*index].clone();
        if !block.heading.contains(heading) {
            return Err(format!(
                "{path}:{}: {} expected under heading containing {heading:?}, found {:?}",
                block.line,
                role.name(),
                block.heading
            ));
        }
        if rust.insert(*role, block.clone()).is_some() {
            return Err(format!(
                "{path}:{} under {:?}: duplicate role {}",
                block.line,
                block.heading,
                role.name()
            ));
        }
    }
    if rust.len() != indexed_roles.len() {
        return Err(format!("{path}:1: a required Rust example role is missing"));
    }
    Ok(rust)
}

fn is_rust(language: &str) -> bool {
    matches!(language.split([',', ' ']).next(), Some("rust" | "rs"))
}

#[test]
fn test_parser_rejects_missing_unknown_unclosed_and_duplicate_roles() {
    let valid = "# Quick Start\n```rust\nfn main() {}\n```\n";
    let missing = parse_document("README.md", "# Guide\n").expect_err("missing role must fail");
    assert!(missing.contains("README.md:") && missing.contains("inventory") && missing.contains("document end"));
    let extra = parse_document("README.md", &format!("{valid}\n```rust\nfn extra() {{}}\n```\n"))
        .expect_err("unknown Rust example must fail");
    assert!(extra.contains("README.md:") && extra.contains("found 2 blocks") && extra.contains("Quick Start"));
    let unclosed =
        parse_document("README.md", "# Guide\n```rust\nfn main() {}\n").expect_err("unclosed fence must fail");
    assert!(unclosed.contains("README.md:") && unclosed.contains("unclosed rust fence"));
    let parsed = parse_document("README.md", valid).expect("single role inventory should parse");
    let block = parsed.rust[&Role::ReadmeMinimum].clone();
    let duplicate_roles = [
        (0, Role::ReadmeMinimum, "Quick Start"),
        (0, Role::ReadmeMinimum, "Quick Start"),
    ];
    let duplicate =
        classify_roles("README.md", &[block], 1, &duplicate_roles).expect_err("duplicate role inventory must fail");
    assert!(
        duplicate.contains("README.md:") && duplicate.contains("Quick Start") && duplicate.contains("duplicate role")
    );
}

pub(crate) fn check_all_documents() {
    let paths = [
        "README.md",
        "README.zh_CN.md",
        "doc/user_guide.md",
        "doc/user_guide.zh_CN.md",
    ];
    let docs: Vec<Document> = paths
        .iter()
        .map(|path| {
            let source =
                fs::read_to_string(path).unwrap_or_else(|error| panic!("{path}: cannot read document: {error}"));
            parse_document(path, &source).unwrap_or_else(|error| panic!("{error}"))
        })
        .collect();
    assert_eq!(
        docs[0].rust.keys().collect::<Vec<_>>(),
        docs[1].rust.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        docs[2].rust.keys().collect::<Vec<_>>(),
        docs[3].rust.keys().collect::<Vec<_>>()
    );
    for doc in docs {
        check_document(doc);
    }
}

fn check_document(doc: Document) {
    for (role, block) in doc.rust {
        let asynchronous = matches!(role, Role::StopHelper | Role::CapacityRetry);
        let fixture = Fixture::new(asynchronous);
        fixture.write_manifest(doc.path, &doc.blocks, asynchronous);
        let source = if role == Role::StopHelper {
            format!(
                "{}\n#[tokio::main]\nasync fn main() {{\n    let service = RayonExecutorService::builder().num_threads(1).build().unwrap();\n    stop_without_blocking_runtime(&service).await.unwrap();\n    assert!(service.is_terminated());\n}}\n",
                block.code
            )
        } else if role == Role::CapacityRetry {
            let helpers = block.code.split("#[tokio::main]").next().unwrap_or(&block.code);
            format!(
                "{helpers}\n#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {{\n    use std::time::Duration;\n    let service = RayonExecutorService::builder().num_threads(1).task_capacity(1).build()?;\n    let (started_tx, started_rx) = std::sync::mpsc::channel();\n    let (release_tx, release_rx) = std::sync::mpsc::channel();\n    let blocker = service.try_submit_callable(move || {{ started_tx.send(()).unwrap(); release_rx.recv_timeout(Duration::from_secs(10)).unwrap(); Ok::<(), std::io::Error>(()) }}).unwrap();\n    started_rx.recv_timeout(Duration::from_secs(10))?;\n    let waiting = submit_after_capacity(&service, vec![8, 13, 21, 34].into_boxed_slice());\n    tokio::pin!(waiting);\n    let was_pending = std::future::poll_fn(|cx| std::task::Poll::Ready(matches!(std::future::Future::poll(waiting.as_mut(), cx), std::task::Poll::Pending))).await;\n    assert!(was_pending, \"saturated submission should wait before capacity is released\");\n    release_tx.send(())?;\n    blocker.await?;\n    assert_eq!(waiting.await?, 76);\n    service.shutdown(); service.await_termination().await; Ok(())\n}}\n"
            )
        } else {
            block.code
        };
        let bin = format!("example_{}", role as usize);
        fixture.write_bin(&bin, &source);
        fixture.run_cargo(doc.path, role, &block.heading, block.line, &bin);
    }
}

struct Fixture {
    root: PathBuf,
    target: PathBuf,
    asynchronous: bool,
}

impl Fixture {
    fn new(asynchronous: bool) -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("rre-doc-examples-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join("src/bin")).expect("create temporary documentation fixture");
        Self {
            target: root.join("target"),
            root,
            asynchronous,
        }
    }

    fn write_manifest(&self, source_path: &str, blocks: &[Block], asynchronous: bool) {
        let install = blocks
            .iter()
            .find(|block| block.language == "toml" && block.code.contains("qubit-rayon-executor"))
            .unwrap_or_else(|| panic!("{source_path}: no installation TOML block found"));
        let async_config = if asynchronous {
            Some(
                blocks
                    .iter()
                    .find(|block| block.language == "toml" && block.code.contains("async-wait"))
                    .unwrap_or_else(|| panic!("{source_path}: async role has no documented async-wait TOML block")),
            )
        } else {
            None
        };
        let version_source = async_config.map_or(&install.code, |block| &block.code);
        let version = dependency_version(version_source, "qubit-rayon-executor", source_path);
        let executor_version = dependency_version(version_source, "qubit-executor", source_path);
        let features = if let Some(documented) = async_config {
            if !documented.code.contains("rt-multi-thread") {
                panic!("{source_path}: Tokio runtime feature is absent from async configuration");
            }
            ", features = [\"async-wait\"]"
        } else {
            ""
        };
        let tokio = if asynchronous {
            let config = blocks
                .iter()
                .find(|block| block.language == "toml" && block.code.contains("tokio ="))
                .unwrap_or_else(|| panic!("{source_path}: async role has no documented Tokio dependency"));
            config
                .code
                .lines()
                .find(|line| line.trim_start().starts_with("tokio ="))
                .unwrap_or_else(|| panic!("{source_path}: Tokio dependency line missing"))
        } else {
            ""
        };
        let manifest = format!(
            "[package]\nname = \"doc-example-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nqubit-executor = {{ version = \"{executor_version}\" }}\nqubit-rayon-executor = {{ version = \"{version}\", path = \"{}\"{features} }}\n{tokio}\n[patch.crates-io]\nqubit-executor = {{ path = \"{}\" }}\n",
            project_root().display(),
            project_root().join("../rs-executor").display()
        );
        fs::write(self.root.join("Cargo.toml"), manifest).expect("write fixture manifest");
        let _ = self.asynchronous;
    }

    fn write_bin(&self, name: &str, source: &str) {
        fs::write(self.root.join("src/bin").join(format!("{name}.rs")), source).expect("write extracted example");
    }

    fn run_cargo(&self, document: &str, role: Role, heading: &str, line: usize, bin: &str) {
        let mut command = Command::new("cargo");
        command
            .arg("run")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(self.root.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&self.target)
            .arg("--bin")
            .arg(bin);
        if !self.asynchronous {
            command.arg("--no-default-features");
        }
        let output = run_bounded(command, document, role, heading, line);
        if !output.status.success() {
            panic!(
                "{document}:{line} ({heading}) {} failed\nstdout:\n{}\nstderr:\n{}",
                role.name(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_bounded(mut command: Command, document: &str, role: Role, heading: &str, line: usize) -> Output {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| {
            panic!(
                "{document}:{line} ({heading}) {}: failed to start child process: {error}",
                role.name()
            )
        });
    let start = Instant::now();
    loop {
        if child
            .try_wait()
            .unwrap_or_else(|error| {
                panic!(
                    "{document}:{line} ({heading}) {}: cannot wait for child process: {error}",
                    role.name()
                )
            })
            .is_some()
        {
            return child.wait_with_output().expect("collect cargo output");
        }
        if start.elapsed() >= TIMEOUT {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect timed-out cargo output");
            panic!(
                "{document}:{line} ({heading}) {} exceeded {} seconds\nstderr:\n{}",
                role.name(),
                TIMEOUT.as_secs(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn dependency_version(toml: &str, name: &str, document: &str) -> String {
    for line in toml.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == name {
            let version_value = value.trim();
            let version_value = if version_value.starts_with('{') {
                version_value
                    .split(',')
                    .find_map(|field| {
                        let (field, value) = field.split_once('=')?;
                        (field.trim().trim_start_matches('{').trim() == "version").then_some(value.trim())
                    })
                    .unwrap_or_else(|| panic!("{document}: documented {name} dependency has no version"))
            } else {
                version_value
            };
            let version = version_value.trim_matches('"').trim();
            return version.to_owned();
        }
    }
    panic!("{document}: documented TOML has no {name} version")
}

fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}
