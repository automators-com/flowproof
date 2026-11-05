//! `run --from/--until`: which recorded steps a partial run replays.
//!
//! Steps are numbered the way a person reads the flow file (see
//! [`crate::recorder::spec_step_numbers`]): an `in:` block's steps count one
//! by one, a `when:` or `repeat:` block counts once. A trace recorded with
//! `spec_step` says which number wrote each step; an older trace is matched
//! by intent, and refused when that match is not certain.

use crate::spec::{FlowSpec, SpecStep};
use flowproof_trace::captures::{produced_by, used_by};
use flowproof_trace::Step;

/// The spec steps a partial run covers, both ends included, counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepRange {
    pub from: u32,
    pub until: u32,
}

/// How many numbered steps `spec` has.
pub fn step_count(spec: &FlowSpec) -> u32 {
    spec.steps
        .iter()
        .map(|step| match step {
            SpecStep::InSurface { block } => block.steps.len() as u32,
            _ => 1,
        })
        .sum()
}

/// Resolve `--from`/`--until` against `spec` and its recorded `steps`: which
/// trace steps to replay (one flag per trace step), or why this flow cannot
/// be run in part. Refused before anything launches: a range outside the
/// flow, a range that recorded nothing, and a selected step that reads a
/// capture an unselected step makes.
pub fn select(
    spec: &FlowSpec,
    steps: &[Step],
    from: Option<u32>,
    until: Option<u32>,
) -> Result<(StepRange, Vec<bool>), String> {
    if spec.apps.values().any(|s| s.app.id() == "agent") {
        return Err("a flow with an agent block cannot be run in part; run it whole".into());
    }
    let count = step_count(spec);
    let range = StepRange {
        from: from.unwrap_or(1),
        until: until.unwrap_or(count),
    };
    if range.from == 0 || range.until == 0 {
        return Err("steps are counted from 1".into());
    }
    if range.until > count {
        return Err(format!(
            "this flow has {count} step{}; there is no step {}",
            if count == 1 { "" } else { "s" },
            range.until
        ));
    }
    if range.from > range.until {
        return Err(format!(
            "--from {} is after --until {}",
            range.from, range.until
        ));
    }
    let numbers = trace_numbers(spec, steps)?;
    let selected: Vec<bool> = numbers
        .iter()
        .map(|n| (range.from..=range.until).contains(n))
        .collect();
    if !selected.contains(&true) {
        return Err(format!(
            "{} wrote nothing when this flow was recorded (a `when:` that did not hold)",
            describe(range)
        ));
    }
    check_captures(steps, &numbers, &selected)?;
    Ok((range, selected))
}

/// `step 4` or `steps 3–5`, for messages and reports.
pub fn describe(range: StepRange) -> String {
    if range.from == range.until {
        format!("step {}", range.from)
    } else {
        format!("steps {}–{}", range.from, range.until)
    }
}

/// The spec step number of every trace step.
fn trace_numbers(spec: &FlowSpec, steps: &[Step]) -> Result<Vec<u32>, String> {
    const RECORD_AGAIN: &str = "record this flow again to run part of it";
    let count = step_count(spec);
    let stamped: Vec<u32> = steps.iter().filter_map(|s| s.spec_step).collect();
    if stamped.len() == steps.len() {
        if stamped.iter().any(|&n| n == 0 || n > count) {
            return Err(format!(
                "the trace does not match this flow's steps; {RECORD_AGAIN}"
            ));
        }
        return Ok(stamped);
    }
    if !stamped.is_empty() {
        return Err(format!(
            "the trace numbers only some of its steps; {RECORD_AGAIN}"
        ));
    }

    // Recorded before `spec_step`: match each trace step to the one spec
    // step that could have written it, by intent. Certain only when no
    // intent belongs to two spec steps.
    let mut owners: std::collections::HashMap<String, u32> = Default::default();
    for (number, intents) in numbered_intents(spec) {
        for intent in intents {
            if let Some(&other) = owners.get(&intent) {
                if other != number {
                    return Err(format!(
                        "steps {other} and {number} both read \"{intent}\", so this older \
                         trace cannot tell them apart; {RECORD_AGAIN}"
                    ));
                }
            }
            owners.insert(intent, number);
        }
    }
    let mut numbers = Vec::with_capacity(steps.len());
    for step in steps {
        let Some(&number) = owners.get(&step.intent) else {
            return Err(format!(
                "the recorded step \"{}\" is not in this flow any more; {RECORD_AGAIN}",
                step.intent
            ));
        };
        if numbers.last().is_some_and(|&last| number < last) {
            return Err(format!(
                "the trace's steps are not in this flow's order; {RECORD_AGAIN}"
            ));
        }
        numbers.push(number);
    }
    Ok(numbers)
}

