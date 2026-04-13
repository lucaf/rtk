//! Translates a raw shell command into its RTK-optimized equivalent.

use super::permissions::{check_command, PermissionVerdict};
use crate::discover::registry;
use std::io::Write;

/// Run the `rtk rewrite` command.
///
/// Prints the RTK-rewritten command to stdout and exits with a code that tells
/// the caller how to handle permissions:
///
/// | Exit | Stdout   | Meaning                                                      |
/// |------|----------|--------------------------------------------------------------|
/// | 0    | rewritten| Rewrite allowed — hook may auto-allow the rewritten command. |
/// | 1    | (none)   | No RTK equivalent — hook passes through unchanged.           |
/// | 2    | (none)   | Deny rule matched — hook defers to Claude Code native deny.  |
/// | 3    | rewritten| Ask rule matched — hook rewrites but lets Claude Code prompt.|
pub fn run(cmd: &str) -> anyhow::Result<()> {
    let config = crate::core::config::Config::load().unwrap_or_default();

    // SECURITY: check deny/ask BEFORE rewrite AND BEFORE disable checks, so
    // non-RTK commands are also covered and RTK's deny rules still fire when
    // the rewrite hook is disabled. A user who types `rtk disable` wants to
    // stop auto-rewriting, not to bypass their own security deny rules.
    let verdict = check_command(cmd);

    if verdict == PermissionVerdict::Deny {
        std::process::exit(2);
    }

    // Check global disable — after deny/ask so deny still works.
    if !config.hooks.enabled {
        std::process::exit(1);
    }

    // Check per-project disable (.rtk/disabled marker — walks up CWD ancestors).
    if crate::hooks::toggle::is_project_disabled() {
        std::process::exit(1);
    }

    let excluded = config.hooks.exclude_commands;

    match registry::rewrite_command(cmd, &excluded) {
        Some(rewritten) => match verdict {
            PermissionVerdict::Allow => {
                print!("{}", rewritten);
                let _ = std::io::stdout().flush();
                Ok(())
            }
            PermissionVerdict::Ask | PermissionVerdict::Default => {
                print!("{}", rewritten);
                let _ = std::io::stdout().flush();
                std::process::exit(3);
            }
            PermissionVerdict::Deny => unreachable!(),
        },
        None => {
            // No RTK equivalent. Exit 1 = passthrough.
            // Claude Code independently evaluates its own ask rules on the original cmd.
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_supported_command_succeeds() {
        assert!(registry::rewrite_command("git status", &[]).is_some());
    }

    #[test]
    fn test_run_unsupported_returns_none() {
        assert!(registry::rewrite_command("htop", &[]).is_none());
    }

    #[test]
    fn test_run_already_rtk_returns_some() {
        assert_eq!(
            registry::rewrite_command("rtk git status", &[]),
            Some("rtk git status".into())
        );
    }
}
