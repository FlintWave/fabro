//! A resume through the engine assembly: a run whose creation a crash cut
//! short starts again from its admitted graphs, and a run whose store
//! failed ends its lifetime without an end of its own.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use fabro_petri::admission::AdmittedGraphs;
use fabro_petri::artifacts::StoreArtifactWriter;
use fabro_petri::check::Launch;
use fabro_petri::checkpoint::{BUDGET_EXHAUSTED_CLASS, RunGitSettings};
use fabro_petri::engine::{self, Conclusion, Execution, RunError, RunStatus};
use fabro_petri::hooks::{HooksSpec, budget_seed_from_projection};
use fabro_petri::projection::{Item, RunView};
use fabro_petri::runtime::RuntimeSpec;
use fabro_petri::test_support::MemoryPlatformRecords;
use fabro_store::platform_records::{PlatformRecord, RunCreatedRecord, StoredPlatformRecord};
use fabro_store::{ArtifactStore, RunProjection};
use fabro_types::{RunId, StageId, test_support as types_support};
use lithos_llm::types::{Cost, CostSource, TokenCounts, Usage};
use object_store::memory::InMemory;
use pebble_coding_agent::test_support::ScriptedCall;
use petri_execution::events::replay_run;
use petri_execution::{CancelReason, CoordinatorError, InvocationId, host, inspect};
use petri_runtime::RunOptions;
use petri_runtime::executor::Retention;
use petri_runtime::ir::{
    Attempt, Backoff, FailureInfo, Graph, GraphBuilder, Outcome, RetryOn, RetryPolicy,
    RunStatus as PetriRunStatus, ScopeId, Status, StepRef, validate,
};
use petri_runtime::steps::{Step, StepCtx};
use petri_store::{
    Access, Digest, LogId, MemoryRunStore, OwnerId, Record, RunKey, RunLogs, RunStore, StoreError,
};
use serde_json::{Value, json};
use support::{SETTINGS, Silent, admit, all_records, no_questions, run_request};
use tokio::runtime::Handle;

/// One command stage between start and exit.
const COMMAND: &str = r#"digraph Command {
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="true"]
    start -> say -> exit
}"#;

/// A crash cut the run's creation short: its key is stored, and nothing
/// else. The resume Petri refuses as never started becomes a start from
/// the admitted graphs, under the same key.
#[tokio::test]
async fn a_resume_of_a_run_that_never_started_starts_it_again() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(MemoryRunStore::new());
    drop(
        store
            .open(&RunKey::new("cut-short"), Access::Create {
                owner: OwnerId::new("crashed"),
            })
            .await
            .expect("the key is stored"),
    );
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", COMMAND), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let outcome = engine::run(resume_request(
        "cut-short",
        root.path(),
        graphs,
        store,
        runtime,
    ))
    .await
    .expect("the run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
}

