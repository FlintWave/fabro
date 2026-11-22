use fabro_test::test_context;
use serde_json::Value;

use super::support::run_state;

#[test]
fn help_exits_zero() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["eval", "--help"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
}

#[test]
fn eval_two_cases_pass() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let wf = "digraph W { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
    context.write_temp("w1/workflow.fabro", wf);
    context.write_temp(
        "w1/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp("w2/workflow.fabro", wf);
    context.write_temp(
        "w2/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp(
        "suite.toml",
        r#"_version = 1

[suite]
name = "smoke"
pass_threshold = 1.0

[[case]]
name = "c1"
workflow = "w1/workflow.toml"
environment = "local"

[[case]]
name = "c2"
workflow = "w2/workflow.toml"
environment = "local"
"#,
    );
    let suite = context.temp_dir.join("suite.toml");
    let output = context
        .command()
        .args(["eval", suite.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:{}\nstderr:{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("passed 2/2"), "{stdout}");
}

#[test]
#[expect(clippy::disallowed_methods, reason = "test sync io")]
fn eval_invalid_suite_exits_nonzero() {
    let context = test_context!();
    let path = context.temp_dir.join("bad.toml");
    std::fs::write(&path, "not = valid = toml\n").unwrap();
    let output = context
        .command()
        .args(["eval", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn eval_json_output() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let wf = "digraph W { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
    context.write_temp("a/workflow.fabro", wf);
    context.write_temp(
        "a/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp("b/workflow.fabro", wf);
    context.write_temp(
        "b/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp(
        "suite.toml",
        r#"_version = 1
[suite]
name = "smoke"
pass_threshold = 1.0
[[case]]
name = "c1"
workflow = "a/workflow.toml"
environment = "local"
[[case]]
name = "c2"
workflow = "b/workflow.toml"
environment = "local"
"#,
    );
    let suite = context.temp_dir.join("suite.toml");
    let output = context
        .command()
        .args(["eval", "--json", suite.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:{}\nstderr:{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    assert_eq!(v["cases"].as_array().unwrap().len(), 2);
}

#[test]
fn eval_failing_case_exits_one() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    context.write_temp(
        "w/workflow.fabro",
        "digraph W { start [shape=Mdiamond] exit [shape=Msquare] run [shape=parallelogram, script=\"false\", goal_gate=true, max_retries=0] start -> run -> exit }",
    );
    context.write_temp(
        "w/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp(
        "suite.toml",
        r#"_version = 1
[suite]
name = "smoke"
pass_threshold = 1.0
[[case]]
name = "c1"
workflow = "w/workflow.toml"
environment = "local"
"#,
    );
    let suite = context.temp_dir.join("suite.toml");
    let output = context
        .command()
        .args(["eval", suite.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "stdout:{}\nstderr:{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("passed 0/1"));
}

#[test]
fn eval_only_runs_named_case() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let wf = "digraph W { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
    context.write_temp("a/workflow.fabro", wf);
    context.write_temp(
        "a/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp("b/workflow.fabro", wf);
    context.write_temp(
        "b/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp(
        "suite.toml",
        r#"_version = 1
[suite]
name = "smoke"
pass_threshold = 1.0
[[case]]
name = "c1"
workflow = "a/workflow.toml"
environment = "local"
[[case]]
name = "c2"
workflow = "b/workflow.toml"
environment = "local"
"#,
    );
    let suite = context.temp_dir.join("suite.toml");
    let output = context
        .command()
        .args(["eval", "--only", "c1", suite.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:{}\nstderr:{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("c1"));
    assert!(!stdout.contains("c2"));
}

#[test]
fn eval_runs_carry_labels() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let wf = "digraph W { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
    context.write_temp("a/workflow.fabro", wf);
    context.write_temp(
        "a/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp(
        "suite.toml",
        r#"_version = 1
[suite]
name = "smoke"
pass_threshold = 1.0
[[case]]
name = "c1"
workflow = "a/workflow.toml"
environment = "local"
"#,
    );
    let suite = context.temp_dir.join("suite.toml");
    let output = context
        .command()
        .args(["eval", "--json", suite.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:{}\nstderr:{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let run_id = v["cases"][0]["run_id"].as_str().unwrap();
    let run_dir = context.find_run_dir(run_id);
    let state = run_state(&run_dir);
    assert_eq!(
        state.spec.labels.get("eval.suite").map(String::as_str),
        Some("smoke")
    );
    assert_eq!(
        state.spec.labels.get("eval.case").map(String::as_str),
        Some("c1")
    );
}

#[test]
fn eval_timeout_cancels_the_run_and_continues() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    context.write_temp(
        "slow/workflow.fabro",
        "digraph W { start [shape=Mdiamond] exit [shape=Msquare] run [shape=parallelogram, script=\"sleep 120\", max_retries=0] start -> run -> exit }",
    );
    context.write_temp(
        "slow/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    let wf = "digraph W { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
    context.write_temp("quick/workflow.fabro", wf);
    context.write_temp(
        "quick/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp(
        "suite.toml",
        r#"_version = 1
[suite]
name = "smoke"
pass_threshold = 0.5
[[case]]
name = "slow"
workflow = "slow/workflow.toml"
environment = "local"
timeout = "3s"
[[case]]
name = "quick"
workflow = "quick/workflow.toml"
environment = "local"
"#,
    );
    let suite = context.temp_dir.join("suite.toml");
    let started = std::time::Instant::now();
    let output = context
        .command()
        .args(["eval", "--json", suite.to_str().unwrap()])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout:{stdout}\nstderr:{stderr}");
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["cases"][0]["result"], "timeout");
    assert_eq!(v["cases"][1]["result"], "passed");
    assert!(v["cases"][0]["run_id"].is_string(), "{v}");
    assert!(v["cases"][0]["status"].is_string(), "{v}");
    assert!(
        v["cases"][0]["error"].is_null(),
        "error should be null/absent: {v}"
    );
    let run_id = v["cases"][0]["run_id"].as_str().unwrap();
    let run_dir = context.find_run_dir(run_id);
    let state = run_state(&run_dir);
    assert_eq!(state.status, fabro_types::RunStatus::Failed {
        reason: fabro_types::FailureReason::Cancelled,
    });
    assert!(elapsed.as_secs() < 60, "elapsed {}s", elapsed.as_secs());
}

#[test]
fn eval_timeout_fails_the_suite_at_full_threshold() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    context.write_temp(
        "slow/workflow.fabro",
        "digraph W { start [shape=Mdiamond] exit [shape=Msquare] run [shape=parallelogram, script=\"sleep 120\", max_retries=0] start -> run -> exit }",
    );
    context.write_temp(
        "slow/workflow.toml",
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    );
    context.write_temp(
        "suite.toml",
        r#"_version = 1
[suite]
name = "smoke"
pass_threshold = 1.0
[[case]]
name = "slow"
workflow = "slow/workflow.toml"
environment = "local"
timeout = "3s"
"#,
    );
    let suite = context.temp_dir.join("suite.toml");
    let output = context
        .command()
        .args(["eval", suite.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("passed 0/1"), "{stdout}");
}
