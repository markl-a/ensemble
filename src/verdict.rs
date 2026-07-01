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
    // approve/lgtm vs changes; keep the LAST such line as authoritative. A reply with no verdict
    // line is conservatively changes-requested (an unparseable review must never land).
    let mut result: Option<Verdict> = None;
    for line in text.lines() {
        let low = line.to_ascii_lowercase();
        let Some(verdict_idx) = low.find("verdict") else {
            continue;
        };
        // Classify by whichever token appears FIRST after "verdict" — not by an unordered
        // `contains` check — so a changes-request whose message happens to mention
        // "approve"/"lgtm" in prose (e.g. "CHANGES: approve once you fix X") is never
        // misread as an approval.
        let after_start = verdict_idx + "verdict".len();
        let after = &low[after_start..];
        let changes_pos = after.find("changes");
        let approve_pos = [after.find("lgtm"), after.find("approve")]
            .into_iter()
            .flatten()
            .min();

        result = Some(match (changes_pos, approve_pos) {
            (Some(c), Some(a)) if a < c => Verdict::Approve,
            (Some(c), _) => {
                // byte indices match since lowercasing ASCII preserves positions.
                let idx = after_start + c;
                let msg = line[idx + "changes".len()..]
                    .trim_start_matches(|ch: char| ch == ':' || ch.is_whitespace());
                Verdict::Changes(msg.to_string())
            }
            (None, Some(_)) => Verdict::Approve,
            (None, None) => Verdict::Changes(format!(
                "unrecognized verdict line: {}",
                line.trim()
            )),
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
    }
}
