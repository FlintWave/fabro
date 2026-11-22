#![expect(clippy::absolute_paths, reason = "eval module uses crate paths")]
use std::time::Instant;

use anyhow::{Result, bail};
use fabro_util::terminal::Styles;

use crate::args::EvalArgs;
use crate::command_context::CommandContext;
use crate::commands::run::create::create_run;
use crate::commands::run::remote_workflow::Interruption;
use crate::shared::{format_duration_ms, format_usd_micros, print_json_pretty};
mod report;
mod suite;

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "percentage display"
)]
pub(crate) async fn run(args: &EvalArgs, styles: &Styles, base_ctx: &CommandContext) -> Result<()> {
    let suite_path = args.suite.clone();
    let suite = suite::load_suite(&suite_path).map_err(|e| anyhow::anyhow!(e))?;

    // --only validation
    if !args.only.is_empty() {
        for name in &args.only {
            if !suite.cases.iter().any(|c| c.name == *name) {
                bail!("unknown case `{name}`");
            }
        }
    }
    let selected_cases: Vec<&suite::SuiteCase> = if args.only.is_empty() {
        suite.cases.iter().collect()
    } else {
        let only_set: std::collections::BTreeSet<&str> =
            args.only.iter().map(String::as_str).collect();
        suite
            .cases
            .iter()
            .filter(|c| only_set.contains(c.name.as_str()))
            .collect()
    };
    if selected_cases.is_empty() {
        bail!("no cases selected");
    }

    let started_at = chrono::Utc::now().to_rfc3339();
    let _started_instant = Instant::now();
    let printer = base_ctx.printer();
    let mut case_reports = Vec::new();

    for case in selected_cases {
        let case_start = Instant::now();
        // Build RunArgs
        let mut run_args = build_run_args(args, case, &suite);
        // environment override
        if let Some(env) = case.environment.as_ref().or(args.environment.as_ref()) {
            run_args.environment = Some(env.clone());
        }
        // target_from
        run_args.target_from = Some(case.target_from.clone());
        // goal_file
        if let Some(gf) = &case.goal_file {
            run_args.goal_file = Some(gf.clone());
        }
        // workflow
        run_args.workflow = Some(case.workflow.clone());
        // inputs
        for (k, v) in &case.inputs {
            run_args.inputs.values.push(format!("{k}={v}"));
        }
        // labels
        run_args.label.push(format!("eval.suite={}", suite.name));
        run_args.label.push(format!("eval.case={}", case.name));
        run_args.detach = true;

        let ctx = base_ctx.with_target(&run_args.target)?;
        let interruption = Interruption::for_run_args(&run_args);
        let run_result = interruption
            .guard(Box::pin(create_run(&ctx, &run_args, styles, &interruption)))
            .await;
        let run_id = match run_result {
            Ok(cr) => cr.run_id,
            Err(err) => {
                // error case
                let report_case = report::CaseReport {
                    name:            case.name.clone(),
                    result:          report::CaseResult::Error,
                    run_id:          None,
                    status:          None,
                    duration_ms:     Some(
                        u64::try_from(case_start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    ),
                    cost_usd_micros: None,
                    error:           Some(err.to_string()),
                };
                if !args.json {
                    print_case_line(styles, printer, &case.name, "error", &report_case);
                }
                case_reports.push(report_case);
                continue;
            }
        };

        // start
        let client = ctx.server().await?;
        if let Err(err) =
            crate::commands::run::start::start_run_with_client(&client, &run_id, false).await
        {
            let report_case = report::CaseReport {
                name:            case.name.clone(),
                result:          report::CaseResult::Error,
                run_id:          Some(run_id.to_string()),
                status:          None,
                duration_ms:     Some(
                    u64::try_from(case_start.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
                cost_usd_micros: None,
                error:           Some(err.to_string()),
            };
            if !args.json {
                print_case_line(styles, printer, &case.name, "error", &report_case);
            }
            case_reports.push(report_case);
            continue;
        }

        // wait with timeout
        let timeout_dur = suite::parse_duration(&case.timeout)
            .unwrap_or_else(|_| std::time::Duration::from_hours(1));
        let deadline = Instant::now() + timeout_dur;
        let interval = std::time::Duration::from_secs(1);
        let mut final_status = None;
        let mut timed_out = false;
        loop {
            let status = client.retrieve_run(&run_id).await?.lifecycle.status;
            if status.is_terminal() {
                final_status = Some(status);
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            tokio::time::sleep(interval.min(deadline - Instant::now())).await;
        }

        let conclusion = if final_status.is_some() {
            client
                .get_run_state(&run_id)
                .await
                .ok()
                .and_then(|s| s.conclusion)
        } else {
            None
        };

        let duration_ms = case_start.elapsed().as_millis() as u64;
        let (result, status_str) = if timed_out {
            (report::CaseResult::Timeout, None)
        } else {
            let s = final_status.expect("final status");
            let kind = crate::shared::run_status_kind(s);
            if matches!(s, fabro_types::RunStatus::Succeeded { .. }) {
                (report::CaseResult::Passed, Some(kind.to_string()))
            } else {
                (report::CaseResult::Failed, Some(kind.to_string()))
            }
        };
        let cost = conclusion
            .as_ref()
            .and_then(|c| c.usage)
            .and_then(|u| u.cost)
            .map(|c| c.usd_micros);
        let report_case = report::CaseReport {
            name: case.name.clone(),
            result,
            run_id: Some(run_id.to_string()),
            status: status_str,
            duration_ms: Some(duration_ms),
            cost_usd_micros: cost,
            error: None,
        };
        if !args.json {
            print_case_line(
                styles,
                printer,
                &case.name,
                report_case.result.as_str(),
                &report_case,
            );
        }
        case_reports.push(report_case);
    }

    let finished_at = chrono::Utc::now().to_rfc3339();
    let eval_report = report::EvalReport {
        suite: suite.name.clone(),
        started_at,
        finished_at,
        pass_threshold: suite.pass_threshold,
        cases: case_reports,
    };
    // output json
    let final_json = serde_json::to_value(&eval_report)?;

    if args.json {
        print_json_pretty(&final_json)?;
    } else {
        // summary line
        let passed_n = eval_report.passed_count();
        let total = eval_report.cases.len();
        let pct = (eval_report.pass_rate() * 100.0) as u64;
        let thr_pct = (suite.pass_threshold * 100.0) as u64;
        let verdict = if eval_report.pass_rate() >= suite.pass_threshold {
            "PASS"
        } else {
            "FAIL"
        };
        fabro_util::printout!(
            printer,
            "passed {}/{} ({}%), threshold {}%: {}",
            passed_n,
            total,
            pct,
            thr_pct,
            verdict
        );
    }
    if let Some(output_path) = &args.output {
        #[expect(clippy::disallowed_methods, reason = "sync output write")]
        {
            std::fs::write(output_path, serde_json::to_string_pretty(&final_json)?)?;
        }
    }

    if eval_report.pass_rate() >= suite.pass_threshold {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn build_run_args(
    args: &EvalArgs,
    _case: &suite::SuiteCase,
    _suite: &suite::Suite,
) -> crate::args::RunArgs {
    crate::args::RunArgs {
        target:               args.server.clone(),
        inputs:               crate::args::InputOverrideArgs { values: Vec::new() },
        workflow:             None,
        workflow_repo:        None,
        workflow_ref:         None,
        target_from:          None,
        target_repo_selector: None,
        target_repo:          None,
        target_branch:        None,
        dry_run:              false,
        auto_approve:         false,
        goal:                 None,
        goal_file:            None,
        model:                None,
        provider:             None,
        verbose:              false,
        environment:          args.environment.clone(),
        label:                Vec::new(),
        parent:               None,
        preserve_sandbox:     false,
        detach:               false,
    }
}

fn print_case_line(
    _styles: &Styles,
    printer: fabro_util::printer::Printer,
    name: &str,
    result: &str,
    report_case: &report::CaseReport,
) {
    // follow wait style roughly
    let duration = report_case
        .duration_ms
        .map(format_duration_ms)
        .unwrap_or_default();
    let cost = report_case
        .cost_usd_micros
        .map(|c| format!("  {}", format_usd_micros(c)))
        .unwrap_or_default();
    let run_id = report_case.run_id.clone().unwrap_or_default();
    fabro_util::printout!(
        printer,
        "{} {} {} {} {}",
        name,
        result,
        duration,
        cost,
        run_id
    );
}
