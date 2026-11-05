//! Every recorded step remembers which spec step wrote it (`spec_step`),
//! numbered the way a person reads the file: an `in:` block's steps count one
//! by one, a `when:` or `repeat:` block counts once.

use flowproof_agent::{record, FlowSpec};
use flowproof_driver::mock::MockAppDriver;
use flowproof_replay::load_trace;

const CALC_ELEMENTS: [&str; 5] = [
    "num5Button",
    "num3Button",
    "plusButton",
    "equalButton",
    "CalculatorResults",
];

fn spec_steps(trace: &std::path::Path) -> Vec<Option<u32>> {
    let (_, steps) = load_trace(trace).expect("trace loads");
    steps.iter().map(|s| s.spec_step).collect()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn plain_steps_are_numbered_in_file_order() {
    let dir = temp_dir("flowproof-spec-step-plain");
    let spec = FlowSpec::parse(
        "name: Add\napp: calc\nsteps:\n  - Type 5\n  - Press plus\n  - Type 3\n  - Press equals\n  - assert: display shows 8\n",
    )
    .expect("spec parses");
    let mut driver =
        MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 8");
    let trace = dir.join("plain.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("records");

    assert_eq!(
        spec_steps(&trace),
        [Some(1), Some(2), Some(3), Some(4), Some(5)]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_when_block_counts_once_and_its_steps_share_its_number() {
    let dir = temp_dir("flowproof-spec-step-when");
    let spec = FlowSpec::parse(
        "name: Recover\napp: calc\nsteps:\n  - Type 5\n  - when: page shows Error\n    steps:\n      - Press equals\n      - Press plus\n  - assert: display shows 8\n",
    )
    .expect("spec parses");

    // Taken at record time: both block steps carry the block's number.
    let mut driver = MockAppDriver::new(&CALC_ELEMENTS)
        .with_text("CalculatorResults", "Display is 8")
        .with_surface_text("Error");
    let trace = dir.join("taken.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("records");
    assert_eq!(spec_steps(&trace), [Some(1), Some(2), Some(2), Some(3)]);

    // Not taken: the block writes nothing, and the next step keeps number 3.
    let mut driver = MockAppDriver::new(&CALC_ELEMENTS)
        .with_text("CalculatorResults", "Display is 8")
        .with_surface_text("all is well");
    let trace = dir.join("skipped.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("records");
    assert_eq!(spec_steps(&trace), [Some(1), Some(3)]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_pass_of_a_repeat_carries_the_repeat_number() {
    let dir = temp_dir("flowproof-spec-step-repeat");
    let spec = FlowSpec::parse(
        "name: Press until done\napp: calc\nsteps:\n  - repeat:\n      until: page shows Done\n      max: 5\n      steps:\n        - Press plus\n  - assert: display shows 8\n",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&CALC_ELEMENTS)
        .with_text("CalculatorResults", "Display is 8")
        .with_surface_text("Done");
    driver.text_sequence.insert(
        MockAppDriver::SURFACE.to_string(),
        ["counting", "counting"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );
    let trace = dir.join("repeat.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("records");

    assert_eq!(spec_steps(&trace), [Some(1), Some(1), Some(2)]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn steps_inside_in_blocks_count_one_by_one() {
    use flowproof_driver::surface::{SurfaceFactory, SurfaceRegistry};

    let dir = temp_dir("flowproof-spec-step-in");
    let spec = FlowSpec::parse(
        "name: Order across surfaces\napps:\n  gui: {app: sap}\n  portal: {app: web, url: \"https://portal.test/orders\"}\nsteps:\n  - in: gui\n    steps:\n      - Remember the \"OrderNo\" as order\n  - in: portal\n    steps:\n      - Type ${captured.order} into the \"Search\" field\n      - assert: the \"Search\" shows ${captured.order}\n",
    )
    .expect("spec parses");
    let factory: SurfaceFactory = Box::new(|name| {
        Ok(Box::new(match name {
            "gui" => MockAppDriver::new(&["OrderNo"]).with_text("OrderNo", "4711"),
            _ => MockAppDriver::new(&["Search"]),
        }))
    });
    let targets = flowproof_agent::surface_targets(&spec).expect("targets");
    let mut registry = SurfaceRegistry::new(targets, factory, std::time::Duration::from_millis(50));
    let trace = dir.join("multi.trace.jsonl");
    record(&spec, &mut registry, &trace).expect("records");

    assert_eq!(spec_steps(&trace), [Some(1), Some(2), Some(3)]);
    std::fs::remove_dir_all(&dir).ok();
}
