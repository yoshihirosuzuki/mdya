//! The `raw_ingest` component: ingests a `raw` collection, whose documents the
//! user passes in directly.

use std::panic::resume_unwind;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::StreamExt;
use tokio::task::JoinError;

use crate::format::{Extracted, FileType, Format};
use crate::index::{DocumentToRegister, Index, RegistrationSink};
use crate::ingest::{
    Ingest, IngestError, PutInstruction, UpdateInstruction, UpdateReport, UpdateStopped,
};
use crate::types::{CollectionName, DirLookup, DocumentOutcome, Key, ProgressSink, TypeAttributes};

#[cfg(test)]
mod tests;

/// Creates the component.
pub fn create(format: Arc<dyn Format>, index: Arc<dyn Index>) -> Arc<dyn Ingest> {
    Arc::new(RawIngest { format, index })
}

/// Holds nothing that changes between calls, so that calls at the same time,
/// and a call that panicked, do not affect the others.
struct RawIngest {
    format: Arc<dyn Format>,
    index: Arc<dyn Index>,
}

/// One call ingests one document, so `index` makes the embedding vectors of
/// one document at a time.
const PARALLELISM: usize = 1;

impl RawIngest {
    fn file_type_of(&self, media_type: &str) -> Result<FileType, IngestError> {
        self.format
            .file_type_from_mime_or_alias(media_type)
            .ok_or(IngestError::UnsupportedFileType)
    }

    /// Extracts on a blocking thread and waits for it, so that a long
    /// extraction does not hold up the other async work, and the thread's
    /// panic reaches the caller as a panic.
    async fn extract(
        &self,
        content: Vec<u8>,
        file_type: FileType,
    ) -> Result<Extracted, IngestError> {
        let format = Arc::clone(&self.format);
        let task = tokio::task::spawn_blocking(move || format.extract(&content, &file_type));
        match task.await {
            Ok(extracted) => extracted.map_err(IngestError::CannotExtract),
            Err(error) => resume_panic(error),
        }
    }

    /// Registers the document alone, and returns the outcome `index` passes
    /// for it.
    async fn register(&self, document: DocumentToRegister) -> Result<DocumentOutcome, IngestError> {
        let outcome = OneOutcome::default();
        let documents = futures::stream::iter([document]).boxed();
        self.index
            .register_stream(documents, PARALLELISM, &outcome, None)
            .await
            .map_err(IngestError::Index)?;
        Ok(outcome.into_passed())
    }
}

#[async_trait]
impl Ingest for RawIngest {
    fn supports_update(&self) -> bool {
        false
    }

    fn supports_put(&self) -> bool {
        true
    }

    /// A `raw` collection has no location, so `dirs` is not used.
    async fn check_attributes(
        &self,
        attributes: TypeAttributes,
        _dirs: &DirLookup,
    ) -> Result<TypeAttributes, IngestError> {
        check_attribute_names(&attributes)?;
        Ok(attributes)
    }

    async fn update(
        &self,
        _instruction: UpdateInstruction,
        _progress: &dyn ProgressSink,
    ) -> Result<UpdateReport, UpdateStopped> {
        Err(UpdateStopped {
            counts: UpdateReport::default(),
            error: IngestError::UpdateNotSupported,
        })
    }

    /// The document has no time. The attributes are not used: a `raw`
    /// collection has none.
    async fn put(&self, instruction: PutInstruction) -> Result<DocumentOutcome, IngestError> {
        let PutInstruction {
            collection,
            attributes: _,
            key,
            media_type,
            content,
            overwrite,
        } = instruction;
        let file_type = self.file_type_of(&media_type)?;
        let Extracted { body, chunks } = self.extract(content, file_type).await?;
        let document = DocumentToRegister {
            collection,
            key,
            time: None,
            body,
            chunks,
            overwrite,
        };
        self.register(document).await
    }
}

/// Keeps the outcome `index` passes for the one document of the stream.
#[derive(Default)]
struct OneOutcome(OnceLock<DocumentOutcome>);

impl OneOutcome {
    #[expect(
        clippy::expect_used,
        reason = "`index` passes the outcome of each document it has written before it returns success"
    )]
    fn into_passed(self) -> DocumentOutcome {
        self.0.into_inner().expect(
            "the index passed no outcome for the document although it returned success, which it does only once it has passed the outcome of each document it has written",
        )
    }
}

impl RegistrationSink for OneOutcome {
    fn registered(&self, _collection: &CollectionName, _key: &Key, outcome: DocumentOutcome) {
        // The stream has one document, so `index` passes one outcome.
        let _ = self.0.set(outcome);
    }
}

#[expect(
    clippy::panic,
    reason = "a blocking task that is awaited ends without a panic only when the runtime shuts down, which the ingest cannot carry on from"
)]
fn resume_panic(error: JoinError) -> ! {
    match error.try_into_panic() {
        Ok(panic) => resume_unwind(panic),
        Err(error) => panic!(
            "the extraction was cancelled, which happens only when the runtime shuts down: {error}"
        ),
    }
}

/// Checks the names of the type-specific attributes: a `raw` collection has
/// none, so any attribute is an extra one. Callable without creating the
/// component.
pub fn check_attribute_names(attributes: &TypeAttributes) -> Result<(), IngestError> {
    if attributes.0.is_empty() {
        return Ok(());
    }
    Err(IngestError::AttributesMissingOrExtra)
}
