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
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(120);
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[test]
fn test_source_development_dependency_matches_manifest() {
    let root = project_root();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read package manifest");
    let requirement = dependency_version(&manifest, "qubit-executor", "Cargo.toml");
    for (path, heading) in [("README.md", "## Testing"), ("README.zh_CN.md", "## 测试")] {
        let source = fs::read_to_string(root.join(path)).expect("read README");
        let section = source
            .split_once(heading)
            .expect("README must have a source testing section")
            .1
            .split("\n## ")
            .next()
            .expect("testing section exists");
        assert!(
            section.contains(&format!("`{requirement}`")),
            "{path}: source testing section must document {requirement}",
        );
    }
}

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
    GenericRecoverable,
    TrackedCancellation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DependencySource {
    Source,
    Registry,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::ReadmeMinimum => "README minimum synchronous program",
            Self::GuideAggregate => "guide synchronous aggregate program",
            Self::StopHelper => "async stop helper",
            Self::CapacityRetry => "capacity retry program",
            Self::GenericRecoverable => "generic recoverable submission program",
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
            5,
            &[
                (0, Role::GuideAggregate, "Core Workflow"),
                (1, Role::StopHelper, "Async shutdown"),
                (2, Role::CapacityRetry, "Async shutdown"),
                (3, Role::GenericRecoverable, "Generic recoverable submission"),
                (4, Role::TrackedCancellation, "Advanced Usage"),
            ],
        ),
        "doc/user_guide.zh_CN.md" => (
            5,
            &[
                (0, Role::GuideAggregate, "核心工作流"),
                (1, Role::StopHelper, "异步关闭与容量等待"),
                (2, Role::CapacityRetry, "异步关闭与容量等待"),
                (3, Role::GenericRecoverable, "泛型恢复提交"),
                (4, Role::TrackedCancellation, "进阶用法"),
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

#[test]
fn test_capacity_retry_original_main_compile_error_is_reported() {
    let source = fs::read_to_string("doc/user_guide.md").expect("read English user guide");
    let document = parse_document("doc/user_guide.md", &source).expect("parse English user guide");
    let block = document.rust[&Role::CapacityRetry].clone();
    let cache = BuildCache::new();
    let fixture = Fixture::new(true, &cache, DependencySource::Source);
    fixture.write_manifest(document.path, &document.blocks, true);

    let malformed = capacity_retry_with_invalid_main(&block);
    let binary = "capacity_retry_invalid_original";
    fixture.write_bin(binary, &malformed);
    let output = fixture.run_cargo_output(document.path, Role::CapacityRetry, &block.heading, block.line, binary);
    assert!(!output.status.success(), "the original main must fail to compile");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(binary), "binary context missing from stderr:\n{stderr}");
    assert!(
        stderr.contains("missing_capacity_retry_symbol"),
        "compiler error for the injected missing symbol is absent:\n{stderr}"
    );
}

pub(crate) fn check_all_documents() {
    check_all(DependencySource::Source);
}

pub(crate) fn check_all_registry_documents() {
    check_all(DependencySource::Registry);
}

fn check_all(source: DependencySource) {
    let cache = BuildCache::new();
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
        check_document(doc, &cache, source);
    }
}

fn check_document(doc: Document, cache: &BuildCache, source: DependencySource) {
    for (role, block) in doc.rust {
        let asynchronous = matches!(role, Role::StopHelper | Role::CapacityRetry);
        let mut fixture = Fixture::new(asynchronous, cache, source);
        fixture.write_manifest(doc.path, &doc.blocks, asynchronous);
        if role == Role::CapacityRetry {
            let original = format!("example_{}_original", role as usize);
            fixture.write_bin(&original, &block.code);
            if source == DependencySource::Registry {
                fixture.verify_registry_resolution(doc.path, role, &block.heading, block.line);
            }
            let original_path = fixture.root.join("src/bin").join(format!("{original}.rs"));
            let written = fs::read(&original_path).expect("read original documentation program");
            assert_eq!(
                written,
                block.code.as_bytes(),
                "{doc_path}:{line} original program was altered",
                doc_path = doc.path,
                line = block.line
            );
            fixture.run_cargo(doc.path, role, &block.heading, block.line, &original);

            let saturated = format!("example_{}_saturated", role as usize);
            fixture.write_bin(&saturated, &capacity_retry_saturated_source(&block));
            fixture.run_cargo(doc.path, role, &block.heading, block.line, &saturated);
            fixture.cleanup();
            continue;
        }
        let source = if role == Role::StopHelper {
            format!(
                "{}\n#[tokio::main]\nasync fn main() {{\n    let service = RayonExecutorService::builder().num_threads(1).build().unwrap();\n    stop_without_blocking_runtime(&service).await.unwrap();\n    assert!(service.is_terminated());\n}}\n",
                block.code
            )
        } else {
            block.code
        };
        let bin = format!("example_{}", role as usize);
        fixture.write_bin(&bin, &source);
        if fixture.source == DependencySource::Registry {
            fixture.verify_registry_resolution(doc.path, role, &block.heading, block.line);
        }
        fixture.run_cargo(doc.path, role, &block.heading, block.line, &bin);
        fixture.cleanup();
    }
}

