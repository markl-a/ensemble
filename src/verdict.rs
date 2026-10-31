/// A reviewer's decision on a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    /// Changes requested; the String is the message routed back to the implementer.
    Changes(String),
}

/// Parse an agent's reply into a verdict. Convention: a line `VERDICT: LGTM|APPROVE` approves;
/// `VERDICT: CHANGES: <msg>` requests changes. Anything without an explicit approving VERDICT
/// line is treated as changes-requested — an unparseable or ambiguous review must NEVER land.
pub fn parse_verdict(text: &str) -> Verdict {
    // Scan every line that MENTIONS "verdict" (case-insensitive) — tolerating markdown prefixes a
    // real reviewer adds, e.g. "## Review verdict: ✅ Approve" or "**VERDICT: LGTM**". Classify by
    // the WORD right after the marker; keep the LAST such line as authoritative. A reply with no
    // verdict line is conservatively changes-requested (an unparseable review must never land).
    //
    // Only that first word counts — not a substring scan of the rest of the line — so a negative
    // word (`DISAPPROVE`, `NOT APPROVED`, `REJECT`) never lands through a bare "approve" match,
    // and a changes-request whose message mentions "approve" in prose stays Changes.
    let mut result: Option<Verdict> = None;
    for line in text.lines() {
        let low = line.to_ascii_lowercase();
        let Some(verdict_idx) = low.find("verdict") else {
            continue;
        };
        // Skip the separators/markdown between the marker and the word (`:`, spaces, `✅`, `*`).
        // Lowercasing ASCII preserves byte offsets, so `low` indices are valid in `line` too.
        let rest = low[verdict_idx + "verdict".len()..]
            .trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric());
        let word_len = rest
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .unwrap_or(rest.len());

        result = Some(match &rest[..word_len] {
            "approve" | "approved" | "lgtm" => Verdict::Approve,
            word if word.starts_with("changes") => {
                let idx = line.len() - rest.len() + "changes".len();
                let msg =
                    line[idx..].trim_start_matches(|ch: char| ch == ':' || ch.is_whitespace());
                Verdict::Changes(msg.to_string())
            }
            // Unclear or negative (disapprove, not approved, reject, blank) ⇒ changes-requested.
            _ => Verdict::Changes(format!("unrecognized verdict line: {}", line.trim())),
        });
    }
    result.unwrap_or_else(|| {
        Verdict::Changes("no explicit VERDICT line; treating as changes-requested".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_approve_and_changes_conservatively() {
        assert_eq!(parse_verdict("looks good\nVERDICT: LGTM"), Verdict::Approve);
        assert_eq!(parse_verdict("VERDICT: APPROVE"), Verdict::Approve);
        assert_eq!(
            parse_verdict("issues...\nVERDICT: CHANGES: fix the off-by-one"),
            Verdict::Changes("fix the off-by-one".to_string())
        );
        // No marker at all ⇒ conservative: NOT an approval (an unparseable review can't land).
        assert_eq!(
            parse_verdict("I think it is fine"),
            Verdict::Changes("no explicit VERDICT line; treating as changes-requested".to_string())
        );
    }

    #[test]
    fn changes_line_mentioning_approve_is_not_misclassified() {
        // "changes" appears before "approve" after the VERDICT marker ⇒ still Changes,
        // even though the message text itself contains the word "approve".
        assert_eq!(
            parse_verdict("VERDICT: CHANGES: approve once you fix X"),
            Verdict::Changes("approve once you fix X".to_string())
        );
        assert_eq!(
            parse_verdict("VERDICT: CHANGES: do not approve until tests pass"),
            Verdict::Changes("do not approve until tests pass".to_string())
        );
    }

    #[test]
    fn negative_or_unclear_words_are_changes() {
        // "disapprove" / "not approved" contain "approve" but are NOT approvals; anything that is
        // not an explicit approve word right after VERDICT is changes-requested.
        for text in [
            "VERDICT: DISAPPROVE",
            "VERDICT: NOT APPROVED",
            "VERDICT: REJECT",
            "VERDICT: looks fine, LGTM",
            "VERDICT:",
        ] {
            assert!(
                matches!(parse_verdict(text), Verdict::Changes(_)),
                "{text:?} must not approve"
            );
        }
    }

    #[test]
    fn approve_words_with_markdown_still_land() {
        assert_eq!(parse_verdict("VERDICT: APPROVED"), Verdict::Approve);
        assert_eq!(
            parse_verdict("## Review verdict: ✅ Approve"),
            Verdict::Approve
        );
        assert_eq!(parse_verdict("**VERDICT: LGTM**"), Verdict::Approve);
        // A "changes" word BEFORE the marker is ignored — the word after the marker wins.
        assert_eq!(
            parse_verdict("no changes needed. VERDICT: LGTM"),
            Verdict::Approve
        );
    }
}
