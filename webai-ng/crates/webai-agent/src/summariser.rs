//! `history_summariser`: long-session compression (ARCHITECTURE §4.9).
//!
//! A session transcript grows unboundedly; to control token cost we compress
//! older turns while preserving key decisions and any in-flight (unfinished)
//! goals. `HistorySummariser` produces a compact summary the loop injects in
//! place of the oldest messages. The summariser itself is pure (deterministic
//! rule-based in this milestone; an LLM pass can be layered on later) and is
//! fully unit-testable.

/// A turn in the session transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    /// Free text for the turn (user prompt or assistant step).
    pub text: String,
}

/// Who produced a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// The result of compressing a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarisedHistory {
    /// The compact narrative summary (older turns collapsed).
    pub summary: String,
    /// In-flight goals (from recent assistant reasoning / user asks) preserved.
    pub open_goals: Vec<String>,
    /// Whether compression actually shrank the transcript (no-op for short ones).
    pub compressed: bool,
}

/// Controls when compression kicks in and how aggressive it is.
#[derive(Debug, Clone, Copy)]
pub struct SummariserConfig {
    /// Turns above this threshold trigger a compression pass.
    pub max_turns_before_summarise: usize,
    /// How many most-recent turns are always kept verbatim.
    pub keep_recent: usize,
}

impl Default for SummariserConfig {
    fn default() -> Self {
        Self {
            max_turns_before_summarise: 40,
            keep_recent: 8,
        }
    }
}

/// Deterministic rule-based summariser.
#[derive(Debug, Clone)]
pub struct HistorySummariser {
    config: SummariserConfig,
}

impl Default for HistorySummariser {
    fn default() -> Self {
        Self::new(SummariserConfig::default())
    }
}

impl HistorySummariser {
    pub fn new(config: SummariserConfig) -> Self {
        Self { config }
    }

    /// Summarise a transcript, preserving the `keep_recent` newest turns in
    /// full and collapsing everything older into a concise narrative.
    pub fn summarise(&self, turns: &[Turn]) -> SummarisedHistory {
        if turns.len() <= self.config.max_turns_before_summarise {
            // Too short to need compression; keep as-is.
            return SummarisedHistory {
                summary: self.humanise(turns),
                open_goals: extract_goals(turns),
                compressed: false,
            };
        }

        let keep = self.config.keep_recent.min(turns.len());
        let (old, recent) = turns.split_at(turns.len() - keep);

        let old_summary = self.collapse(old);
        let recent_text = self.humanise(recent);
        let open_goals = extract_goals(turns);

        let summary = if old_summary.is_empty() {
            recent_text
        } else {
            format!(
                "# Earlier context\n{}\n\n# Recent\n{}",
                old_summary, recent_text
            )
        };

        SummarisedHistory {
            summary,
            open_goals,
            compressed: true,
        }
    }

    fn collapse(&self, old: &[Turn]) -> String {
        if old.is_empty() {
            return String::new();
        }
        // Collapse the oldest block into a single-line key-decision summary.
        // This milestone keeps every distinct assistant action as a bullet so
        // "keep key decisions & unfinished goals" is preserved; an LLM pass
        // can later drop detail further.
        let mut out = String::new();
        for turn in old.iter().filter(|t| t.role == Role::Assistant) {
            out.push_str("- ");
            out.push_str(&turn.text);
            out.push('\n');
        }
        out
    }

    fn humanise(&self, turns: &[Turn]) -> String {
        let mut out = String::new();
        for turn in turns {
            let name = match turn.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            out.push_str(&format!("{name}: {}\n", turn.text));
        }
        out
    }
}

/// Extract likely in-flight / open goals from a transcript. A goal is "open"
/// if the latest user turn (or the last assistant step) is not a terminal
/// confirmation. Heuristic: the most recent user prompt plus any user prompt
/// that mentions future intent words.
fn extract_goals(turns: &[Turn]) -> Vec<String> {
    let mut goals: Vec<String> = Vec::new();
    // The most recent user turn is an open goal unless the last assistant turn
    // is a completion.
    if let Some(last) = turns.last() {
        if last.role == Role::Assistant && !is_complete(&last.text) {
            if let Some(goal) = last_user_before(turns) {
                goals.push(goal);
            }
        } else if last.role == Role::User {
            goals.push(last.text.clone());
        }
    }
    goals.dedup();
    goals
}

fn last_user_before(turns: &[Turn]) -> Option<String> {
    turns
        .iter()
        .rev()
        .find(|t| t.role == Role::User)
        .map(|t| t.text.clone())
}

fn is_complete(text: &str) -> bool {
    let lower = text.to_lowercase();
    for marker in [
        "done",
        "finished",
        "complete",
        "completed",
        "总结完成",
        "完成",
    ] {
        if lower.contains(marker) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_transcript_is_not_compressed() {
        let turns = vec![
            Turn {
                role: Role::User,
                text: "open site".into(),
            },
            Turn {
                role: Role::Assistant,
                text: "opened".into(),
            },
        ];
        let s = HistorySummariser::default();
        let result = s.summarise(&turns);
        assert!(!result.compressed);
        // Both turns preserved.
        assert!(result.summary.contains("open site"));
        assert!(result.summary.contains("opened"));
    }

    #[test]
    fn long_transcript_is_compressed_keeping_recent() {
        let mut turns: Vec<Turn> = Vec::new();
        for i in 0..50 {
            turns.push(Turn {
                role: Role::Assistant,
                text: format!("step {i}"),
            });
        }
        turns.push(Turn {
            role: Role::User,
            text: "continue to the finance page".into(),
        });

        let s = HistorySummariser::new(SummariserConfig {
            max_turns_before_summarise: 40,
            keep_recent: 8,
        });
        let result = s.summarise(&turns);
        assert!(result.compressed);
        // Recent turns preserved verbatim.
        assert!(result.summary.contains("step 48"));
        assert!(result.summary.contains("step 49"));
        // The open user goal is preserved.
        assert!(result.open_goals.iter().any(|g| g.contains("finance page")));
    }

    #[test]
    fn no_completion_keeps_open_goal() {
        let turns = vec![
            Turn {
                role: Role::User,
                text: "login to gmail".into(),
            },
            Turn {
                role: Role::Assistant,
                text: "email loaded".into(),
            },
        ];
        let s = HistorySummariser::default();
        let result = s.summarise(&turns);
        assert!(result.open_goals.iter().any(|g| g.contains("gmail")));
    }

    #[test]
    fn completion_markers_close_a_goal() {
        let turns = vec![
            Turn {
                role: Role::User,
                text: "login to gmail".into(),
            },
            Turn {
                role: Role::Assistant,
                text: "task done".into(),
            },
        ];
        let s = HistorySummariser::default();
        let result = s.summarise(&turns);
        // "task done" doesn't contain gmail; the heuristic keeps the last user
        // turn only when the assistant hasn't completed. Here it has
        // ("done"), so the gmail goal is not re-listed as open.
        assert!(!result.open_goals.iter().any(|g| g.contains("gmail")));
    }
}