/// A write to the run's store fails partway through the run: the lifetime
/// ends with the store's failure, which interrupts the run rather than
/// failing it, and nothing records an end. A resume over the same records
/// finishes the run.
#[tokio::test]
async fn a_failed_store_write_interrupts_the_run_and_a_resume_finishes_it() {
    let root = tempfile::tempdir().expect("a temp dir");
    let memory = Arc::new(MemoryRunStore::new());
    let failing = Arc::new(FailingStore {
        inner:    Arc::clone(&memory),
        log:      LogId::Resources,
        appended: Arc::new(AtomicUsize::new(0)),
    });
    let runtime = RuntimeSpec::default();
    let graphs = || {
        admit(
            &[("workflow.fabro", COMMAND), ("workflow.toml", SETTINGS)],
            Launch::default(),
            &runtime,
        )
    };

    let first = engine::run(run_request(
        "interrupted",
        root.path(),
        graphs(),
        failing,
        runtime.clone(),
        no_questions(Arc::new(Silent)),
    ))
    .await;

    assert!(
        matches!(&first, Err(RunError::StoreFailed(message)) if message.contains("the disk is full")),
        "{first:?}"
    );
    assert!(
        matches!(engine::conclusion(&first), Conclusion::Interrupted { .. }),
        "{first:?}"
    );
    assert!(!finished(&memory).await, "nothing ended the run");

    let outcome = engine::run(resume_request(
        "interrupted",
        root.path(),
        graphs(),
        Arc::clone(&memory) as Arc<dyn RunStore>,
        runtime,
    ))
    .await
    .expect("the resumed run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
    assert!(finished(&memory).await, "the resume ended the run");
}

/// Whether the interrupted run's records hold Petri's own finish.
async fn finished(store: &MemoryRunStore) -> bool {
    all_records(store, "interrupted")
        .await
        .iter()
        .any(|record| record["body"]["event"] == "run.finished")
}

/// A resume of the run over `store`, with the admitted graphs to start it
/// from should its creation have been cut short.
fn resume_request(
    run_id: &str,
    run_dir: &std::path::Path,
    graphs: AdmittedGraphs,
    store: Arc<dyn RunStore>,
    runtime: RuntimeSpec,
) -> engine::RunRequest {
    let mut request = run_request(
        run_id,
        run_dir,
        graphs,
        store,
        runtime,
        no_questions(Arc::new(Silent)),
    );
    let Execution::Start(graphs) = request.execution else {
        unreachable!("run_request builds a start");
    };
    request.execution = Execution::Resume(graphs);
    request
}

/// The in-memory store, failing its first append to `log` as a full disk
/// would, before anything is stored.
struct FailingStore {
    inner:    Arc<MemoryRunStore>,
    log:      LogId,
    appended: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RunStore for FailingStore {
    async fn open(&self, key: &RunKey, access: Access) -> Result<Arc<dyn RunLogs>, StoreError> {
        Ok(Arc::new(FailingLogs {
            inner:    self.inner.open(key, access).await?,
            log:      self.log,
            appended: Arc::clone(&self.appended),
        }))
    }
}

struct FailingLogs {
    inner:    Arc<dyn RunLogs>,
    log:      LogId,
    appended: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RunLogs for FailingLogs {
    fn locator(&self) -> String {
        self.inner.locator()
    }

    async fn append(&self, log: &LogId, records: &[Record]) -> Result<(), StoreError> {
        if *log == self.log && self.appended.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(StoreError::backend(
                self.locator(),
                "append",
                "the disk is full",
            ));
        }
        self.inner.append(log, records).await
    }

    async fn read(&self, log: &LogId) -> Result<Vec<Record>, StoreError> {
        self.inner.read(log).await
    }

    async fn put_blob(&self, bytes: &[u8]) -> Result<Digest, StoreError> {
        self.inner.put_blob(bytes).await
    }

    async fn get_blob(&self, digest: Digest) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_blob(digest).await
    }
}

#[tokio::test]
async fn resume_uses_the_persisted_stall_budget_not_the_fallback_graph() {
    resumed_stall_policy("1s", "0s", RunStatus::Cancelled).await;
}

#[tokio::test]
async fn resume_preserves_a_persisted_disabled_stall_budget() {
    resumed_stall_policy("0s", "1s", RunStatus::Success).await;
}

async fn resumed_stall_policy(persisted: &str, fallback: &str, expected: RunStatus) {
    let root = tempfile::tempdir().expect("a temporary run directory");
    let memory = Arc::new(MemoryRunStore::new());
    let runtime = RuntimeSpec::default();
    let graphs = |budget| {
        admit(
            &[
                (
                    "workflow.fabro",
                    &format!(
                        r#"digraph Resume {{
            graph [stall_timeout="{budget}"]
            start [shape=Mdiamond]
            work [shape=parallelogram, script="sleep 2", timeout="10s"]
            exit [shape=Msquare]
            start -> work -> exit
        }}"#
                    ),
                ),
                ("workflow.toml", SETTINGS),
            ],
            Launch::default(),
            &runtime,
        )
    };
    // Fail after the root graph is durable but before acquiring a workspace.
    // This exercises cleanup on host error and a genuine persisted resume.
    let failing = Arc::new(FailingStore {
        inner:    memory.clone(),
        log:      LogId::Resources,
        appended: Arc::new(AtomicUsize::new(0)),
    });
    let first = engine::run(run_request(
        "stall-resume",
        root.path(),
        graphs(persisted),
        failing,
        runtime.clone(),
        no_questions(Arc::new(Silent)),
    ))
    .await;
    assert!(matches!(first, Err(RunError::StoreFailed(_))), "{first:?}");
    let metrics = Handle::current().metrics();
    support::wait_until("error cleanup", || metrics.num_alive_tasks() == 0).await;
    let outcome = engine::run(resume_request(
        "stall-resume",
        root.path(),
        graphs(fallback),
        memory.clone(),
        runtime,
    ))
    .await
    .expect("the resumed run completes");
    assert_eq!(outcome.status, expected, "{outcome:?}");
    let logs = memory
        .open(&RunKey::new("stall-resume"), Access::Read)
        .await
        .expect("the resumed record opens");
    let state = host::stored_state(&*logs)
        .await
        .expect("the stored coordinator state replays");
    let reason = &state.invocations[&InvocationId::ROOT].cancel_reason;
    if expected == RunStatus::Cancelled {
        assert!(
            matches!(
                reason,
                Some(CancelReason::StallTimeout {
                    stall_timeout_ms: 1000,
                    ..
                })
            ),
            "{reason:?}"
        );
    } else {
        assert_eq!(*reason, None);
    }
    support::wait_until("resume cleanup", || metrics.num_alive_tasks() == 0).await;
}