fn capacity_retry_saturated_source(block: &Block) -> String {
    let helpers = block.code.split("#[tokio::main]").next().unwrap_or(&block.code);
    format!(
        "{helpers}\n#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {{\n    use std::time::Duration;\n    let service = RayonExecutorService::builder().num_threads(1).task_capacity(1).build()?;\n    let (started_tx, started_rx) = std::sync::mpsc::channel();\n    let (release_tx, release_rx) = std::sync::mpsc::channel();\n    let blocker = service.try_submit_callable(move || {{ started_tx.send(()).unwrap(); release_rx.recv_timeout(Duration::from_secs(10)).unwrap(); Ok::<(), std::io::Error>(()) }}).unwrap();\n    started_rx.recv_timeout(Duration::from_secs(10))?;\n    let waiting = submit_after_capacity(&service, vec![8, 13, 21, 34].into_boxed_slice());\n    tokio::pin!(waiting);\n    let was_pending = std::future::poll_fn(|cx| std::task::Poll::Ready(matches!(std::future::Future::poll(waiting.as_mut(), cx), std::task::Poll::Pending))).await;\n    assert!(was_pending, \"saturated submission should wait before capacity is released\");\n    release_tx.send(())?;\n    blocker.await?;\n    assert_eq!(waiting.await?, 76);\n    service.shutdown(); service.await_termination().await; Ok(())\n}}\n"
    )
}

fn capacity_retry_with_invalid_main(block: &Block) -> String {
    let helpers = block.code.split("#[tokio::main]").next().unwrap_or(&block.code);
    format!("{helpers}\n#[tokio::main]\nasync fn main() {{ missing_capacity_retry_symbol(); }}\n")
}

struct Fixture {
    root: PathBuf,
    target: PathBuf,
    asynchronous: bool,
    source: DependencySource,
    preserve_on_drop: bool,
}

struct BuildCache {
    root: PathBuf,
    target: PathBuf,
}

impl BuildCache {
    fn new() -> Self {
        let temp = std::env::temp_dir();
        let root = loop {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let candidate = temp.join(format!("rre-doc-build-cache-{}-{id}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create documentation build cache {}: {error}", candidate.display()),
            }
        };
        let target = root.join("target");
        if let Err(error) = fs::create_dir(&target) {
            let _ = fs::remove_dir_all(&root);
            panic!("create documentation build cache {}: {error}", target.display());
        }
        Self { root, target }
    }
}

