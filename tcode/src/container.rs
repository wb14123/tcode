use anyhow::{Result, bail};
use llm_rs::tool::ContainerConfig;

/// Validate that a container is ready for use with tcode.
///
/// Checks in order:
/// 1. Runtime CLI (`docker`/`podman`) is available
/// 2. Container exists and is running
/// 3. Bash is available inside the container
/// 4. Current directory is mounted at the same path inside the container
pub async fn validate_container(name: &str, runtime: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("Container name must not be empty.");
    }

    // 1. Runtime CLI available
    let version_result = tokio::process::Command::new(runtime)
        .arg("--version")
        .output()
        .await;
    match version_result {
        Ok(output) if output.status.success() => {}
        _ => {
            bail!("{runtime} is not available. Make sure {runtime} is installed and in PATH.");
        }
    }

    // 2. Container is running
    let inspect_result = tokio::process::Command::new(runtime)
        .args(["inspect", "--format", "{{.State.Status}}", name])
        .output()
        .await;
    match inspect_result {
        Ok(output) if output.status.success() => {
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if status != "running" {
                bail!(
                    "Container '{name}' is not running (status: {status}). Start it before running tcode."
                );
            }
        }
        _ => {
            bail!(
                "Container '{name}' does not exist. Make sure it is started before running tcode."
            );
        }
    }

    // 3. Bash available inside container
    let bash_result = tokio::process::Command::new(runtime)
        .args(["exec", name, "bash", "--version"])
        .output()
        .await;
    match bash_result {
        Ok(output) if output.status.success() => {}
        _ => {
            bail!(
                "bash is not available inside container '{name}'. Install bash in the container image."
            );
        }
    }

    // 4. Same-path mount check
    let cwd = std::env::current_dir()?;
    let marker_name = format!(".tcode-mount-check-{}", uuid::Uuid::new_v4());
    let marker_path = cwd.join(&marker_name);

    // Create marker file on host
    std::fs::write(&marker_path, "")?;

    // Ensure cleanup on all paths (including early returns / panics)
    struct MarkerGuard {
        path: std::path::PathBuf,
    }
    impl Drop for MarkerGuard {
        fn drop(&mut self) {
            if let Err(e) = std::fs::remove_file(&self.path) {
                eprintln!(
                    "Warning: failed to remove mount-check marker file {}: {}",
                    self.path.display(),
                    e
                );
            }
        }
    }
    let _guard = MarkerGuard {
        path: marker_path.clone(),
    };

    let marker_path_str = marker_path.to_string_lossy();
    let test_result = tokio::process::Command::new(runtime)
        .args(["exec", name, "test", "-f", &marker_path_str])
        .output()
        .await;
    match test_result {
        Ok(output) if output.status.success() => {}
        _ => {
            let cwd_display = cwd.display();
            bail!(
                "Current directory '{}' is not mounted at the same path inside container '{}'. \
                 Mount it with: -v {}:{}",
                cwd_display,
                name,
                cwd_display,
                cwd_display
            );
        }
    }

    Ok(())
}

/// Build a `ContainerConfig` from runtime info and host environment.
pub fn build_container_config(name: &str, runtime: &str) -> ContainerConfig {
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    ContainerConfig {
        name: name.to_string(),
        runtime: runtime.to_string(),
        uid,
        gid,
        home,
    }
}
