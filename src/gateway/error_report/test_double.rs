//! A test double for [`ErrorReport`], for the tests of the submodules that
//! use it.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::Mutex;

use super::super::operations::OutcomeAnomaly;
use super::{ErrorReport, ExplainStyle, GatewayError, McpErrorBody};

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
    Explain(String, ExplainStyle),
    McpError(String),
    ExplainPanic(ExplainStyle),
    McpPanicError,
    ExplainMemoryLimit(u64, ExplainStyle),
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

    /// The explanation the double gives for `error`.
    pub(crate) fn explanation_of(error: &GatewayError) -> String {
        format!("explanation of {error:?}")
    }

    /// The explanation the double gives for an unexpected failure.
    pub(crate) const PANIC_EXPLANATION: &'static str = "explanation of a panic";

    /// The explanation the double gives for going over the memory limit.
    pub(crate) fn memory_limit_explanation(limit_mb: u64) -> String {
        format!("explanation of the memory limit of {limit_mb} MB")
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

    fn explain(&self, error: &GatewayError, style: ExplainStyle) -> String {
        self.record(ReportCall::Explain(format!("{error:?}"), style));
        Self::explanation_of(error)
    }

    fn mcp_error(&self, error: &GatewayError) -> McpErrorBody {
        self.record(ReportCall::McpError(format!("{error:?}")));
        McpErrorBody {
            code: "test",
            message: Self::explanation_of(error),
            details: serde_json::Map::new(),
        }
    }

    fn explain_panic(&self, style: ExplainStyle) -> String {
        self.record(ReportCall::ExplainPanic(style));
        Self::PANIC_EXPLANATION.to_owned()
    }

    fn mcp_panic_error(&self) -> McpErrorBody {
        self.record(ReportCall::McpPanicError);
        McpErrorBody {
            code: "internal",
            message: Self::PANIC_EXPLANATION.to_owned(),
            details: serde_json::Map::new(),
        }
    }

    fn explain_memory_limit(&self, limit_mb: u64, style: ExplainStyle) -> String {
        self.record(ReportCall::ExplainMemoryLimit(limit_mb, style));
        Self::memory_limit_explanation(limit_mb)
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
    fn explain_names_the_error_and_records_the_style() {
        let report = TestErrorReport::new();
        let style = ExplainStyle {
            color: true,
            replace_controls: true,
        };

        let explanation = report.explain(&not_found(), style);

        assert!(explanation.contains("CollectionNotFound"));
        assert!(explanation.contains("notes"));
        assert_eq!(
            report.calls(),
            vec![ReportCall::Explain(format!("{:?}", not_found()), style)]
        );
    }

    #[test]
    fn the_mcp_errors_carry_the_explanations() {
        let report = TestErrorReport::new();

        let error = report.mcp_error(&not_found());
        let panic = report.mcp_panic_error();

        assert_eq!(error.message, TestErrorReport::explanation_of(&not_found()));
        assert_eq!(panic.code, "internal");
        assert_eq!(panic.message, TestErrorReport::PANIC_EXPLANATION);
    }

    #[test]
    fn explain_panic_gives_the_panic_explanation_and_records_the_style() {
        let report = TestErrorReport::new();
        let style = ExplainStyle {
            color: false,
            replace_controls: true,
        };

        let explanation = report.explain_panic(style);

        assert_eq!(explanation, TestErrorReport::PANIC_EXPLANATION);
        assert_eq!(report.calls(), vec![ReportCall::ExplainPanic(style)]);
    }

    #[test]
    fn explain_memory_limit_names_the_limit_and_records_the_style() {
        let report = TestErrorReport::new();
        let style = ExplainStyle {
            color: true,
            replace_controls: false,
        };

        let explanation = report.explain_memory_limit(8192, style);

        assert_eq!(explanation, TestErrorReport::memory_limit_explanation(8192));
        assert_eq!(
            report.calls(),
            vec![ReportCall::ExplainMemoryLimit(8192, style)]
        );
    }
}