/// Two agent stages the scripted model prices: `alpha` at 400 micro-dollars
/// and `omega` at 700.
const PRICED: &str = r#"digraph Budget {
    graph [goal="Resume under a budget", backend="api", default_max_retries=0]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    alpha [shape=box, prompt="Write alpha.txt", model="test/model"]
    omega [shape=box, prompt="Write omega.txt", model="test/model"]
    start -> alpha -> omega -> exit
}"#;

/// The scripted exchanges for one agent stage, its pair priced
/// `usd_micros`.
fn priced_stage(file: &str, usd_micros: u64) -> Vec<ScriptedCall> {
    use pebble_coding_agent::test_support::{text_response, tool_call_response, with_cost};
    use serde_json::json;

    vec![
        ScriptedCall::response(with_cost(
            tool_call_response(
                "shell",
                "write",
                json!({"command": format!("echo priced > {file}")}),
            ),
            usd_micros,
        )),
        ScriptedCall::response(with_cost(text_response("wrote it"), 0)),
    ]
}

/// The in-memory store, interrupting the run once by failing the append
/// that records the attempt priced `usd_micros` micro-dollars: the crash a
/// resume then replays past.
struct PricedCrashStore {
    inner:      Arc<MemoryRunStore>,
    usd_micros: u64,
    crashed:    Arc<AtomicBool>,
}

/// Hooks without a spend cap (`None`), sharing `records`.
fn uncapped_hooks(records: &Arc<MemoryPlatformRecords>) -> HooksSpec {
    HooksSpec {
        records:            records.clone(),
        git:                RunGitSettings::default(),
        artifacts:          Vec::new(),
        test_gates:         None,
        artifact_writer:    Arc::new(StoreArtifactWriter::new(ArtifactStore::new(
            Arc::new(InMemory::new()),
            "budget-resume-test",
        ))),
        source:             None,
        publisher:          None,
        budget_usd_micros:  None,
        budget_seed_micros: 0,
    }
}

