//! User-facing CI preview state presentation shared by the Java status response
//! and the waiting lobby.  Keeping the mapping here prevents the list/status
//! protocol and lobby chat from drifting apart as lifecycle states are added.

use super::CiPreviewState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CiPreviewStatusMessage {
    /// Stable short label used by the Java server-list/status response.
    pub status_label: &'static str,
    /// Human-readable status sent to a player who is waiting in the lobby.
    pub chat_message: &'static str,
}

/// Return the canonical user-facing label and message for a preview state.
///
/// The `running` state deliberately keeps the historical `online` status-list
/// label: Minecraft clients commonly render that label as the server's
/// availability indicator.  The chat text still names the runtime state.
pub const fn ci_preview_status_message(state: CiPreviewState) -> CiPreviewStatusMessage {
    match state {
        CiPreviewState::Lobby => CiPreviewStatusMessage {
            status_label: "lobby",
            chat_message: "CI preview queued for its first build…",
        },
        CiPreviewState::Queued => CiPreviewStatusMessage {
            status_label: "queued",
            chat_message: "CI preview queued for its first build…",
        },
        CiPreviewState::Fetching => CiPreviewStatusMessage {
            status_label: "fetching",
            chat_message: "Fetching the exact CI commit…",
        },
        CiPreviewState::Patching => CiPreviewStatusMessage {
            status_label: "patching",
            chat_message: "Applying Kurbis integration patches…",
        },
        CiPreviewState::Compiling => CiPreviewStatusMessage {
            status_label: "compiling",
            chat_message: "Compiling the CI preview…",
        },
        CiPreviewState::Packaging => CiPreviewStatusMessage {
            status_label: "packaging",
            chat_message: "Packaging the CI preview…",
        },
        CiPreviewState::Ready => CiPreviewStatusMessage {
            status_label: "ready",
            chat_message: "Starting the CI preview runtime…",
        },
        CiPreviewState::Booting => CiPreviewStatusMessage {
            status_label: "booting",
            chat_message: "Starting the CI preview runtime…",
        },
        CiPreviewState::Running => CiPreviewStatusMessage {
            status_label: "online",
            chat_message: "CI preview is ready. Transferring you now…",
        },
        CiPreviewState::Stopping => CiPreviewStatusMessage {
            status_label: "stopping",
            chat_message: "CI preview is restarting…",
        },
        CiPreviewState::Failed => CiPreviewStatusMessage {
            status_label: "failed",
            chat_message: "CI preview build failed.",
        },
        CiPreviewState::Expired => CiPreviewStatusMessage {
            status_label: "expired",
            chat_message: "CI preview expired.",
        },
    }
}

/// Add one bounded, safe diagnostic line to a lobby message.
///
/// Build output is produced by untrusted source builds and may contain
/// terminal control sequences, multiline noise, or credential-shaped values.
/// Only the final non-empty line is useful to a player, so the lobby receives
/// that line after control characters and obvious secret assignments are
/// removed.  Detailed logs remain available through the operator-facing CI
/// surfaces rather than being dumped into player chat.
pub fn ci_preview_chat_message(
    state: CiPreviewState,
    failure_message: Option<&str>,
    build_log: &str,
) -> String {
    let status = ci_preview_status_message(state);
    let mut message = status.chat_message.to_owned();
    let diagnostic = failure_message
        .and_then(sanitize_ci_diagnostic)
        .or_else(|| last_log_line(build_log).and_then(sanitize_ci_diagnostic));

    if let Some(diagnostic) = diagnostic {
        let prefix = if state == CiPreviewState::Failed {
            "Details"
        } else {
            "Latest build log"
        };
        message.push('\n');
        message.push_str(prefix);
        message.push_str(": ");
        message.push_str(&diagnostic);
    }
    message
}

/// Produce the server-list description for an active CI preview.
pub fn ci_preview_status_description(
    state: CiPreviewState,
    commit_sha: &str,
    failure_message: Option<&str>,
    build_log: &str,
) -> String {
    let mut description = format!("Pumpkin CI preview ({state}) — {commit_sha}");
    let diagnostic = failure_message
        .and_then(sanitize_ci_diagnostic)
        .or_else(|| last_log_line(build_log).and_then(sanitize_ci_diagnostic));
    if let Some(diagnostic) = diagnostic {
        description.push('\n');
        description.push_str(&diagnostic);
    }
    description
}

