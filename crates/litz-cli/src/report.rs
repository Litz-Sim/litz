//! Machine-readable results.
//!
//! A CI job should not have to parse a terminal. Two shapes cover nearly
//! everything: a single JSON object for a script that wants one answer, and
//! JUnit XML for the dashboards that already read test reports. Both are
//! derived from the same [`Outcome`], so they cannot disagree with the exit
//! code the process returns.

use crate::runner::Outcome;
use serde_json::json;

/// Why the run ended, in the vocabulary a CI consumer recognises.
fn reason(outcome: &Outcome) -> &'static str {
    if outcome.tests.is_empty() {
        return if outcome.exit_code == 0 {
            "ok"
        } else {
            "failed"
        };
    }
    if outcome.exit_code == 0 {
        "all tests passed"
    } else if outcome
        .tests
        .iter()
        .any(|t| t.output.contains("electrical fault"))
    {
        "electrical fault"
    } else if outcome.tests.iter().any(|t| t.output.contains("never saw")) {
        "assertion failed"
    } else if outcome
        .tests
        .iter()
        .any(|t| t.output.contains("found failure text"))
    {
        "failure text matched"
    } else {
        "timed out"
    }
}

pub fn json(outcome: &Outcome) -> String {
    let tests: Vec<_> = outcome
        .tests
        .iter()
        .map(|test| {
            json!({
                "name": test.name,
                "status": if test.passed { "passed" } else { "failed" },
                "exit_code": test.exit_code,
                "sim_time_ms": test.sim_ms,
                "output": test.output,
            })
        })
        .collect();
    let document = json!({
        "status": if outcome.exit_code == 0 { "passed" } else { "failed" },
        "reason": reason(outcome),
        "exit_code": outcome.exit_code,
        "sim_time_ms": outcome.sim_ms,
        "tests": tests,
    });
    serde_json::to_string_pretty(&document).expect("a JSON value always serialises")
}

/// JUnit XML, one `testcase` per `[[test]]`, with the transcript in
/// `system-out` — the same place a failing CI user looks first.
pub fn junit(outcome: &Outcome, suite: &str) -> String {
    let failures = outcome.tests.iter().filter(|t| !t.passed).count();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuites tests=\"{}\" failures=\"{failures}\">\n",
        outcome.tests.len()
    ));
    xml.push_str(&format!(
        "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{failures}\">\n",
        escape(suite),
        outcome.tests.len()
    ));
    for test in &outcome.tests {
        xml.push_str(&format!(
            "    <testcase name=\"{}\" classname=\"{}\"",
            escape(&test.name),
            escape(suite)
        ));
        if test.passed {
            xml.push_str("/>\n");
            continue;
        }
        xml.push_str(">\n");
        xml.push_str(&format!(
            "      <failure message=\"exit {}\">{}</failure>\n",
            test.exit_code,
            escape(first_note(&test.output))
        ));
        xml.push_str(&format!(
            "      <system-out>{}</system-out>\n",
            escape(&test.output)
        ));
        xml.push_str("    </testcase>\n");
    }
    xml.push_str("  </testsuite>\n</testsuites>\n");
    xml
}

/// The one line a dashboard shows: the `litz:` note if there is one, else the
/// last non-empty line of the transcript.
fn first_note(output: &str) -> &str {
    output
        .lines()
        .find(|line| line.starts_with("litz: "))
        .or_else(|| output.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("failed")
}

/// XML text escaping. Control characters are dropped rather than encoded:
/// they are illegal in XML 1.0 even as references, and a serial log can
/// contain anything the firmware printed.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' | '\r' | '\t' => out.push(ch),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::TestOutcome;

    fn outcome(tests: Vec<TestOutcome>, exit_code: i32) -> Outcome {
        Outcome {
            exit_code,
            output: String::new(),
            sim_ms: tests.iter().map(|t| t.sim_ms).max().unwrap_or(0),
            tests,
        }
    }

    fn test(name: &str, passed: bool, output: &str) -> TestOutcome {
        TestOutcome {
            name: name.into(),
            passed,
            exit_code: if passed { 0 } else { 1 },
            output: output.into(),
            sim_ms: 250,
        }
    }

    #[test]
    fn the_json_record_says_what_happened_and_why() {
        let out = outcome(
            vec![
                test("blinks", true, "litz: blinky\n"),
                test("stays dark", false, "litz: never saw \"x\"\n"),
            ],
            1,
        );
        let value: serde_json::Value = serde_json::from_str(&json(&out)).unwrap();
        assert_eq!(value["status"], "failed");
        assert_eq!(value["reason"], "assertion failed");
        assert_eq!(value["exit_code"], 1);
        assert_eq!(value["sim_time_ms"], 250);
        assert_eq!(value["tests"][0]["status"], "passed");
        assert_eq!(value["tests"][1]["name"], "stays dark");
    }

    #[test]
    fn a_fault_is_reported_as_a_fault() {
        let out = outcome(
            vec![test("x", false, "litz: electrical fault on net 0: boom\n")],
            1,
        );
        let value: serde_json::Value = serde_json::from_str(&json(&out)).unwrap();
        assert_eq!(value["reason"], "electrical fault");
    }

    #[test]
    fn junit_counts_failures_and_keeps_the_transcript() {
        let out = outcome(
            vec![
                test("ok", true, "fine\n"),
                test("bad", false, "litz: never saw \"x\"\n"),
            ],
            1,
        );
        let xml = junit(&out, "litz");
        assert!(xml.contains("tests=\"2\" failures=\"1\""), "{xml}");
        assert!(
            xml.contains("<failure message=\"exit 1\">litz: never saw &quot;x&quot;</failure>"),
            "{xml}"
        );
        assert!(
            xml.contains("<system-out>litz: never saw &quot;x&quot;"),
            "{xml}"
        );
        assert!(
            xml.contains("<testcase name=\"ok\" classname=\"litz\"/>"),
            "{xml}"
        );
    }

    #[test]
    fn xml_special_characters_are_escaped_and_controls_are_dropped() {
        assert_eq!(escape("a<b&c>\"d\"\u{1}"), "a&lt;b&amp;c&gt;&quot;d&quot; ");
    }
}