/// A crash after `alpha` (400 micro-dollars) finished but before `omega`
/// (700) was recorded: the resume's spend total starts from the finished
/// stages' recorded usage, so under a 1000-micro-dollar cap the resumed
/// `omega` — 700 on its own, within the cap — exhausts it, 400 already
/// spent. Without the seed it would finish and the run would succeed.
#[tokio::test]
async fn a_resumed_run_counts_the_cost_of_the_stages_that_finished_before_it() {
    use pebble_coding_agent::test_support::scripted_client;

    let root = tempfile::tempdir().expect("a temporary run directory");
    let run_dir = root.path().join("run");
    let run_id = RunId::new();
    let run_key = || RunKey::new(run_id.to_string());
    let memory = Arc::new(MemoryRunStore::new());
    let records = Arc::new(MemoryPlatformRecords::new());

    // The first run: uncapped, crashing when the 700-micro-dollar attempt's
    // finish is appended.
    let calls = [
        priced_stage("alpha.txt", 400),
        priced_stage("omega.txt", 700),
    ]
    .concat();
    let (client, _provider) = scripted_client(calls);
    let runtime = RuntimeSpec {
        model_client: Some(client),
        ..RuntimeSpec::default()
    };
    let mut first = run_request(
        &run_id.to_string(),
        &run_dir,
        admit(
            &[("workflow.fabro", PRICED), ("workflow.toml", SETTINGS)],
            Launch::default(),
            &runtime,
        ),
        Arc::new(PricedCrashStore {
            inner:      Arc::clone(&memory),
            usd_micros: 700,
            crashed:    Arc::new(AtomicBool::new(false)),
        }),
        runtime,
        no_questions(Arc::new(Silent)),
    );
    first.hooks = Some(uncapped_hooks(&records));
    let first = engine::run(first).await;
    assert!(
        matches!(&first, Err(RunError::StoreFailed(message)) if message.contains("the disk is full")),
        "{first:?}"
    );
    let metrics = Handle::current().metrics();
    support::wait_until("error cleanup", || metrics.num_alive_tasks() == 0).await;

    // The seed the worker would give the resume: the finished stages'
    // recorded usage. `alpha` and `start` finished; `omega` did not.
    let logs = memory
        .open(&run_key(), Access::Read)
        .await
        .expect("the interrupted run opens");
    let events = replay_run(&*logs).await.expect("the record replays");
    let mut spec = types_support::test_run_spec();
    spec.run_id = run_id;
    let created = StoredPlatformRecord {
        seq:         1,
        recorded_at: 0,
        record:      PlatformRecord::RunCreated(RunCreatedRecord {
            spec,
            title: None,
            parent_id: None,
            retried_from: None,
            web_url: None,
        }),
        position:    None,
    };
    let mut view = RunView::new();
    view.fold(&Item::Platform(&created), 1);
    for (index, event) in events.iter().enumerate() {
        view.fold(&Item::Petri(event), index as u64 + 2);
    }
    let seed =
        budget_seed_from_projection(view.projection().expect("the interrupted run projects"));
    assert_eq!(seed, 400, "only the stage that finished counts");

    // The resume: a cap of 1000 micro-dollars, seeded with what the first
    // run spent. `omega`'s 700 reaches 1100 and the budget refuses it.
    let (client, omega_requests) = scripted_client(priced_stage("omega.txt", 700));
    let runtime = RuntimeSpec {
        model_client: Some(client),
        ..RuntimeSpec::default()
    };
    let mut hooks = uncapped_hooks(&records);
    hooks.budget_usd_micros = Some(1000);
    hooks.budget_seed_micros = seed;
    let mut resumed = resume_request(
        &run_id.to_string(),
        &run_dir,
        admit(
            &[("workflow.fabro", PRICED), ("workflow.toml", SETTINGS)],
            Launch::default(),
            &runtime,
        ),
        Arc::clone(&memory) as Arc<dyn RunStore>,
        runtime,
    );
    resumed.hooks = Some(hooks);
    let outcome = engine::run(resumed).await.expect("the resumed run ends");

    assert_eq!(outcome.status, RunStatus::Failed, "{outcome:?}");
    let failure = outcome.failure.expect("the run failed with a reason");
    assert!(
        failure.contains("run budget exhausted: spent $0.001100 of $0.001000"),
        "the seed counts: 400 + 700 of the 1000-micro-dollar cap: {failure}"
    );
    assert_eq!(
        omega_requests.requests().len(),
        2,
        "omega re-ran alone; alpha was not replayed"
    );
    let logs = memory
        .open(&run_key(), Access::Read)
        .await
        .expect("the resumed run opens");
    let inspection = inspect::inspect_run(&*logs)
        .await
        .expect("the resumed run inspects");
    let omega = inspection
        .executions
        .iter()
        .filter_map(|execution| execution.engine.as_ref())
        .flat_map(|engine| engine.attempts.iter())
        .find(|attempt| attempt.node.as_deref() == Some("omega"))
        .expect("omega finished");
    assert_eq!(
        omega.failure.as_ref().map(|failure| failure.class.as_str()),
        Some(BUDGET_EXHAUSTED_CLASS),
        "{omega:?}"
    );
}

/// The in-memory store logs, failing the one append whose records carry the
/// `usd_micros` marker.
struct PricedCrashLogs {
    inner:      Arc<dyn RunLogs>,
    usd_micros: u64,
    crashed:    Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl RunStore for PricedCrashStore {
    async fn open(&self, key: &RunKey, access: Access) -> Result<Arc<dyn RunLogs>, StoreError> {
        Ok(Arc::new(PricedCrashLogs {
            inner:      self.inner.open(key, access).await?,
            usd_micros: self.usd_micros,
            crashed:    Arc::clone(&self.crashed),
        }))
    }
}

#[async_trait::async_trait]
impl RunLogs for PricedCrashLogs {
    fn locator(&self) -> String {
        self.inner.locator()
    }

