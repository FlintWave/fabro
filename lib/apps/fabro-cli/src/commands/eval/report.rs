use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use strum::IntoStaticStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub(crate) enum CaseResult {
    Passed,
    Failed,
    Timeout,
    Error,
}

impl CaseResult {
    pub(crate) fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CaseReport {
    pub(crate) name:            String,
    pub(crate) result:          CaseResult,
    pub(crate) run_id:          Option<String>,
    pub(crate) status:          Option<String>,
    pub(crate) duration_ms:     Option<u64>,
    pub(crate) cost_usd_micros: Option<u64>,
    pub(crate) error:           Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct EvalReport {
    pub(crate) suite:          String,
    pub(crate) started_at:     String,
    pub(crate) finished_at:    String,
    pub(crate) pass_threshold: f64,
    pub(crate) cases:          Vec<CaseReport>,
}

impl EvalReport {
    pub(crate) fn passed_count(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| c.result == CaseResult::Passed)
            .count()
    }
    pub(crate) fn pass_rate(&self) -> f64 {
        if self.cases.is_empty() {
            0.0
        } else {
            self.passed_count() as f64 / self.cases.len() as f64
        }
    }
    pub(crate) fn passed(&self) -> bool {
        self.pass_rate() >= self.pass_threshold
    }
}

impl Serialize for EvalReport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_struct("EvalReport", 8)?;
        s.serialize_field("suite", &self.suite)?;
        s.serialize_field("started_at", &self.started_at)?;
        s.serialize_field("finished_at", &self.finished_at)?;
        s.serialize_field("pass_rate", &self.pass_rate())?;
        s.serialize_field("pass_threshold", &self.pass_threshold)?;
        s.serialize_field("passed", &self.passed())?;
        s.serialize_field("cases", &self.cases)?;
        s.end()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{self, Value};

    use super::*;

    fn mk_report(passed: usize, total: usize, threshold: f64) -> EvalReport {
        let mut cases = Vec::new();
        for i in 0..total {
            cases.push(CaseReport {
                name:            format!("c{i}"),
                result:          if i < passed {
                    CaseResult::Passed
                } else {
                    CaseResult::Failed
                },
                run_id:          None,
                status:          None,
                duration_ms:     None,
                cost_usd_micros: None,
                error:           None,
            });
        }
        EvalReport {
            suite: "s".into(),
            started_at: "a".into(),
            finished_at: "b".into(),
            pass_threshold: threshold,
            cases,
        }
    }

    #[test]
    fn pass_rate_0_of_3() {
        let r = mk_report(0, 3, 1.0);
        assert!((r.pass_rate() - 0.0).abs() < f64::EPSILON);
        assert!(!r.passed());
    }
    #[test]
    fn pass_rate_2_of_3() {
        let r = mk_report(2, 3, 1.0);
        assert!((r.pass_rate() - 2.0 / 3.0).abs() < 1e-9);
    }
    #[test]
    fn pass_rate_3_of_3() {
        let r = mk_report(3, 3, 1.0);
        assert!((r.pass_rate() - 1.0).abs() < f64::EPSILON);
        assert!(r.passed());
    }

    #[test]
    fn threshold_boundary() {
        let r = mk_report(2, 4, 0.5);
        assert!(r.passed());
        let r = mk_report(2, 4, 0.51);
        assert!(!r.passed());
    }

    #[test]
    fn json_shape_with_nulls() {
        let r = mk_report(1, 1, 1.0);
        // make run_id etc absent already null
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("pass_rate").is_some());
        assert!(v.get("passed").is_some());
        let case = &v["cases"][0];
        assert_eq!(case["run_id"], Value::Null);
        assert_eq!(case["status"], Value::Null);
        assert_eq!(case["duration_ms"], Value::Null);
        assert_eq!(case["cost_usd_micros"], Value::Null);
        assert_eq!(case["error"], Value::Null);
    }
}
