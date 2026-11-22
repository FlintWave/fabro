use fabro_test::{fabro_json_snapshot, fabro_snapshot, test_context};
use serde_json::Value;

use super::support::run_state;

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["eval", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Run an eval suite

    Usage: fabro eval [OPTIONS] <SUITE>

    Arguments:
      <SUITE>  Path to the eval suite TOML file

    Options:
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --only <CASE>                Restrict the run to the named case(s)
          --environment <ENVIRONMENT>  Default named environment for agent tools (a case's own environment overrides this)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --output <FILE>              Write the JSON report to FILE
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --json                       Emit JSON output
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn eval_two_passing_cases_print_summary() {
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
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["eval", suite.to_str().unwrap()]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    c1 passed [DURATION]  [ULID]
    c2 passed [DURATION]  [ULID]
    passed 2/2 (100%), threshold 100%: PASS
    ----- stderr -----
    ");
}

#[test]
#[expect(clippy::disallowed_methods, reason = "test sync io")]
fn eval_invalid_suite_exits_nonzero() {
    let context = test_context!();
    let path = context.temp_dir.join("bad.toml");
    std::fs::write(&path, "not = valid = toml\n").unwrap();
    let mut cmd = context.command();
    cmd.args(["eval", path.to_str().unwrap()]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × could not parse suite file [TEMP_DIR]/bad.toml
      ╰─▶ TOML parse error at line 1, column 7
            |
          1 | not = valid = toml
            |       ^
          invalid string
          expected `"`, `'`
    "#);
}

#[test]
fn eval_missing_suite_file_exits_nonzero() {
    let context = test_context!();
    let path = context.temp_dir.join("missing.toml");
    let mut cmd = context.command();
    cmd.args(["eval", path.to_str().unwrap()]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × could not read suite file [TEMP_DIR]/missing.toml
      ╰─▶ No such file or directory (os error 2)
    "#);
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
    let mut v: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    sanitize_eval_report(&mut v);
    fabro_json_snapshot!(context, v, @r#"
    {
      "suite": "smoke",
      "started_at": "[STARTED_AT]",
      "finished_at": "[FINISHED_AT]",
      "pass_rate": 1.0,
      "pass_threshold": 1.0,
      "passed": true,
      "cases": [
        {
          "name": "c1",
          "result": "passed",
          "run_id": "[RUN_ID]",
          "status": "succeeded",
          "duration_ms": "[DURATION_MS]",
          "cost_usd_micros": null,
          "error": null
        },
        {
          "name": "c2",
          "result": "passed",
          "run_id": "[RUN_ID]",
          "status": "succeeded",
          "duration_ms": "[DURATION_MS]",
          "cost_usd_micros": null,
          "error": null
        }
      ]
    }
    "#);
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
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["eval", suite.to_str().unwrap()]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    c1 failed [DURATION]  [ULID]
    passed 0/1 (0%), threshold 100%: FAIL
    ----- stderr -----
    ");
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
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["eval", "--only", "c1", suite.to_str().unwrap()]);
    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    c1 passed [DURATION]  [ULID]
    passed 1/1 (100%), threshold 100%: PASS
    ----- stderr -----
    ");
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
    let mut v: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    sanitize_eval_report(&mut v);
    fabro_json_snapshot!(context, v, @r#"
    {
      "suite": "smoke",
      "started_at": "[STARTED_AT]",
      "finished_at": "[FINISHED_AT]",
      "pass_rate": 1.0,
      "pass_threshold": 1.0,
      "passed": true,
      "cases": [
        {
          "name": "c1",
          "result": "passed",
          "run_id": "[RUN_ID]",
          "status": "succeeded",
          "duration_ms": "[DURATION_MS]",
          "cost_usd_micros": null,
          "error": null
        }
      ]
    }
    "#);
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
    let mut v: Value = serde_json::from_str(stdout.trim()).unwrap();
    sanitize_eval_report(&mut v);
    fabro_json_snapshot!(context, v, @r#"
    {
      "suite": "smoke",
      "started_at": "[STARTED_AT]",
      "finished_at": "[FINISHED_AT]",
      "pass_rate": 0.5,
      "pass_threshold": 0.5,
      "passed": true,
      "cases": [
        {
          "name": "slow",
          "result": "timeout",
          "run_id": "[RUN_ID]",
          "status": "failed",
          "duration_ms": "[DURATION_MS]",
          "cost_usd_micros": null,
          "error": null
        },
        {
          "name": "quick",
          "result": "passed",
          "run_id": "[RUN_ID]",
          "status": "succeeded",
          "duration_ms": "[DURATION_MS]",
          "cost_usd_micros": null,
          "error": null
        }
      ]
    }
    "#);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
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
    let mut filters = context.filters();
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["eval", suite.to_str().unwrap()]);
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    slow timeout [DURATION]  [ULID]
    passed 0/1 (0%), threshold 100%: FAIL
    ----- stderr -----
    ");
}

fn sanitize_eval_report(v: &mut Value) {
    if let Some(cases) = v.get_mut("cases").and_then(|c| c.as_array_mut()) {
        for c in cases {
            if let Some(o) = c.as_object_mut() {
                if o.contains_key("run_id") {
                    o.insert("run_id".into(), Value::String("[RUN_ID]".into()));
                }
                if o.contains_key("duration_ms") && o["duration_ms"].is_number() {
                    o.insert("duration_ms".into(), Value::String("[DURATION_MS]".into()));
                }
                if o.contains_key("cost_usd_micros") && o["cost_usd_micros"].is_number() {
                    o.insert(
                        "cost_usd_micros".into(),
                        Value::String("[COST_USD_MICROS]".into()),
                    );
                }
            }
        }
    }
    if let Some(o) = v.as_object_mut() {
        if o.contains_key("started_at") {
            o.insert("started_at".into(), Value::String("[STARTED_AT]".into()));
        }
        if o.contains_key("finished_at") {
            o.insert("finished_at".into(), Value::String("[FINISHED_AT]".into()));
        }
    }
}