    async fn append(&self, log: &LogId, records: &[Record]) -> Result<(), StoreError> {
        let marker = format!("\"usd_micros\":{}", self.usd_micros);
        let hit = records
            .iter()
            .any(|record| record.record.to_string().contains(&marker));
        if hit && !self.crashed.swap(true, Ordering::SeqCst) {
            return Err(StoreError::backend(
                self.locator(),
                "append",
                "the disk is full",
            ));
        }
        self.inner.append(log, records).await
    }

    async fn read(&self, log: &LogId) -> Result<Vec<Record>, StoreError> {
        self.inner.read(log).await
    }

    async fn put_blob(&self, bytes: &[u8]) -> Result<Digest, StoreError> {
        self.inner.put_blob(bytes).await
    }

    async fn get_blob(&self, digest: Digest) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_blob(digest).await
    }
}

/// A stage the engine retries within one visit spends on every attempt: a
/// failed attempt that spent money stays in the record, and a seed read off
/// the finished stage's `usage` — the last attempt's alone — loses it. A
/// scripted api agent cannot produce this case: its model failures are
/// classed `llm:*`, and Fabro's retry policy retries `retry_requested`
/// failures alone (a timed-out ACP turn, a human gate's retry). The graph
/// below is therefore built on `PricedStep`, which records exactly what
/// such a stage records — a `pebble.usage` price on every finished attempt.
///
/// `alpha` runs with one retry: its first attempt charges 400 micro-dollars
/// and fails retryably, its second charges 500 and succeeds; `omega` (700)
/// crashes the run when its finish is appended. The crashed run's records
/// project `alpha` at 900 spent against a `usage` of 500, so the resume's
/// seed is 900 — not 500.
#[tokio::test]
async fn a_resumed_run_counts_the_cost_of_a_stages_retried_attempts() {
    let root = tempfile::tempdir().expect("a temporary run directory");
    let run_dir = root.path().join("run");
    let run_id = RunId::new();
    let memory = Arc::new(MemoryRunStore::new());
    let crashing = Arc::new(PricedCrashStore {
        inner:      Arc::clone(&memory),
        usd_micros: 700,
        crashed:    Arc::new(AtomicBool::new(false)),
    });
    let mut options = RunOptions::new(&run_dir);
    options.run_key = Some(RunKey::new(run_id.to_string()));
    options.grace = Duration::from_secs(2);
    options.retention = Retention::Never;
    options.echo = false;
    // Fabro's runtime assembly: the host scope runs in process.
    let runtime = RuntimeSpec::default()
        .runtime(true)
        .step(PricedStep)
        .store(crashing)
        .options(options);

    let first = host::run(&runtime, retried_graph()).await;
    let status = first.as_ref().map(|report| report.status);
    assert!(
        matches!(
            &first,
            Err(host::HostError::Coordinator(CoordinatorError::StoreFailed(message)))
                if message.contains("the disk is full")
        ),
        "the crash interrupts the run the way the neighbouring test's does: {status:?}"
    );

    // The projection the resume's seed is read from, rebuilt from the run's
    // stored records.
    let projection = rebuilt_projection(&memory, run_id).await;
    let alpha = projection
        .stage(&StageId::new("alpha", 1))
        .expect("alpha is projected");
    assert!(
        alpha.completion.is_some(),
        "alpha's visit finished: {alpha:?}"
    );
    assert_eq!(
        alpha.attempt_cost_usd_micros, 900,
        "both attempts are charged: {alpha:?}"
    );
    assert_eq!(
        alpha.usage.cost.as_ref().map_or(0, |cost| cost.usd_micros),
        500,
        "usage keeps the last attempt alone, which the old seed summed: {alpha:?}"
    );
    let seed = budget_seed_from_projection(&projection);
    assert_eq!(seed, 900, "the seed keeps the retried attempt's cost");

    // The resume completes omega, and the projection the finished run's
    // records rebuild still carries both of alpha's attempts.
    let report = host::resume(&runtime).await.expect("the resumed run ends");
    assert_eq!(report.status, PetriRunStatus::Success);
    let projection = rebuilt_projection(&memory, run_id).await;
    let alpha = projection
        .stage(&StageId::new("alpha", 1))
        .expect("alpha is still projected");
    assert_eq!(
        alpha.attempt_cost_usd_micros, 900,
        "the rebuild on resume carries both attempts' cost: {alpha:?}"
    );
    assert_eq!(budget_seed_from_projection(&projection), 900 + 700);
}

