//! Toggle RTK hook rewriting on/off (global + per-project).
//!
//! - Global toggle: `hooks.enabled` in `~/.config/rtk/config.toml`
//! - Per-project toggle: `.rtk/disabled` marker file in project root

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Marker filename within a project's `.rtk/` directory.
const DISABLED_MARKER: &str = "disabled";

// ---------------------------------------------------------------------------
// Project-level helpers
// ---------------------------------------------------------------------------

fn marker_path_in(root: &std::path::Path) -> PathBuf {
    root.join(".rtk").join(DISABLED_MARKER)
}

fn is_disabled_in(root: &std::path::Path) -> bool {
    marker_path_in(root).exists()
}

/// Walk from `start` up through ancestor directories looking for `.rtk/disabled`.
/// Returns true if any ancestor has the marker. Matches git's discovery semantics:
/// run RTK from any subdirectory of the project and still honor the marker at the root.
fn is_disabled_walking_up(start: &std::path::Path) -> bool {
    for dir in start.ancestors() {
        if is_disabled_in(dir) {
            return true;
        }
    }
    false
}

pub fn is_project_disabled() -> bool {
    std::env::current_dir()
        .map(|cwd| is_disabled_walking_up(&cwd))
        .unwrap_or(false)
}

fn disable_in(root: &std::path::Path) -> Result<()> {
    let path = marker_path_in(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    std::fs::write(&path, "").with_context(|| format!("Failed to create {}", path.display()))?;
    Ok(())
}

fn enable_in(root: &std::path::Path) -> Result<()> {
    let path = marker_path_in(root);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Global-level helpers
// ---------------------------------------------------------------------------

fn set_global_enabled(enabled: bool) -> Result<()> {
    let mut config = crate::core::config::Config::load().context("Failed to load config")?;
    config.hooks.enabled = enabled;
    config.save().context("Failed to save config")
}

// ---------------------------------------------------------------------------
// Public CLI entry points
// ---------------------------------------------------------------------------

pub fn run_disable(project: bool) -> Result<()> {
    if project {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        disable_in(&cwd)?;
        println!("RTK hook rewriting disabled for this project.");
        println!("(Add .rtk/disabled to .gitignore to keep it local)");
    } else {
        set_global_enabled(false)?;
        println!("RTK hook rewriting disabled globally.");
    }
    Ok(())
}

pub fn run_enable(project: bool) -> Result<()> {
    if project {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        enable_in(&cwd)?;
        println!("RTK hook rewriting enabled for this project.");
    } else {
        set_global_enabled(true)?;
        println!("RTK hook rewriting enabled globally.");
    }
    Ok(())
}

pub fn run_status() -> Result<()> {
    let global_enabled = crate::core::config::Config::load()
        .map(|c| c.hooks.enabled)
        .unwrap_or(true);
    let project_disabled = is_project_disabled();

    let effective = global_enabled && !project_disabled;

    println!("RTK hook rewriting status:");
    println!(
        "  Global:  {}",
        if global_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  Project: {}",
        if project_disabled {
            "disabled (.rtk/disabled present)"
        } else {
            "enabled (no marker file)"
        }
    );
    println!();
    println!(
        "  Effective: {}",
        if effective {
            "ACTIVE"
        } else {
            "INACTIVE (passthrough)"
        }
    );

    if !global_enabled && project_disabled {
        println!("  Note: Both global and project-level are disabled.");
    } else if !global_enabled {
        println!("  Note: Run `rtk enable` to re-enable globally.");
    } else if project_disabled {
        println!("  Note: Run `rtk enable --project` to re-enable for this project.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_not_disabled_by_default() {
        let temp = TempDir::new().expect("tempdir");
        assert!(!is_disabled_in(temp.path()));
    }

    #[test]
    fn test_disable_creates_marker() {
        let temp = TempDir::new().expect("tempdir");
        disable_in(temp.path()).expect("disable");
        assert!(is_disabled_in(temp.path()));
        assert!(temp.path().join(".rtk/disabled").exists());
    }

    #[test]
    fn test_enable_removes_marker() {
        let temp = TempDir::new().expect("tempdir");
        disable_in(temp.path()).expect("disable");
        assert!(is_disabled_in(temp.path()));
        enable_in(temp.path()).expect("enable");
        assert!(!is_disabled_in(temp.path()));
    }

    #[test]
    fn test_enable_when_not_disabled_is_noop() {
        let temp = TempDir::new().expect("tempdir");
        enable_in(temp.path()).expect("enable");
        assert!(!is_disabled_in(temp.path()));
    }

    #[test]
    fn test_disable_creates_rtk_dir() {
        let temp = TempDir::new().expect("tempdir");
        assert!(!temp.path().join(".rtk").exists());
        disable_in(temp.path()).expect("disable");
        assert!(temp.path().join(".rtk").exists());
    }

    #[test]
    fn test_walks_up_from_subdirectory() {
        // Marker at project root should be found when CWD is a subdirectory.
        let temp = TempDir::new().expect("tempdir");
        disable_in(temp.path()).expect("disable at root");
        let subdir = temp.path().join("src").join("cmds").join("apple");
        std::fs::create_dir_all(&subdir).expect("mkdir");
        assert!(is_disabled_walking_up(&subdir));
    }

    #[test]
    fn test_walks_up_stops_at_filesystem_root() {
        // No marker anywhere — walking up all the way to `/` must return false,
        // not crash or loop forever.
        let temp = TempDir::new().expect("tempdir");
        let subdir = temp.path().join("deeply").join("nested");
        std::fs::create_dir_all(&subdir).expect("mkdir");
        assert!(!is_disabled_walking_up(&subdir));
    }

    #[test]
    fn test_walk_up_finds_nearest_marker() {
        // If markers exist at multiple ancestor levels, the walk still returns true.
        let temp = TempDir::new().expect("tempdir");
        disable_in(temp.path()).expect("root marker");
        let subdir = temp.path().join("sub");
        std::fs::create_dir_all(&subdir).expect("mkdir sub");
        disable_in(&subdir).expect("sub marker");
        assert!(is_disabled_walking_up(&subdir));
        let deeper = subdir.join("deeper");
        std::fs::create_dir_all(&deeper).expect("mkdir deeper");
        assert!(is_disabled_walking_up(&deeper));
    }
}
