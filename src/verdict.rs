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
    for line in text.lines() {
        let l = line.trim();
        let upper = l.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("VERDICT:") {
            let rest = rest.trim();
            if rest.starts_with("LGTM") || rest.starts_with("APPROVE") {
                return Verdict::Approve;
            }
            if let Some(idx) = rest.find("CHANGES") {
                // take the message after "CHANGES:" from the ORIGINAL-case line
                let after = l[l.to_ascii_uppercase().find("CHANGES").unwrap() + "CHANGES".len()..]
                    .trim_start_matches(|c: char| c == ':' || c.is_whitespace());
                let _ = idx;
                return Verdict::Changes(after.to_string());
            }
            return Verdict::Changes(format!("unrecognized VERDICT line: {l}"));
        }
    }
    Verdict::Changes("no explicit VERDICT line; treating as changes-requested".to_string())
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
            Verdict::Changes(
                "no explicit VERDICT line; treating as changes-requested".to_string()
            )
        );
    }
}