/// `alpha` with one retry over a tiny backoff, then `omega`.
fn retried_graph() -> Graph {
    let mut builder = GraphBuilder::new();
    let scope = ScopeId::new(0);
    let alpha = builder.add_node(
        "alpha",
        scope,
        StepRef::new(PRICED_STEP_KIND, json!({"fail_cost": 400, "cost": 500})),
    );
    let omega = builder.add_node(
        "omega",
        scope,
        StepRef::new(PRICED_STEP_KIND, json!({"cost": 700})),
    );
    builder.node_mut(alpha).retry = RetryPolicy::attempts(2)
        .with_backoff(Backoff {
            initial: Duration::from_millis(10),
            factor:  1.0,
            max:     Duration::from_millis(10),
            jitter:  false,
        })
        .with_retry_on(RetryOn::classes(&["retry_requested"]));
    builder.link(alpha, omega);
    let graph = builder.build();
    validate(&graph).expect("the graph is valid");
    graph
}

/// The step kind of `PricedStep`, vendored so no built-in kind collides.
const PRICED_STEP_KIND: &str = "fabro-test/priced";

/// A step whose every finished attempt is priced: its outcome carries the
/// `pebble.usage` metric a priced agent attempt's outcome carries. With
/// `fail_cost` nonzero the first attempt spends it and fails retryably, so
/// the engine retries the node; every later attempt succeeds at `cost`.
struct PricedStep;

#[async_trait::async_trait]
impl Step for PricedStep {
    const NAME: &'static str = PRICED_STEP_KIND;

    type Config = PricedConfig;

    async fn run(&self, config: Self::Config, ctx: StepCtx) -> Outcome {
        let failed = config.fail_cost != 0 && ctx.attempt == Attempt::FIRST;
        let mut outcome = if failed {
            Outcome::new(
                Status::Failure(
                    FailureInfo::new("the first attempt is scripted to fail")
                        .with_class("retry_requested"),
                ),
                Value::Null,
            )
        } else {
            Outcome::new(Status::Success, Value::Null)
        };
        let usd_micros = if failed {
            config.fail_cost
        } else {
            config.cost
        };
        outcome
            .metrics
            .custom
            .insert("pebble.usage".into(), json!(priced_usage(usd_micros)));
        outcome
    }
}

#[derive(serde::Deserialize)]
struct PricedConfig {
    /// The cost of a scripted first-attempt failure; zero fails nothing.
    #[serde(default)]
    fail_cost: u64,
    /// The cost a successful attempt charges.
    cost:      u64,
}

/// A usage at the price the scripted runs price themselves.
fn priced_usage(usd_micros: u64) -> Usage {
    Usage {
        tokens: TokenCounts {
            input: 20,
            output: 10,
            ..TokenCounts::default()
        },
        cost:   Some(Cost {
            usd_micros,
            source: CostSource::Catalog,
        }),
    }
}

/// The projection the run's stored records rebuild: the platform
/// `run.created` record, then Petri's events in record order, folded as the
/// projector folds them.
async fn rebuilt_projection(store: &MemoryRunStore, run_id: RunId) -> RunProjection {
    let logs = store
        .open(&RunKey::new(run_id.to_string()), Access::Read)
        .await
        .expect("the run opens");
    let events = replay_run(&*logs).await.expect("the record replays");
    let mut spec = types_support::test_run_spec();
    spec.run_id = run_id;
    let created = StoredPlatformRecord {
        seq:         1,
        recorded_at: 0,
        record:      PlatformRecord::RunCreated(RunCreatedRecord {
            spec,
            title: None,
            parent_id: None,
            retried_from: None,
            web_url: None,
        }),
        position:    None,
    };
    let mut view = RunView::new();
    view.fold(&Item::Platform(&created), 1);
    for (index, event) in events.iter().enumerate() {
        view.fold(&Item::Petri(event), index as u64 + 2);
    }
    view.projection().expect("the run projects").clone()
}