impl Drop for BuildCache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(asynchronous: bool, cache: &BuildCache, source: DependencySource) -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("rre-doc-examples-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join("src/bin")).expect("create temporary documentation fixture");
        Self {
            target: cache.target.clone(),
            root,
            asynchronous,
            source,
            preserve_on_drop: source == DependencySource::Registry,
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
        let features = documented_dependency_features(version_source, "qubit-rayon-executor", source_path);
        if asynchronous {
            let documented = async_config.expect("async configuration was required above");
            if !documented.code.contains("rt-multi-thread") {
                panic!("{source_path}: Tokio runtime feature is absent from async configuration");
            }
        }
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
        let manifest = fixture_manifest(&version, &executor_version, &features, tokio, self.source);
        fs::write(self.root.join("Cargo.toml"), manifest).expect("write fixture manifest");
        assert_eq!(self.asynchronous, asynchronous);
    }

    fn write_bin(&self, name: &str, source: &str) {
        fs::write(self.root.join("src/bin").join(format!("{name}.rs")), source).expect("write extracted example");
    }

    fn run_cargo(&self, document: &str, role: Role, heading: &str, line: usize, bin: &str) {
        let output = self.run_cargo_output(document, role, heading, line, bin);
        if !output.status.success() {
            panic!(
                "{document}:{line} ({heading}) {} failed\nstdout:\n{}\nstderr:\n{}",
                role.name(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn verify_registry_resolution(&self, document: &str, role: Role, heading: &str, line: usize) {
        let mut command = Command::new("cargo");
        command
            .arg("metadata")
            .arg("--format-version")
            .arg("1")
            .arg("--manifest-path")
            .arg(self.root.join("Cargo.toml"));
        let output = run_bounded(command, document, role, heading, line);
        if !output.status.success() {
            panic!(
                "{document}:{line} ({heading}) {} registry metadata failed\nfixture: {}\nstdout:\n{}\nstderr:\n{}",
                role.name(),
                self.root.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{document}:{line} ({heading}) {} invalid cargo metadata JSON: {error}\nfixture: {}",
                role.name(),
                self.root.display()
            )
        });
        let expected = [("qubit-rayon-executor", None), ("qubit-executor", None)];
        let packages = registry_packages(&metadata, &expected).unwrap_or_else(|error| {
            panic!(
                "{document}:{line} ({heading}) {} registry resolution rejected: {error}\nfixture: {}",
                role.name(),
                self.root.display()
            )
        });
        eprintln!(
            "registry resolution {document}:{line} ({}) fixture={} {}",
            role.name(),
            self.root.display(),
            packages
                .iter()
                .map(|(name, version, features)| format!("{name} {version} features={features:?}"))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    fn cleanup(&mut self) {
        self.preserve_on_drop = false;
        fs::remove_dir_all(&self.root).unwrap_or_else(|error| {
            panic!(
                "remove successful documentation fixture {}: {error}",
                self.root.display()
            )
        });
    }

    fn run_cargo_output(
        &self,
        document: &str,
        role: Role,
        heading: &str,
        line: usize,
        bin: &str,
    ) -> std::process::Output {
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
        run_bounded(command, document, role, heading, line)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.preserve_on_drop {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn run_bounded(mut command: Command, document: &str, role: Role, heading: &str, line: usize) -> std::process::Output {
    let result = crate::common::subprocess::run_bounded_output(&mut command, TIMEOUT).unwrap_or_else(|error| {
        panic!(
            "{document}:{line} ({heading}) {}: failed to run child process: {error}",
            role.name()
        )
    });
    if result.timed_out {
        panic!(
            "{document}:{line} ({heading}) {} exceeded {} seconds\nstdout:\n{}\nstderr:\n{}",
            role.name(),
            TIMEOUT.as_secs(),
            String::from_utf8_lossy(&result.output.stdout),
            String::from_utf8_lossy(&result.output.stderr)
        );
    }
    result.output
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

fn documented_dependency_features(toml: &str, name: &str, document: &str) -> Vec<String> {
    for line in toml.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != name {
            continue;
        }
        let Some(features) = value.split_once("features").map(|(_, rest)| rest) else {
            return Vec::new();
        };
        let array = features
            .split_once('[')
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split_once(']').map(|(array, _)| array))
            .unwrap_or_else(|| panic!("{document}: documented {name} features must be a TOML array"));
        return array
            .split(',')
            .map(str::trim)
            .filter(|feature| !feature.is_empty())
            .map(|feature| {
                let feature = feature.trim_matches('"');
                if feature.is_empty() || feature.contains('"') {
                    panic!("{document}: invalid documented {name} feature {feature:?}");
                }
                feature.to_owned()
            })
            .collect();
    }
    panic!("{document}: documented TOML has no {name} dependency")
}

fn fixture_manifest(
    version: &str,
    executor_version: &str,
    features: &[String],
    tokio: &str,
    source: DependencySource,
) -> String {
    let features = if features.is_empty() {
        String::new()
    } else {
        format!(
            ", features = [{}]",
            features
                .iter()
                .map(|feature| format!("\"{feature}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let (rayon, patch) = match source {
        DependencySource::Source => (
            format!(
                "qubit-rayon-executor = {{ version = \"{version}\", path = \"{}\"{features} }}",
                project_root().display()
            ),
            format!(
                "[patch.crates-io]\nqubit-executor = {{ path = \"{}\" }}\n",
                project_root().join("../rs-executor").display()
            ),
        ),
        DependencySource::Registry => (
            format!("qubit-rayon-executor = {{ version = \"{version}\"{features} }}"),
            String::new(),
        ),
    };
    format!(
        "[package]\nname = \"doc-example-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nqubit-executor = \"{executor_version}\"\n{rayon}\n{tokio}\n{patch}"
    )
}

fn registry_packages(
    metadata: &serde_json::Value,
    expected: &[(&str, Option<&str>)],
) -> Result<Vec<(String, String, Vec<String>)>, String> {
    let packages = metadata["packages"].as_array().ok_or("metadata packages are missing")?;
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("metadata resolve nodes are missing")?;
    let root_id = metadata["resolve"]["root"]
        .as_str()
        .ok_or("metadata resolve root is missing")?;
    let root_node = nodes
        .iter()
        .find(|node| node["id"].as_str() == Some(root_id))
        .ok_or("fixture root node is missing")?;
    let mut resolved = Vec::new();
    for (name, expected_version) in expected {
        let metadata_name = name.replace('-', "_");
        let dependency = root_node["deps"]
            .as_array()
            .and_then(|deps| {
                deps.iter()
                    .find(|dependency| dependency["name"].as_str() == Some(metadata_name.as_str()))
            })
            .ok_or_else(|| format!("fixture does not resolve required dependency {name}"))?;
        let package_id = dependency["pkg"]
            .as_str()
            .ok_or_else(|| format!("{name} resolved package id is missing"))?;
        let package = packages
            .iter()
            .find(|package| package["id"].as_str() == Some(package_id))
            .ok_or_else(|| format!("metadata package for {name} is missing"))?;
        if package["name"].as_str() != Some(*name) {
            return Err(format!(
                "resolved package {package_id} is named {:?}, expected {name}",
                package["name"].as_str()
            ));
        }
        let source = package["source"]
            .as_str()
            .ok_or_else(|| format!("{name} {package_id} has no registry source"))?;
        if source != "registry+https://github.com/rust-lang/crates.io-index" {
            return Err(format!("{name} resolved from {source}, expected crates.io registry"));
        }
        let version = package["version"]
            .as_str()
            .ok_or_else(|| format!("{name} resolved version is missing"))?;
        if let Some(expected_version) = expected_version
            && version != *expected_version
        {
            return Err(format!(
                "{name} resolved version {version}, expected {expected_version}"
            ));
        }
        let node = nodes
            .iter()
            .find(|node| node["id"].as_str() == Some(package_id))
            .ok_or_else(|| format!("resolve node for {name} {version} is missing"))?;
        let features = node["features"]
            .as_array()
            .ok_or_else(|| format!("features for {name} {version} are missing"))?
            .iter()
            .map(|feature| {
                feature
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("invalid feature for {name} {version}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        resolved.push(((*name).to_owned(), version.to_owned(), features));
    }
    Ok(resolved)
}

fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(test)]
mod subprocess_error_tests {
    use std::panic::AssertUnwindSafe;
    use std::process::Command;

    use super::BuildCache;
    use super::DependencySource;
    use super::Fixture;
    use super::Role;
    use super::run_bounded;

    #[test]
    fn test_spawn_error_keeps_document_location_and_role() {
        let executable = std::env::temp_dir().join(format!("missing-doc-example-{}", std::process::id()));
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            run_bounded(
                Command::new(executable),
                "guide.md",
                Role::CapacityRetry,
                "Capacity retry",
                42,
            )
        }));

        let panic = result.expect_err("missing executable should panic with context");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("panic message should be text");
        assert!(message.contains("guide.md:42 (Capacity retry) capacity retry program"));
        assert!(message.contains("failed to run child process"));
        assert!(
            !message.contains("stdout summary:"),
            "spawn failure has no child output"
        );
        assert!(
            !message.contains("stderr summary:"),
            "spawn failure has no child output"
        );
    }

    #[test]
    fn test_documentation_fixtures_share_cache_without_sharing_sources() {
        let cache = BuildCache::new();
        let cache_root = cache.root.clone();
        let target = cache.target.clone();
        let first = Fixture::new(false, &cache, DependencySource::Source);
        let second = Fixture::new(true, &cache, DependencySource::Source);
        let first_root = first.root.clone();
        let second_root = second.root.clone();
        assert_ne!(first_root, second_root);
        assert_eq!(first.target, target);
        assert_eq!(second.target, target);
        assert!(target.starts_with(&cache_root));
        drop(first);
        assert!(!first_root.exists());
        assert!(second_root.exists());
        assert!(cache_root.exists());
        assert!(target.exists());
        drop(second);
        assert!(!second_root.exists());
        drop(cache);
        assert!(!cache_root.exists());
    }
}

#[cfg(test)]
mod dependency_source_tests {
    use super::DependencySource;
    use super::documented_dependency_features;
    use super::fixture_manifest;
    use super::registry_packages;

    fn metadata(source: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "resolve": {
                "root": "fixture 0.1.0 (path+file:///tmp/fixture)",
                "nodes": [
                    {"id":"fixture 0.1.0 (path+file:///tmp/fixture)","deps":[
                        {"name":"qubit_rayon_executor","pkg":"qubit-rayon-executor 0.9.0 (registry+https://github.com/rust-lang/crates.io-index)"},
                        {"name":"qubit_executor","pkg":"qubit-executor 0.9.2 (registry+https://github.com/rust-lang/crates.io-index)"}
                    ],"features":[]},
                    {"id":"qubit-rayon-executor 0.9.0 (registry+https://github.com/rust-lang/crates.io-index)","features":["async-wait"]},
                    {"id":"qubit-executor 0.9.2 (registry+https://github.com/rust-lang/crates.io-index)","features":[]}
                ]
            },
            "packages": [
                {"id":"qubit-rayon-executor 0.9.0 (registry+https://github.com/rust-lang/crates.io-index)","name":"qubit-rayon-executor","version":version,"source":source},
                {"id":"qubit-executor 0.9.2 (registry+https://github.com/rust-lang/crates.io-index)","name":"qubit-executor","version":"0.9.2","source":"registry+https://github.com/rust-lang/crates.io-index"}
            ]
        })
    }

    #[test]
    fn test_source_manifest_keeps_local_path_and_executor_patch() {
        let manifest = fixture_manifest("0.9", "^0.9.2", &[], "", DependencySource::Source);
        assert!(manifest.contains("qubit-rayon-executor = { version = \"0.9\", path ="));
        assert!(manifest.contains("[patch.crates-io]\nqubit-executor = { path ="));
    }

    #[test]
    fn test_registry_manifest_keeps_versions_features_and_tokio_without_overrides() {
        let doc_toml =
            "qubit-executor = \"0.9.2\"\nqubit-rayon-executor = { version = \"0.9\", features = [\"async-wait\"] }\n";
        let features = documented_dependency_features(doc_toml, "qubit-rayon-executor", "guide.md");
        assert_eq!(features, ["async-wait"]);
        let manifest = fixture_manifest(
            "0.9",
            "^0.9.2",
            &features,
            "tokio = { version = \"1.54\", features = [\"macros\", \"rt-multi-thread\"] }",
            DependencySource::Registry,
        );
        assert!(manifest.contains("qubit-executor = \"^0.9.2\""));
        assert!(manifest.contains("qubit-rayon-executor = { version = \"0.9\", features = [\"async-wait\"] }"));
        assert!(manifest.contains("tokio = { version = \"1.54\", features = [\"macros\", \"rt-multi-thread\"] }"));
        assert!(!manifest.contains("path ="));
        assert!(!manifest.contains("[patch"));
        assert!(!manifest.contains("[replace"));

        let changed_doc_toml =
            "qubit-rayon-executor = { version = \"0.9\", features = [\"async-wait\", \"custom-feature\"] }";
        let changed_features = documented_dependency_features(changed_doc_toml, "qubit-rayon-executor", "guide.md");
        let changed_manifest = fixture_manifest("0.9", "^0.9.2", &changed_features, "", DependencySource::Registry);
        assert!(changed_manifest.contains("features = [\"async-wait\", \"custom-feature\"]"));
    }

    #[test]
    fn test_registry_metadata_accepts_only_crates_io_packages_and_expected_version() {
        let valid = metadata("registry+https://github.com/rust-lang/crates.io-index", "0.9.0");
        let resolved = registry_packages(&valid, &[("qubit-rayon-executor", None), ("qubit-executor", None)])
            .expect("crates.io dependencies should pass");
        assert_eq!(resolved[0].1, "0.9.0");
        assert_eq!(resolved[0].2, ["async-wait"]);

        let path = metadata("path+file:///tmp/rs-rayon-executor", "0.9.0");
        assert!(
            registry_packages(&path, &[("qubit-rayon-executor", None), ("qubit-executor", None)])
                .expect_err("path package should be rejected")
                .contains("path+file")
        );
        let custom_registry = metadata("registry+https://github.com/rust-lang/crates.io-index.example", "0.9.0");
        assert!(
            registry_packages(
                &custom_registry,
                &[("qubit-rayon-executor", None), ("qubit-executor", None)]
            )
            .expect_err("a custom registry must not pass as crates.io")
            .contains("registry+https://github.com/rust-lang/crates.io-index.example")
        );
        assert!(
            registry_packages(&valid, &[("qubit-rayon-executor", None), ("missing-executor", None)])
                .expect_err("missing package should be rejected")
                .contains("missing-executor")
        );
        assert!(
            registry_packages(
                &valid,
                &[("qubit-rayon-executor", Some("0.9.1")), ("qubit-executor", None)]
            )
            .expect_err("a version different from the requested acceptance version must fail")
            .contains("resolved version 0.9.0, expected 0.9.1")
        );
    }
}