/// Each numbered spec step with the intents of the trace steps it can write.
fn numbered_intents(spec: &FlowSpec) -> Vec<(u32, Vec<String>)> {
    fn leaves(step: &SpecStep, out: &mut Vec<String>) {
        match step {
            SpecStep::When { when } => when.steps.iter().for_each(|s| leaves(s, out)),
            SpecStep::Repeat { repeat } => repeat.steps.iter().for_each(|s| leaves(s, out)),
            SpecStep::InSurface { block } => block.steps.iter().for_each(|s| leaves(s, out)),
            other => out.push(other.intent()),
        }
    }
    let numbers = crate::recorder::spec_step_numbers(&spec.steps);
    let mut out = Vec::new();
    for (step, number) in spec.steps.iter().zip(numbers) {
        match step {
            SpecStep::InSurface { block } => {
                for (i, inner) in block.steps.iter().enumerate() {
                    let mut intents = Vec::new();
                    leaves(inner, &mut intents);
                    out.push((number + i as u32, intents));
                }
            }
            other => {
                let mut intents = Vec::new();
                leaves(other, &mut intents);
                out.push((number, intents));
            }
        }
    }
    out
}

/// Captured values live only in one run's memory, so a selected step that
/// reads a capture must also run the step that makes it.
fn check_captures(steps: &[Step], numbers: &[u32], selected: &[bool]) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        if !selected[i] {
            continue;
        }
        for name in used_by(step) {
            let maker = (0..i)
                .rev()
                .find(|&j| produced_by(&steps[j]).contains(&name));
            if let Some(j) = maker.filter(|&j| !selected[j]) {
                return Err(format!(
                    "step {} uses `captured.{name}`, made by step {}: start from step {} or \
                     earlier",
                    numbers[i], numbers[j], numbers[j]
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(intent: &str, spec_step: Option<u32>, action: serde_json::Value) -> Step {
        let mut step: Step = serde_json::from_value(serde_json::json!({
            "id": "s0001",
            "intent": intent,
            "action": action,
            "selectors": [],
            "sync": {"pre": [], "post": []},
            "artifacts": {},
        }))
        .expect("step");
        step.spec_step = spec_step;
        step
    }

    fn click(intent: &str, n: Option<u32>) -> Step {
        step(
            intent,
            n,
            serde_json::json!({"type": "click", "params": {}}),
        )
    }

    const SPEC: &str = "name: Order\napp: calc\nsteps:\n  - Press one\n  - when: page shows Error\n    steps:\n      - Press two\n      - Press three\n  - Press four\n";

    #[test]
    fn a_range_selects_the_trace_steps_its_spec_steps_wrote() {
        let spec = FlowSpec::parse(SPEC).expect("spec");
        assert_eq!(step_count(&spec), 3);
        let steps = [
            click("Press one", Some(1)),
            click("Press two", Some(2)),
            click("Press three", Some(2)),
            click("Press four", Some(3)),
        ];
        let (range, selected) = select(&spec, &steps, Some(2), None).expect("selects");
        assert_eq!(range, StepRange { from: 2, until: 3 });
        assert_eq!(selected, [false, true, true, true]);
        let (_, selected) = select(&spec, &steps, None, Some(1)).expect("selects");
        assert_eq!(selected, [true, false, false, false]);
    }

    #[test]
    fn an_older_trace_is_matched_by_intent() {
        let spec = FlowSpec::parse(SPEC).expect("spec");
        let steps = [
            click("Press one", None),
            click("Press two", None),
            click("Press three", None),
            click("Press four", None),
        ];
        let (_, selected) = select(&spec, &steps, Some(2), Some(2)).expect("selects");
        assert_eq!(selected, [false, true, true, false]);
    }

    #[test]
    fn an_older_trace_with_a_repeated_intent_is_refused() {
        let spec = FlowSpec::parse(
            "name: Twice\napp: calc\nsteps:\n  - Press plus\n  - Press one\n  - Press plus\n",
        )
        .expect("spec");
        let steps = [
            click("Press plus", None),
            click("Press one", None),
            click("Press plus", None),
        ];
        let err = select(&spec, &steps, Some(2), None).expect_err("ambiguous");
        assert!(err.contains("record this flow again"), "{err}");
    }

    #[test]
    fn a_range_outside_the_flow_is_refused() {
        let spec = FlowSpec::parse(SPEC).expect("spec");
        let steps = [click("Press one", Some(1)), click("Press four", Some(3))];
        assert!(select(&spec, &steps, Some(0), None).is_err());
        assert!(select(&spec, &steps, None, Some(4))
            .expect_err("too far")
            .contains("has 3 steps"));
        assert!(select(&spec, &steps, Some(3), Some(2)).is_err());
        // Step 2 is a `when:` that did not hold at record time.
        assert!(select(&spec, &steps, Some(2), Some(2))
            .expect_err("nothing recorded")
            .contains("wrote nothing"));
    }

    #[test]
    fn a_capture_made_outside_the_range_is_refused() {
        let spec = FlowSpec::parse(
            "name: Order\napp: calc\nsteps:\n  - Remember the order\n  - Press next\n  - Type the order\n",
        )
        .expect("spec");
        let steps = [
            step(
                "Remember the order",
                Some(1),
                serde_json::json!({"type": "capture", "params": {"name": "order_no"}}),
            ),
            click("Press next", Some(2)),
            step(
                "Type the order",
                Some(3),
                serde_json::json!({"type": "type_text", "params": {"text": "${captured.order_no}"}}),
            ),
        ];
        let err = select(&spec, &steps, Some(2), None).expect_err("needs step 1");
        assert_eq!(
            err,
            "step 3 uses `captured.order_no`, made by step 1: start from step 1 or earlier"
        );
        assert!(select(&spec, &steps, Some(1), None).is_ok());
    }
}
