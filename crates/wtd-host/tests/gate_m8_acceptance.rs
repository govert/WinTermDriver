//! M8 Acceptance Gate — Shipped and documented (§37.5)
//!
//! This test proves the M8 milestone: the project is shipped and documented.
//!
//! Criteria validated:
//!   1. GitHub Actions release workflow exists, triggers on version tags,
//!      builds binaries, generates checksums, and creates a GitHub Release.
//!   2. README contains complete run instructions that work from a fresh download
//!      (install, create workspace, open, interact via CLI, launch UI).
//!   3. README includes screenshots of the running application.
//!   4. docs/AGENT_GUIDE.md exists with worked examples for programmatic use.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // Integration tests run from the crate directory; repo root is two levels up
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("could not find repo root")
        .to_path_buf()
}

// ---------- Criterion 1: GitHub Release workflow ----------

#[test]
fn criterion_1_release_workflow_exists() {
    let root = repo_root();
    let workflow = root.join(".github/workflows/release.yml");
    assert!(
        workflow.exists(),
        "Release workflow not found at .github/workflows/release.yml"
    );

    let content = fs::read_to_string(&workflow).expect("failed to read release.yml");

    // Triggers on version tags
    assert!(
        content.contains("v*") || content.contains("tags:"),
        "Release workflow must trigger on version tags"
    );

    // Builds release binaries
    assert!(
        content.contains("cargo build") || content.contains("package-release"),
        "Release workflow must build binaries"
    );

    // Generates SHA-256 checksums
    assert!(
        content.contains("sha256") || content.contains("SHA-256"),
        "Release workflow must generate checksums"
    );

    // Creates a GitHub Release
    assert!(
        content.contains("gh release create"),
        "Release workflow must create a GitHub Release"
    );

    // Uploads the archive and checksum as release assets
    assert!(
        content.contains(".zip") && content.contains(".sha256"),
        "Release workflow must upload zip archive and checksum file"
    );
}

#[test]
fn criterion_1_packaging_script_exists() {
    let root = repo_root();
    let script = root.join("tools/package-release.sh");
    assert!(
        script.exists(),
        "Packaging script not found at tools/package-release.sh"
    );

    let content = fs::read_to_string(&script).expect("failed to read package-release.sh");

    // Builds all three binaries
    assert!(
        content.contains("wtd-host") && content.contains("wtd-ui") && content.contains("wtd"),
        "Packaging script must build wtd-host, wtd-ui, and wtd binaries"
    );

    // Creates a zip archive
    assert!(
        content.contains(".zip"),
        "Packaging script must create a zip archive"
    );

    // Bundles README and LICENSE
    assert!(
        content.contains("README.md") && content.contains("LICENSE.md"),
        "Packaging script must bundle README.md and LICENSE.md"
    );
}

// ---------- Criterion 2: README with complete run instructions ----------

#[test]
fn criterion_2_readme_has_install_instructions() {
    let root = repo_root();
    let readme = fs::read_to_string(root.join("README.md")).expect("README.md not found");

    // From GitHub Releases download path
    assert!(
        readme.contains("GitHub Releases") || readme.contains("Releases"),
        "README must document installation from GitHub Releases"
    );
    assert!(
        readme.contains("wtd-") && readme.contains("windows-x86_64.zip"),
        "README must reference the release archive filename pattern"
    );

    // From source build path
    assert!(
        readme.contains("cargo build"),
        "README must document building from source"
    );
    assert!(
        readme.contains("rustup.rs") || readme.contains("Rust"),
        "README must mention Rust toolchain requirement"
    );
}

