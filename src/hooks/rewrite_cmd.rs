//! Translates a raw shell command into its RTK-optimized equivalent.

use super::permissions::{check_command, PermissionVerdict};
use crate::discover::registry;
use std::io::Write;

/// The outcome of evaluating a command for rewriting. Extracted from `run()`
/// so the priority ordering (deny → disable → rewrite) is unit-testable
/// without subprocess boundaries.
#[derive(Debug, PartialEq)]
pub(crate) enum RewriteOutcome {
    /// Exit 2 — RTK deny rule matched (security; overrides disable).
    Deny,
    /// Exit 1 — no RTK equivalent, or rewriting is disabled globally/per-project.
    Passthrough,
    /// Exit 0 — rewrite allowed, stdout = the rewritten command.
    Allow(String),
    /// Exit 3 — rewrite + ask prompt, stdout = the rewritten command.
    AskWithRewrite(String),
}

/// Pure function that computes the rewrite outcome from inputs. Inputs:
/// - `cmd`: the raw shell command
/// - `verdict`: pre-computed permission verdict (so tests can inject it)
/// - `hooks_enabled`: global enable flag from config
/// - `project_disabled`: whether the `.rtk/disabled` marker was found by walking up
/// - `excluded`: list of commands to skip rewriting
pub(crate) fn compute_outcome(
    cmd: &str,
    verdict: PermissionVerdict,
    hooks_enabled: bool,
    project_disabled: bool,
    excluded: &[String],
) -> RewriteOutcome {
    // Deny is highest priority — fires even when hooks are disabled so
    // user security deny rules cannot be bypassed by `rtk disable`.
    if verdict == PermissionVerdict::Deny {
        return RewriteOutcome::Deny;
    }

    // Disable checks: return passthrough without rewriting.
    if !hooks_enabled || project_disabled {
        return RewriteOutcome::Passthrough;
    }

    // Attempt rewrite, combined with ask/allow verdict.
    match registry::rewrite_command(cmd, excluded) {
        Some(rewritten) => match verdict {
            PermissionVerdict::Allow => RewriteOutcome::Allow(rewritten),
            PermissionVerdict::Ask | PermissionVerdict::Default => {
                RewriteOutcome::AskWithRewrite(rewritten)
            }
            PermissionVerdict::Deny => unreachable!("Deny handled above"),
        },
        None => RewriteOutcome::Passthrough,
    }
}

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
///
/// # Disable interactions
///
/// When hooks are disabled globally (`hooks.enabled = false`) or per-project
/// (`.rtk/disabled` marker in any ancestor directory), the above table changes:
///
/// - **Deny still fires** (exit 2). Deny rules are security-critical and run
///   BEFORE the disable check — the user explicitly chose to deny these
///   commands, and RTK honors that regardless of whether rewriting is on.
/// - **Ask is NOT signaled** (exits 1 instead of 3). When RTK is disabled we
///   emit exit 1 (passthrough) even for commands that would have triggered an
///   ask rule. Claude Code may still evaluate its own ask rules on the
///   original command. If users want RTK's ask rules to fire without RTK
///   rewriting the command, they should keep hooks enabled and rely on RTK
///   ask rules rather than disable.
/// - **No rewrite happens** (no exit 0 / 3 with rewritten command).
pub fn run(cmd: &str) -> anyhow::Result<()> {
    let config = crate::core::config::Config::load().unwrap_or_default();
    let verdict = check_command(cmd);
    let project_disabled = crate::hooks::toggle::is_project_disabled();

    match compute_outcome(
        cmd,
        verdict,
        config.hooks.enabled,
        project_disabled,
        &config.hooks.exclude_commands,
    ) {
        RewriteOutcome::Deny => std::process::exit(2),
        RewriteOutcome::Passthrough => std::process::exit(1),
        RewriteOutcome::Allow(rewritten) => {
            print!("{}", rewritten);
            let _ = std::io::stdout().flush();
            Ok(())
        }
        RewriteOutcome::AskWithRewrite(rewritten) => {
            print!("{}", rewritten);
            let _ = std::io::stdout().flush();
            std::process::exit(3);
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

    // ── compute_outcome: priority ordering tests ──────────────────────────

    #[test]
    fn test_outcome_deny_fires_when_hooks_disabled() {
        // SECURITY INVARIANT: deny rules must still be honored even when
        // hooks are globally disabled. Otherwise `rtk disable` silently
        // removes user security protections.
        let outcome = compute_outcome(
            "rm -rf /",
            PermissionVerdict::Deny,
            false, // hooks_enabled = false
            false,
            &[],
        );
        assert_eq!(outcome, RewriteOutcome::Deny);
    }

    #[test]
    fn test_outcome_deny_fires_when_project_disabled() {
        // Same invariant for project-level disable.
        let outcome = compute_outcome(
            "rm -rf /",
            PermissionVerdict::Deny,
            true,
            true, // project_disabled = true
            &[],
        );
        assert_eq!(outcome, RewriteOutcome::Deny);
    }

    #[test]
    fn test_outcome_disabled_returns_passthrough_on_allow() {
        // When disabled, even a normally-rewriteable command must passthrough.
        let outcome = compute_outcome("git status", PermissionVerdict::Allow, false, false, &[]);
        assert_eq!(outcome, RewriteOutcome::Passthrough);
    }

    #[test]
    fn test_outcome_disabled_returns_passthrough_on_ask() {
        // When disabled, ask verdict degrades to passthrough (no exit 3).
        // Documented behavior: Claude Code's own ask rules still apply.
        let outcome = compute_outcome("git status", PermissionVerdict::Ask, false, false, &[]);
        assert_eq!(outcome, RewriteOutcome::Passthrough);
    }

    #[test]
    fn test_outcome_enabled_allow_rewrites() {
        let outcome = compute_outcome("git status", PermissionVerdict::Allow, true, false, &[]);
        match outcome {
            RewriteOutcome::Allow(s) => assert!(s.starts_with("rtk git")),
            _ => panic!("expected Allow, got {:?}", outcome),
        }
    }

    #[test]
    fn test_outcome_enabled_ask_rewrites_with_prompt() {
        let outcome = compute_outcome("git status", PermissionVerdict::Ask, true, false, &[]);
        match outcome {
            RewriteOutcome::AskWithRewrite(s) => assert!(s.starts_with("rtk git")),
            _ => panic!("expected AskWithRewrite, got {:?}", outcome),
        }
    }

    #[test]
    fn test_outcome_unsupported_is_passthrough() {
        let outcome = compute_outcome("htop", PermissionVerdict::Allow, true, false, &[]);
        assert_eq!(outcome, RewriteOutcome::Passthrough);
    }
}
