//! A test double for [`ErrorReport`], for the tests of the submodules that
//! use it.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::Mutex;

use super::super::operations::OutcomeAnomaly;
use super::{ErrorReport, Explanation, GatewayError, McpErrorCode};

type ExitCodeFn = dyn Fn(&GatewayError) -> u8 + Send + Sync;

/// Answers with fixed, recognizable values and records every call. The
/// explanations name the error's variants (from its `Debug` form), so that a
/// test can see which error was explained where. `exit_code` answers 1 unless
/// replaced; `outcome_exit_code` answers 0 for no anomaly and 1 otherwise.
pub(crate) struct TestErrorReport {
    exit_code: Box<ExitCodeFn>,
    calls: Mutex<Vec<ReportCall>>,
}

/// A call the double received. Errors are kept in their `Debug` form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportCall {
    ExitCode(String),
    OutcomeExitCode(Vec<OutcomeAnomaly>),
    Explanation(String),
    McpError(String),
    PanicExplanation,
    McpPanicError,
    MemoryLimitExplanation(u64),
}

impl TestErrorReport {
    pub(crate) fn new() -> Self {
        Self {
            exit_code: Box::new(|_| 1),
            calls: Mutex::default(),
        }
    }

    pub(crate) fn on_exit_code(
        mut self,
        f: impl Fn(&GatewayError) -> u8 + Send + Sync + 'static,
    ) -> Self {
        self.exit_code = Box::new(f);
        self
    }

    pub(crate) fn calls(&self) -> Vec<ReportCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: ReportCall) {
        self.calls.lock().unwrap().push(call);
    }

    /// The explanation the double gives for `error`: a message naming the
    /// error, one cause, and one line of the fix.
    pub(crate) fn explanation_of(error: &GatewayError) -> Explanation {
        explanation(format!("explanation of {error:?}"))
    }

    /// The explanation the double gives for an unexpected failure.
    pub(crate) fn panic_explanation_of() -> Explanation {
        explanation("explanation of a panic".to_owned())
    }

    /// The explanation the double gives for going over the memory limit.
    pub(crate) fn memory_limit_explanation_of(limit_mb: u64) -> Explanation {
        explanation(format!("explanation of the memory limit of {limit_mb} MB"))
    }
}

fn explanation(message: String) -> Explanation {
    Explanation {
        message,
        causes: vec!["a test cause".to_owned()],
        fix: vec!["a test fix".to_owned()],
    }
}

impl Default for TestErrorReport {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorReport for TestErrorReport {
    fn exit_code(&self, error: &GatewayError) -> u8 {
        self.record(ReportCall::ExitCode(format!("{error:?}")));
        (self.exit_code)(error)
    }

    fn outcome_exit_code(&self, anomalies: &[OutcomeAnomaly]) -> u8 {
        self.record(ReportCall::OutcomeExitCode(anomalies.to_vec()));
        u8::from(!anomalies.is_empty())
    }

    fn explanation(&self, error: &GatewayError) -> Explanation {
        self.record(ReportCall::Explanation(format!("{error:?}")));
        Self::explanation_of(error)
    }

    fn mcp_error(&self, error: &GatewayError) -> McpErrorCode {
        self.record(ReportCall::McpError(format!("{error:?}")));
        McpErrorCode {
            code: "test",
            details: serde_json::Map::new(),
        }
    }

    fn panic_explanation(&self) -> Explanation {
        self.record(ReportCall::PanicExplanation);
        Self::panic_explanation_of()
    }

    fn mcp_panic_error(&self) -> McpErrorCode {
        self.record(ReportCall::McpPanicError);
        McpErrorCode {
            code: "internal",
            details: serde_json::Map::new(),
        }
    }

    fn memory_limit_explanation(&self, limit_mb: u64) -> Explanation {
        self.record(ReportCall::MemoryLimitExplanation(limit_mb));
        Self::memory_limit_explanation_of(limit_mb)
    }
}

#[cfg(test)]
mod tests {
    use crate::types::CollectionName;

    use super::*;
    use crate::gateway::operations::OperationError;

    fn not_found() -> GatewayError {
        GatewayError::Operation(OperationError::CollectionNotFound {
            collection: CollectionName("notes".to_owned()),
        })
    }

    #[test]
    fn exit_code_is_1_unless_replaced() {
        let default = TestErrorReport::new();
        let replaced = TestErrorReport::new().on_exit_code(|_| 2);

        assert_eq!(default.exit_code(&not_found()), 1);
        assert_eq!(replaced.exit_code(&not_found()), 2);
    }

    #[test]
    fn outcome_exit_code_is_0_only_without_anomalies() {
        let report = TestErrorReport::new();

        assert_eq!(report.outcome_exit_code(&[]), 0);
        assert_eq!(report.outcome_exit_code(&[OutcomeAnomaly::Declined]), 1);
    }

    #[test]
    fn the_explanation_names_the_error_and_the_call_is_recorded() {
        let report = TestErrorReport::new();

        let explanation = report.explanation(&not_found());

        assert!(explanation.message.contains("CollectionNotFound"));
        assert!(explanation.message.contains("notes"));
        assert_eq!(
            report.calls(),
            vec![ReportCall::Explanation(format!("{:?}", not_found()))]
        );
    }

    #[test]
    fn the_mcp_errors_have_a_test_code_or_internal() {
        let report = TestErrorReport::new();

        let error = report.mcp_error(&not_found());
        let panic = report.mcp_panic_error();

        assert_eq!(error.code, "test");
        assert_eq!(panic.code, "internal");
        assert_eq!(
            report.calls(),
            vec![
                ReportCall::McpError(format!("{:?}", not_found())),
                ReportCall::McpPanicError
            ]
        );
    }

    #[test]
    fn the_panic_explanation_is_the_doubles_and_the_call_is_recorded() {
        let report = TestErrorReport::new();

        let explanation = report.panic_explanation();

        assert_eq!(explanation, TestErrorReport::panic_explanation_of());
        assert_eq!(report.calls(), vec![ReportCall::PanicExplanation]);
    }

    #[test]
    fn the_memory_limit_explanation_names_the_limit_and_the_call_is_recorded() {
        let report = TestErrorReport::new();

        let explanation = report.memory_limit_explanation(8192);

        assert_eq!(
            explanation,
            TestErrorReport::memory_limit_explanation_of(8192)
        );
        assert_eq!(
            report.calls(),
            vec![ReportCall::MemoryLimitExplanation(8192)]
        );
    }
}