#[test]
fn criterion_2_readme_has_quickstart() {
    let root = repo_root();
    let readme = fs::read_to_string(root.join("README.md")).expect("README.md not found");

    // Quickstart section exists
    assert!(
        readme.contains("Quickstart") || readme.contains("quickstart"),
        "README must have a Quickstart section"
    );

    // Step-by-step instructions for the full lifecycle
    assert!(
        readme.contains("wtd open"),
        "README quickstart must show how to open a workspace"
    );
    assert!(
        readme.contains("wtd send"),
        "README quickstart must show how to send commands"
    );
    assert!(
        readme.contains("wtd capture"),
        "README quickstart must show how to capture output"
    );
    assert!(
        readme.contains("wtd close"),
        "README quickstart must show how to close a workspace"
    );
    assert!(
        readme.contains("wtd-ui"),
        "README quickstart must mention the UI process"
    );
}

#[test]
fn criterion_2_readme_has_workspace_yaml_example() {
    let root = repo_root();
    let readme = fs::read_to_string(root.join("README.md")).expect("README.md not found");

    // Shows a workspace YAML example
    assert!(
        readme.contains("version: 1"),
        "README must include a workspace YAML example with version field"
    );
    assert!(
        readme.contains("tabs:") && readme.contains("layout:"),
        "README YAML example must show tabs and layout structure"
    );
    assert!(
        readme.contains("profile:"),
        "README YAML example must show profile configuration"
    );
}

#[test]
fn criterion_2_readme_has_cli_reference() {
    let root = repo_root();
    let readme = fs::read_to_string(root.join("README.md")).expect("README.md not found");

    // CLI reference section
    assert!(
        readme.contains("CLI Reference") || readme.contains("Commands"),
        "README must have a CLI reference section"
    );
    assert!(
        readme.contains("--json"),
        "README must document the --json flag"
    );
    assert!(
        readme.contains("Target paths") || readme.contains("target path"),
        "README must explain target path addressing"
    );
}

#[test]
fn criterion_2_readme_has_troubleshooting() {
    let root = repo_root();
    let readme = fs::read_to_string(root.join("README.md")).expect("README.md not found");

    assert!(
        readme.contains("Troubleshooting") || readme.contains("troubleshooting"),
        "README must have a troubleshooting section"
    );
    assert!(
        readme.contains("ConPTY"),
        "README troubleshooting must mention ConPTY requirements"
    );
}

// ---------- Criterion 3: README includes screenshots ----------

#[test]
fn criterion_3_screenshots_exist() {
    let root = repo_root();
    let images_dir = root.join("docs/images");
    assert!(
        images_dir.is_dir(),
        "docs/images/ directory must exist"
    );

    let required_screenshots = [
        "workspace-overview.png",
        "command-palette.png",
        "prefix-chord.png",
        "failed-pane.png",
    ];

    for name in &required_screenshots {
        let path = images_dir.join(name);
        assert!(
            path.exists(),
            "Screenshot missing: docs/images/{name}"
        );

        let metadata = fs::metadata(&path).unwrap();
        assert!(
            metadata.len() > 1024,
            "Screenshot docs/images/{name} is suspiciously small ({} bytes)",
            metadata.len()
        );
    }
}

#[test]
fn criterion_3_readme_references_screenshots() {
    let root = repo_root();
    let readme = fs::read_to_string(root.join("README.md")).expect("README.md not found");

    let expected_refs = [
        "docs/images/workspace-overview.png",
        "docs/images/command-palette.png",
        "docs/images/prefix-chord.png",
        "docs/images/failed-pane.png",
    ];

    for img_ref in &expected_refs {
        assert!(
            readme.contains(img_ref),
            "README must reference screenshot: {img_ref}"
        );
    }

    // Verify they use markdown image syntax
    let image_link_count = readme.matches("![").count();
    assert!(
        image_link_count >= 4,
        "README must have at least 4 markdown image links, found {image_link_count}"
    );
}

// ---------- Criterion 4: Agent guidance document ----------

