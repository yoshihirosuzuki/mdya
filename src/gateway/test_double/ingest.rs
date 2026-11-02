//! A test double for [`Ingest`], standing for any ingest implementation.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]
#![expect(
    clippy::result_large_err,
    reason = "the handlers return `update`'s error, as the trait does"
)]

use std::sync::Mutex;

use async_trait::async_trait;

use crate::ingest::{
    Ingest, IngestError, LocationBase, PutInstruction, UpdateInstruction, UpdateReport,
    UpdateStopped,
};
use crate::types::{DocumentOutcome, Progress, ProgressSink, TypeAttributes};

type CheckFn =
    dyn Fn(TypeAttributes, &LocationBase) -> Result<TypeAttributes, IngestError> + Send + Sync;
type UpdateFn = dyn Fn(&UpdateInstruction, &dyn ProgressSink) -> Result<UpdateReport, UpdateStopped>
    + Send
    + Sync;
type PutFn = dyn Fn(&PutInstruction) -> Result<DocumentOutcome, IngestError> + Send + Sync;

/// Answers every function from a handler and records every call. By default
/// it supports both update and put, returns the attributes it is given,
/// updates nothing (reporting no progress), and adds every document.
pub(crate) struct TestIngest {
    supports_update: bool,
    supports_put: bool,
    check: Box<CheckFn>,
    update: Box<UpdateFn>,
    put: Box<PutFn>,
    calls: Mutex<Vec<IngestCall>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum IngestCall {
    CheckAttributes(TypeAttributes, LocationBase),
    Update(UpdateInstruction),
    Put(PutInstruction),
}

impl TestIngest {
    pub(crate) fn new() -> Self {
        Self {
            supports_update: true,
            supports_put: true,
            check: Box::new(|attributes, _| Ok(attributes)),
            update: Box::new(|_, _| Ok(UpdateReport::default())),
            put: Box::new(|_| Ok(DocumentOutcome::Added)),
            calls: Mutex::default(),
        }
    }

    pub(crate) fn supporting(mut self, update: bool, put: bool) -> Self {
        self.supports_update = update;
        self.supports_put = put;
        self
    }

    pub(crate) fn on_check_attributes(
        mut self,
        f: impl Fn(TypeAttributes, &LocationBase) -> Result<TypeAttributes, IngestError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.check = Box::new(f);
        self
    }

    /// The handler may report progress to the sink, as an implementation
    /// would.
    pub(crate) fn on_update(
        mut self,
        f: impl Fn(&UpdateInstruction, &dyn ProgressSink) -> Result<UpdateReport, UpdateStopped>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.update = Box::new(f);
        self
    }

    pub(crate) fn on_put(
        mut self,
        f: impl Fn(&PutInstruction) -> Result<DocumentOutcome, IngestError> + Send + Sync + 'static,
    ) -> Self {
        self.put = Box::new(f);
        self
    }

    pub(crate) fn calls(&self) -> Vec<IngestCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: IngestCall) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Default for TestIngest {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Ingest for TestIngest {
    fn supports_update(&self) -> bool {
        self.supports_update
    }

    fn supports_put(&self) -> bool {
        self.supports_put
    }

    async fn check_attributes(
        &self,
        attributes: TypeAttributes,
        base: &LocationBase,
    ) -> Result<TypeAttributes, IngestError> {
        self.record(IngestCall::CheckAttributes(
            attributes.clone(),
            base.clone(),
        ));
        (self.check)(attributes, base)
    }

    async fn update(
        &self,
        instruction: UpdateInstruction,
        progress: &dyn ProgressSink,
    ) -> Result<UpdateReport, UpdateStopped> {
        self.record(IngestCall::Update(instruction.clone()));
        (self.update)(&instruction, progress)
    }

    async fn put(&self, instruction: PutInstruction) -> Result<DocumentOutcome, IngestError> {
        self.record(IngestCall::Put(instruction.clone()));
        (self.put)(&instruction)
    }
}

/// Reports `done` documents of `total` to the sink: a helper for `on_update`
/// handlers.
pub(crate) fn report_documents(progress: &dyn ProgressSink, done: u64, total: u64) {
    progress.report(Progress {
        done,
        total: Some(total),
    });
}

#[cfg(test)]
mod tests {
    use crate::gateway::operations::test_double::{ProgressEvent, TestProgress};
    use crate::types::{CollectionName, Key};

    use super::*;

    fn instruction() -> UpdateInstruction {
        UpdateInstruction {
            collection: CollectionName("notes".to_owned()),
            attributes: TypeAttributes::default(),
            overwrite: false,
            parallelism: 8,
        }
    }

    #[tokio::test]
    async fn by_default_it_supports_everything_and_changes_nothing() {
        let ingest = TestIngest::new();
        let progress = TestProgress::new();

        let report = ingest.update(instruction(), &progress).await.unwrap();
        let outcome = ingest
            .put(PutInstruction {
                collection: CollectionName("memo".to_owned()),
                attributes: TypeAttributes::default(),
                key: Key("a".to_owned()),
                media_type: "text/markdown".to_owned(),
                content: b"# a".to_vec(),
                overwrite: false,
            })
            .await
            .unwrap();

        assert!(ingest.supports_update() && ingest.supports_put());
        assert_eq!(report, UpdateReport::default());
        assert_eq!(outcome, DocumentOutcome::Added);
        assert!(progress.events().is_empty());
    }

    #[tokio::test]
    async fn an_update_handler_can_report_progress() {
        let ingest = TestIngest::new().on_update(|_, progress| {
            report_documents(progress, 1, 2);
            Ok(UpdateReport {
                added: 1,
                ..UpdateReport::default()
            })
        });
        let progress = TestProgress::new();

        let report = ingest.update(instruction(), &progress).await.unwrap();

        assert_eq!(report.added, 1);
        assert_eq!(
            progress.events(),
            vec![ProgressEvent::Report(Progress {
                done: 1,
                total: Some(2)
            })]
        );
        assert_eq!(ingest.calls(), vec![IngestCall::Update(instruction())]);
    }

    #[tokio::test]
    async fn check_attributes_records_the_base() {
        let ingest = TestIngest::new().supporting(false, true);
        let base = LocationBase {
            working_dir: "/work".into(),
            home_dir: None,
        };

        let attributes = ingest
            .check_attributes(TypeAttributes::default(), &base)
            .await
            .unwrap();

        assert_eq!(attributes, TypeAttributes::default());
        assert!(!ingest.supports_update());
        assert_eq!(
            ingest.calls(),
            vec![IngestCall::CheckAttributes(TypeAttributes::default(), base)]
        );
    }

    #[tokio::test]
    async fn a_replaced_put_answers_and_put_support_can_be_turned_off() {
        let ingest = TestIngest::new()
            .supporting(true, false)
            .on_put(|_| Err(IngestError::PutNotSupported));
        let instruction = PutInstruction {
            collection: CollectionName("notes".to_owned()),
            attributes: TypeAttributes::default(),
            key: Key("a".to_owned()),
            media_type: "text/markdown".to_owned(),
            content: b"# a".to_vec(),
            overwrite: false,
        };

        let outcome = ingest.put(instruction.clone()).await;

        assert!(!ingest.supports_put());
        assert!(ingest.supports_update());
        assert!(matches!(outcome, Err(IngestError::PutNotSupported)));
        assert_eq!(ingest.calls(), vec![IngestCall::Put(instruction)]);
    }
}
