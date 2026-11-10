//! The `collection` table of the LanceDB in the data directory, one row per
//! collection.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::Table;
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::{Array, ArrayRef, RecordBatch, RecordBatchIterator, StringArray};
use lancedb::arrow::arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use lancedb::expr::{DfExpr, col, lit};
use lancedb::query::{ExecutableQuery, Query, QueryBase};
use tokio::sync::OnceCell;

use super::{CollectionError, CollectionInfo};
use crate::types::{BoxError, CollectionName, CollectionType, TypeAttributes};

/// The directory in the data directory that holds the LanceDB. `index` opens
/// the same one.
pub(super) const LANCEDB_DIR: &str = "lancedb";
pub(super) const TABLE: &str = "collection";

const NAME: &str = "name";
const COLLECTION_TYPE: &str = "collection_type";
/// The type attributes, as the text of a JSON object.
const ATTRIBUTES: &str = "attributes";
/// Null when the collection has no description.
const DESCRIPTION: &str = "description";

/// A call into LanceDB, as the hooks see it before it is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LanceCall {
    Connect,
    OpenTable,
    CreateTable,
    ReadRows,
    AddRow,
    ReplaceRow,
    DeleteRow,
}

/// Where the unit tests observe the component's calls into LanceDB and make
/// them fail. The component always goes through it; outside tests,
/// [`RealHooks`] lets every call go ahead.
#[async_trait]
pub(super) trait Hooks: Send + Sync {
    /// Called just before each call of [`LanceCall`]. An error from here is
    /// taken as that call's error.
    async fn before(&self, call: LanceCall) -> Result<(), BoxError>;
}

pub(super) struct RealHooks;

#[async_trait]
impl Hooks for RealHooks {
    async fn before(&self, _call: LanceCall) -> Result<(), BoxError> {
        Ok(())
    }
}

/// Opens the LanceDB in the data directory when a function first needs it, and
/// keeps the connection for the calls after. The table is opened by each call,
/// so that a table another process creates meanwhile is found.
pub(super) struct Store {
    data_dir: PathBuf,
    /// Stays unset when connecting fails or panics, so the next call connects
    /// again.
    connection: OnceCell<lancedb::Connection>,
    hooks: Arc<dyn Hooks>,
}

impl Store {
    pub(super) fn new(data_dir: PathBuf, hooks: Arc<dyn Hooks>) -> Self {
        Self {
            data_dir,
            connection: OnceCell::new(),
            hooks,
        }
    }

    /// The table, for a function that only reads. `None` when there is no
    /// LanceDB directory or no table yet. Nothing is created.
    pub(super) async fn table_to_read(&self) -> Result<Option<Table>, CollectionError> {
        // Connecting would create the directory.
        let exists = tokio::fs::try_exists(self.lancedb_dir()).await;
        if !exists.map_err(|error| read_error(error.into()))? {
            return Ok(None);
        }
        let connection = self.connection().await.map_err(read_error)?;
        self.open_table(connection).await
    }

    /// The table, for a function that writes. Creates the directory and the
    /// table when they are not there yet.
    pub(super) async fn table_to_write(&self) -> Result<Table, CollectionError> {
        let made = tokio::fs::create_dir_all(self.lancedb_dir()).await;
        made.map_err(|error| write_error(error.into()))?;
        let connection = self.connection().await.map_err(write_error)?;
        match self.open_table(connection).await? {
            Some(table) => Ok(table),
            None => self.create_table(connection).await,
        }
    }

    pub(super) async fn read_all(
        &self,
        table: &Table,
    ) -> Result<Vec<CollectionInfo>, CollectionError> {
        self.read_rows(table.query()).await
    }

    pub(super) async fn read_one(
        &self,
        table: &Table,
        name: &CollectionName,
    ) -> Result<Option<CollectionInfo>, CollectionError> {
        let query = table.query().only_if_expr(name_filter(name)).limit(1);
        let collections = self.read_rows(query).await?;
        Ok(collections.into_iter().next())
    }