#[test]
fn criterion_4_agent_guide_exists() {
    let root = repo_root();
    let guide = root.join("docs/AGENT_GUIDE.md");
    assert!(
        guide.exists(),
        "docs/AGENT_GUIDE.md must exist"
    );

    let content = fs::read_to_string(&guide).expect("failed to read AGENT_GUIDE.md");

    // Must be substantial (not a stub)
    assert!(
        content.len() > 5000,
        "AGENT_GUIDE.md is too short ({} bytes) — expected a substantial document",
        content.len()
    );
}

#[test]
fn criterion_4_agent_guide_has_architecture_overview() {
    let root = repo_root();
    let content =
        fs::read_to_string(root.join("docs/AGENT_GUIDE.md")).expect("failed to read AGENT_GUIDE.md");

    assert!(
        content.contains("wtd-host") && content.contains("wtd-ui") && content.contains("wtd.exe"),
        "Agent guide must describe all three processes"
    );
    assert!(
        content.contains("named pipe") || content.contains("IPC"),
        "Agent guide must explain the IPC mechanism"
    );
}

#[test]
fn criterion_4_agent_guide_has_workspace_yaml_examples() {
    let root = repo_root();
    let content =
        fs::read_to_string(root.join("docs/AGENT_GUIDE.md")).expect("failed to read AGENT_GUIDE.md");

    assert!(
        content.contains("version: 1") && content.contains("name:"),
        "Agent guide must include workspace YAML examples"
    );
    assert!(
        content.contains("profile:"),
        "Agent guide YAML examples must show profile usage"
    );
}

#[test]
fn criterion_4_agent_guide_has_cli_examples() {
    let root = repo_root();
    let content =
        fs::read_to_string(root.join("docs/AGENT_GUIDE.md")).expect("failed to read AGENT_GUIDE.md");

    // Core CLI commands for agents
    assert!(
        content.contains("wtd open") && content.contains("wtd close"),
        "Agent guide must show workspace open/close commands"
    );
    assert!(
        content.contains("wtd send") && content.contains("wtd capture"),
        "Agent guide must show send/capture commands"
    );
    assert!(
        content.contains("wtd list") && content.contains("wtd inspect"),
        "Agent guide must show list/inspect commands"
    );
    assert!(
        content.contains("--json"),
        "Agent guide must document JSON output mode"
    );
}

#[test]
fn criterion_4_agent_guide_has_polling_patterns() {
    let root = repo_root();
    let content =
        fs::read_to_string(root.join("docs/AGENT_GUIDE.md")).expect("failed to read AGENT_GUIDE.md");

    // Polling/timing guidance
    assert!(
        content.contains("poll") || content.contains("Poll"),
        "Agent guide must explain polling patterns for async output"
    );
    assert!(
        content.contains("fence") || content.contains("marker") || content.contains("DONE"),
        "Agent guide must describe output fencing with markers"
    );
}

#[test]
fn criterion_4_agent_guide_has_error_handling() {
    let root = repo_root();
    let content =
        fs::read_to_string(root.join("docs/AGENT_GUIDE.md")).expect("failed to read AGENT_GUIDE.md");

    assert!(
        content.contains("exit code") || content.contains("Exit code"),
        "Agent guide must document exit codes"
    );
    assert!(
        content.contains("error") || content.contains("Error"),
        "Agent guide must cover error handling"
    );
}

#[test]
fn criterion_4_agent_guide_has_worked_orchestration_example() {
    let root = repo_root();
    let content =
        fs::read_to_string(root.join("docs/AGENT_GUIDE.md")).expect("failed to read AGENT_GUIDE.md");

    // Must have at least one complete multi-step script example
    assert!(
        content.contains("#!/usr/bin/env bash") || content.contains("#!/bin/bash"),
        "Agent guide must include at least one complete shell script example"
    );

    // The orchestration example should show the full lifecycle
    assert!(
        content.contains("wtd open") && content.contains("wtd send") && content.contains("wtd close"),
        "Agent guide orchestration example must show full workspace lifecycle"
    );
}