/// Keep diagnostics short enough for both Minecraft chat and status packets.
pub fn sanitize_ci_diagnostic(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let lower = value.to_ascii_lowercase();
    if contains_credential_assignment(&lower)
        || lower.contains("authorization: bearer ")
        || lower.contains("proxy-authorization: ")
    {
        return Some("diagnostic omitted because it contained credential data".to_owned());
    }

    let mut clean = String::with_capacity(value.len().min(256));
    let mut previous_was_space = false;
    for character in value.chars() {
        if character.is_control() {
            if !previous_was_space {
                clean.push(' ');
                previous_was_space = true;
            }
            continue;
        }
        if character.is_whitespace() {
            if !previous_was_space {
                clean.push(' ');
                previous_was_space = true;
            }
            continue;
        }
        clean.push(character);
        previous_was_space = false;
        if clean.len() >= 256 {
            break;
        }
    }

    let clean = clean.trim().to_owned();
    (!clean.is_empty()).then_some(clean)
}

fn last_log_line(value: &str) -> Option<&str> {
    value.lines().rev().find(|line| !line.trim().is_empty())
}

fn contains_credential_assignment(value: &str) -> bool {
    let words = value.split_whitespace().collect::<Vec<_>>();
    words.iter().enumerate().any(|(index, word)| {
        let key = word
            .split_once(['=', ':'])
            .map_or(*word, |(key, _)| key)
            .trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '_');
        let key = key.to_ascii_lowercase();
        let sensitive = key.contains("token")
            || key.contains("secret")
            || key.contains("password")
            || key.contains("passwd")
            || key.contains("api_key")
            || key.contains("apikey")
            || key.contains("access_key")
            || key.contains("private_key");
        sensitive
            && (word.contains('=')
                || word.contains(':')
                || words
                    .get(index + 1)
                    .is_some_and(|next| *next == "=" || next.starts_with('=')))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_covers_every_lobby_and_runtime_state() {
        let states = [
            CiPreviewState::Lobby,
            CiPreviewState::Queued,
            CiPreviewState::Fetching,
            CiPreviewState::Patching,
            CiPreviewState::Compiling,
            CiPreviewState::Packaging,
            CiPreviewState::Ready,
            CiPreviewState::Booting,
            CiPreviewState::Running,
            CiPreviewState::Stopping,
            CiPreviewState::Failed,
            CiPreviewState::Expired,
        ];
        for state in states {
            let status = ci_preview_status_message(state);
            assert!(!status.status_label.is_empty());
            assert!(!status.chat_message.is_empty());
        }
    }

    #[test]
    fn failed_chat_prefers_failure_message_and_bounds_it() {
        let message = ci_preview_chat_message(
            CiPreviewState::Failed,
            Some("compiler rejected feature xyz"),
            "a less useful log line",
        );
        assert!(message.contains("Details: compiler rejected feature xyz"));
        assert!(!message.contains("less useful"));

        let long = "x".repeat(512);
        let message = ci_preview_chat_message(CiPreviewState::Failed, Some(&long), "");
        assert!(message.len() <= 320);
    }

    #[test]
    fn diagnostics_are_single_line_and_credentials_are_not_echoed() {
        let message = ci_preview_chat_message(
            CiPreviewState::Compiling,
            None,
            "first line\nlatest\tline\nTOKEN=do-not-show",
        );
        assert!(message.contains("diagnostic omitted because it contained credential data"));
        assert!(!message.contains("do-not-show"));

        let description = ci_preview_status_description(
            CiPreviewState::Compiling,
            "0123456789abcdef0123456789abcdef01234567",
            None,
            "\u{1b}[31mcompile failed\u{1b}[0m",
        );
        assert!(!description.contains('\u{1b}'));
        assert_eq!(description.lines().count(), 2);
        assert!(!description.lines().nth(1).unwrap().contains('\n'));
    }

    #[test]
    fn credential_detection_handles_common_key_value_forms() {
        for line in [
            "authorization: Bearer abc",
            "GITHUB_TOKEN=abc",
            "password:abc",
            "private_key=abc",
        ] {
            let result = sanitize_ci_diagnostic(line).unwrap();
            assert_eq!(
                result,
                "diagnostic omitted because it contained credential data"
            );
        }
    }
}