    pub(super) async fn add(
        &self,
        table: &Table,
        collection: &CollectionInfo,
    ) -> Result<(), CollectionError> {
        self.hooks
            .before(LanceCall::AddRow)
            .await
            .map_err(write_error)?;
        let added = table.add(vec![row_of(collection)]).execute().await;
        added.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Replaces the row of the collection of the same name, in one commit.
    /// Matching on the column takes the name as data: `Table::update` takes
    /// its condition only as SQL text, which must not hold a name.
    pub(super) async fn replace(
        &self,
        table: &Table,
        collection: &CollectionInfo,
    ) -> Result<(), CollectionError> {
        self.hooks
            .before(LanceCall::ReplaceRow)
            .await
            .map_err(write_error)?;
        let row = row_of(collection);
        let schema = row.schema();
        let rows = RecordBatchIterator::new([Ok::<_, ArrowError>(row)], schema);
        let mut merge = table.merge_insert(&[NAME]);
        merge.when_matched_update_all(None);
        let merged = merge.execute(Box::new(rows)).await;
        merged.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    pub(super) async fn delete(
        &self,
        table: &Table,
        name: &CollectionName,
    ) -> Result<(), CollectionError> {
        self.hooks
            .before(LanceCall::DeleteRow)
            .await
            .map_err(write_error)?;
        let deleted = table.delete(&name_filter(name)).await;
        deleted.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    fn lancedb_dir(&self) -> PathBuf {
        self.data_dir.join(LANCEDB_DIR)
    }

    async fn connection(&self) -> Result<&lancedb::Connection, BoxError> {
        self.connection.get_or_try_init(|| self.connect()).await
    }

    async fn connect(&self) -> Result<lancedb::Connection, BoxError> {
        self.hooks.before(LanceCall::Connect).await?;
        // LanceDB takes a path that begins with "db" for a LanceDB server, so
        // it gets an absolute path even if the data directory is relative.
        let lancedb_dir = std::path::absolute(self.lancedb_dir())?;
        let uri = lancedb_dir
            .to_str()
            .ok_or("LanceDB takes only a Unicode path, and the data directory's path is not")?;
        // Reads see what other connections, including other processes, wrote.
        let builder = lancedb::connect(uri).read_consistency_interval(Duration::ZERO);
        Ok(builder.execute().await?)
    }

    /// Opens the table; `None` when it does not exist.
    async fn open_table(
        &self,
        connection: &lancedb::Connection,
    ) -> Result<Option<Table>, CollectionError> {
        self.hooks
            .before(LanceCall::OpenTable)
            .await
            .map_err(read_error)?;
        match connection.open_table(TABLE).execute().await {
            Ok(table) => Ok(Some(table)),
            Err(lancedb::Error::TableNotFound { .. }) => Ok(None),
            Err(error) => Err(read_error(error.into())),
        }
    }

    async fn create_table(
        &self,
        connection: &lancedb::Connection,
    ) -> Result<Table, CollectionError> {
        self.hooks
            .before(LanceCall::CreateTable)
            .await
            .map_err(write_error)?;
        let created = connection.create_empty_table(TABLE, schema());
        created
            .execute()
            .await
            .map_err(|error| write_error(error.into()))
    }

    async fn read_rows(&self, query: Query) -> Result<Vec<CollectionInfo>, CollectionError> {
        self.hooks
            .before(LanceCall::ReadRows)
            .await
            .map_err(read_error)?;
        collections_from(query).await.map_err(read_error)
    }
}

fn read_error(source: BoxError) -> CollectionError {
    CollectionError::CannotRead { source }
}

fn write_error(source: BoxError) -> CollectionError {
    CollectionError::CannotWrite { source }
}

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new(NAME, DataType::Utf8, false),
        Field::new(COLLECTION_TYPE, DataType::Utf8, false),
        Field::new(ATTRIBUTES, DataType::Utf8, false),
        Field::new(DESCRIPTION, DataType::Utf8, true),
    ]))
}

/// The condition that picks a collection's row. The name is a value of the
/// expression, not text of it, because it can hold any string.
fn name_filter(name: &CollectionName) -> DfExpr {
    col(NAME).eq(lit(name.0.clone()))
}

fn row_of(collection: &CollectionInfo) -> RecordBatch {
    #[expect(clippy::expect_used)]
    let attributes = serde_json::to_string(&collection.attributes.0)
        .expect("a JSON object holds only JSON values and string keys, which always serialize");
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(vec![collection.name.0.as_str()])),
        Arc::new(StringArray::from(vec![
            collection.collection_type.0.as_str(),
        ])),
        Arc::new(StringArray::from(vec![attributes.as_str()])),
        Arc::new(StringArray::from(vec![collection.description.as_deref()])),
    ];
    #[expect(clippy::expect_used)]
    RecordBatch::try_new(schema(), columns).expect(
        "the columns are those of the schema, with one row each and no null where none is allowed",
    )
}

async fn collections_from(query: Query) -> Result<Vec<CollectionInfo>, BoxError> {
    let batches: Vec<RecordBatch> = query.execute().await?.try_collect().await?;
    let mut collections = Vec::new();
    for rows in &batches {
        collections.extend(collections_in(rows)?);
    }
    Ok(collections)
}

fn collections_in(rows: &RecordBatch) -> Result<Vec<CollectionInfo>, BoxError> {
    let names = text_column(rows, NAME)?;
    let collection_types = text_column(rows, COLLECTION_TYPE)?;
    let attributes = text_column(rows, ATTRIBUTES)?;
    let descriptions = text_column(rows, DESCRIPTION)?;
    let collection_in = |row: usize| -> Result<CollectionInfo, BoxError> {
        Ok(CollectionInfo {
            name: CollectionName(names.value(row).to_owned()),
            collection_type: CollectionType(collection_types.value(row).to_owned()),
            attributes: TypeAttributes(serde_json::from_str(attributes.value(row))?),
            description: descriptions
                .is_valid(row)
                .then(|| descriptions.value(row).to_owned()),
        })
    };
    (0..rows.num_rows()).map(collection_in).collect()
}

fn text_column<'a>(rows: &'a RecordBatch, name: &str) -> Result<&'a StringArray, BoxError> {
    rows.column_by_name(name)
        .and_then(|column| column.as_string_opt::<i32>())
        .ok_or_else(|| format!("the table has no text column {name}").into())
}
