//! The tools: their definitions, and how their inputs are read. Each tool's
//! inputs are listed once, and both the input schema and the reading of the
//! inputs are made from that list, so that the two cannot disagree.

use std::sync::Arc;

use rmcp::model::JsonObject;
use serde_json::{Value, json};

use crate::query::{ChunkRangeRequest, SearchMode, SearchUnit};
use crate::types::{CollectionName, Key};

use super::super::operations::{
    CollectionList, DocumentText, GetDocumentInput, SearchInput, SearchLimit, SearchOutput,
    VersionOutput,
};
use super::McpError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    ListCollections,
    Search,
    GetDocument,
    GetVersion,
}

impl Tool {
    const ALL: [Tool; 4] = [
        Tool::ListCollections,
        Tool::Search,
        Tool::GetDocument,
        Tool::GetVersion,
    ];

    pub(super) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Tool::ListCollections => "list_collections",
            Tool::Search => "search",
            Tool::GetDocument => "get_document",
            Tool::GetVersion => "get_version",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Tool::ListCollections => {
                "Lists the collections in mdya: for each, its name, type, description, \
                 number of documents, and type-specific attributes. Use it to see what the \
                 user's documents in mdya are about, and to find the collection names to \
                 pass to search."
            }
            Tool::Search => {
                "Searches the documents in mdya for the query and returns the hits, \
                 highest score first. Each hit has the collection, the key of the document, \
                 the score, a snippet, the index of the chunk the snippet comes from \
                 (chunk_index), and the number of chunks in the document (chunk_count). Use \
                 it to find the user's documents about something, then pass a hit's \
                 collection, key, and chunk_index to get_document to read the text."
            }
            Tool::GetDocument => {
                "Gets the text of a document in mdya: the whole text, or the text that a \
                 range of chunks covers. Use it to read a document that search found. When \
                 the text is larger than the output size limit, it returns an error instead: \
                 get the text in parts then, giving ranges of chunks."
            }
            Tool::GetVersion => {
                "Returns the version of mdya that runs this MCP server, such as mdya 1.2.3, \
                 or only mdya when mdya is set not to show its version."
            }
        }
    }

    fn inputs(self) -> &'static [Input] {
        match self {
            Tool::ListCollections | Tool::GetVersion => &[],
            Tool::Search => &SEARCH_INPUTS,
            Tool::GetDocument => &GET_DOCUMENT_INPUTS,
        }
    }

    fn definition(self) -> rmcp::model::Tool {
        let tool = rmcp::model::Tool::new(self.name(), self.description(), input_schema(self));
        match self {
            Tool::ListCollections => tool.with_output_schema::<CollectionList>(),
            Tool::Search => tool.with_output_schema::<SearchOutput>(),
            Tool::GetDocument => tool.with_output_schema::<DocumentText>(),
            Tool::GetVersion => tool.with_output_schema::<VersionOutput>(),
        }
    }
}

/// The definitions of all the tools, for the client's list of tools.
pub(super) fn definitions() -> Vec<rmcp::model::Tool> {
    Tool::ALL.into_iter().map(Tool::definition).collect()
}

/// An input of a tool.
struct Input {
    name: &'static str,
    kind: Kind,
    required: bool,
    description: &'static str,
}

/// What an input holds.
enum Kind {
    Text,
    Texts,
    /// One of the given words.
    Choice(&'static [&'static str]),
    /// A whole number from `minimum` to `maximum`. Any from 0 is read; a
    /// number below `minimum` is left for the tool's reading of its inputs to
    /// report, so that it is reported as the abnormal case it is (a limit of
    /// 0).
    WholeNumber {
        minimum: u64,
        maximum: u64,
    },
    /// The number of a chunk: a whole number of 0 or more, with no upper
    /// bound. One larger than a `usize` holds is read as `usize::MAX`, which
    /// is past the last chunk all the same.
    ChunkIndex,
}

const MODES: [&str; 3] = ["fts", "vector", "hybrid"];
const UNITS: [&str; 2] = ["document", "chunk"];

const SEARCH_INPUTS: [Input; 5] = [
    Input {
        name: "query",
        kind: Kind::Text,
        required: true,
        description: "What to search for. Not empty, and not only white space.",
    },
    Input {
        name: "mode",
        kind: Kind::Choice(&MODES),
        required: false,
        description: "How to search: fts (full-text search, for the words of the query), \
                      vector (vector search, for text close in meaning to the query), or \
                      hybrid (both, with the results combined by rank). Omitted, hybrid.",
    },
    Input {
        name: "collections",
        kind: Kind::Texts,
        required: false,
        description: "The names of the collections to search. Omitted, all the collections.",
    },
    Input {
        name: "limit",
        kind: Kind::WholeNumber {
            minimum: 1,
            maximum: SearchLimit::MAX,
        },
        required: false,
        description: "The most hits to return, 1 or more. Omitted, 20.",
    },
    Input {
        name: "unit",
        kind: Kind::Choice(&UNITS),
        required: false,
        description: "document to return each matching document once, or chunk to return \
                      each matching chunk as a hit of its own. Omitted, document.",
    },
];

const GET_DOCUMENT_INPUTS: [Input; 4] = [
    Input {
        name: "collection",
        kind: Kind::Text,
        required: true,
        description: "The name of the collection the document is in.",
    },
    Input {
        name: "key",
        kind: Kind::Text,
        required: true,
        description: "The key of the document, as search returns it.",
    },
    Input {
        name: "chunk_index",
        kind: Kind::ChunkIndex,
        required: false,
        description: "The index of the first chunk to get, such as a search hit's \
                      chunk_index. Omitted, the whole text of the document.",
    },
    Input {
        name: "end_chunk_index",
        kind: Kind::ChunkIndex,
        required: false,
        description: "The index of the last chunk to get, not smaller than chunk_index, \
                      which it needs. Omitted, only the chunk at chunk_index. Past the last \
                      chunk, the text up to the end of the document.",
    },
];

/// The input schema of the tool: an object with the tool's inputs and no
/// others.
fn input_schema(tool: Tool) -> Arc<JsonObject> {
    let properties: JsonObject = tool
        .inputs()
        .iter()
        .map(|input| (input.name.to_owned(), input_property(input)))
        .collect();
    let required: Vec<&str> = tool
        .inputs()
        .iter()
        .filter(|input| input.required)
        .map(|input| input.name)
        .collect();
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_owned(), json!(required));
    }
    schema.insert("additionalProperties".to_owned(), json!(false));
    Arc::new(schema)
}

fn input_property(input: &Input) -> Value {
    let mut property = match input.kind {
        Kind::Text => json!({ "type": "string" }),
        Kind::Texts => json!({ "type": "array", "items": { "type": "string" } }),
        Kind::Choice(words) => json!({ "type": "string", "enum": words }),
        Kind::WholeNumber { minimum, maximum } => {
            json!({ "type": "integer", "minimum": minimum, "maximum": maximum })
        }
        Kind::ChunkIndex => json!({ "type": "integer", "minimum": 0 }),
    };
    if let Value::Object(property) = &mut property {
        property.insert("description".to_owned(), json!(input.description));
    }
    property
}

/// A value read from the tool's input.
enum Read {
    Text(String),
    Texts(Vec<String>),
    WholeNumber(u64),
    ChunkIndex(usize),
}

/// A whole number of 0 or more in the JSON of an input: one that a `u64`
/// holds, or a larger one. JSON reading gives a number of 2^64 or more as a
/// float, with digits lost, so only that it is larger is kept.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WholeNumber {
    U64(u64),
    BeyondU64,
}

impl WholeNumber {
    fn to_usize(self) -> usize {
        match self {
            WholeNumber::U64(number) => usize::try_from(number).unwrap_or(usize::MAX),
            WholeNumber::BeyondU64 => usize::MAX,
        }
    }
}

/// The inputs as read, by name.
struct ReadInputs(Vec<(&'static str, Read)>);

impl ReadInputs {
    fn take(&mut self, name: &str) -> Option<Read> {
        let position = self.0.iter().position(|(read, _)| *read == name)?;
        Some(self.0.swap_remove(position).1)
    }

    fn text(&mut self, name: &str) -> Option<String> {
        match self.take(name)? {
            Read::Text(text) => Some(text),
            _ => None,
        }
    }

    fn texts(&mut self, name: &str) -> Option<Vec<String>> {
        match self.take(name)? {
            Read::Texts(texts) => Some(texts),
            _ => None,
        }
    }

    fn whole_number(&mut self, name: &str) -> Option<u64> {
        match self.take(name)? {
            Read::WholeNumber(number) => Some(number),
            _ => None,
        }
    }

    fn chunk_index(&mut self, name: &str) -> Option<usize> {
        match self.take(name)? {
            Read::ChunkIndex(index) => Some(index),
            _ => None,
        }
    }
}

/// Reads the inputs of a tool that has none: only an empty input fits.
pub(super) fn no_input(arguments: &JsonObject) -> Result<(), McpError> {
    read(&[], arguments).map(|_| ())
}

/// Reads the search tool's inputs. A `limit` of 0 is turned away here, as a
/// search limit cannot be 0.
pub(super) fn search_input(arguments: &JsonObject) -> Result<SearchInput, McpError> {
    let mut inputs = read(&SEARCH_INPUTS, arguments)?;
    let limit = match inputs.whole_number("limit").unwrap_or(20) {
        0 => return Err(McpError::ZeroLimit),
        limit => SearchLimit::new(limit),
    };
    Ok(SearchInput {
        query: inputs.text("query").unwrap_or_default(),
        mode: match inputs.text("mode").as_deref() {
            Some("fts") => SearchMode::FullText,
            Some("vector") => SearchMode::Vector,
            _ => SearchMode::Hybrid,
        },
        collections: inputs
            .texts("collections")
            .unwrap_or_default()
            .into_iter()
            .map(CollectionName)
            .collect(),
        limit,
        unit: match inputs.text("unit").as_deref() {
            Some("chunk") => SearchUnit::Chunk,
            _ => SearchUnit::Document,
        },
    })
}

/// `size_limit` is the MCP output size limit, which the input of the
/// operation carries.
pub(super) fn get_document_input(
    arguments: &JsonObject,
    size_limit: Option<u64>,
) -> Result<GetDocumentInput, McpError> {
    let mut inputs = read(&GET_DOCUMENT_INPUTS, arguments)?;
    Ok(GetDocumentInput {
        collection: CollectionName(inputs.text("collection").unwrap_or_default()),
        key: Key(inputs.text("key").unwrap_or_default()),
        range: ChunkRangeRequest {
            start: inputs.chunk_index("chunk_index"),
            end: inputs.chunk_index("end_chunk_index"),
        },
        size_limit,
    })
}

/// Checks the arguments against the tool's inputs and reads them: first that
/// every argument is one of the inputs, then each input in the order of the
/// list. The first argument that does not fit is the error.
fn read(inputs: &'static [Input], arguments: &JsonObject) -> Result<ReadInputs, McpError> {
    if let Some(unknown) = arguments
        .keys()
        .find(|name| !inputs.iter().any(|input| input.name == name.as_str()))
    {
        return Err(invalid(unknown, "the tool has no input of this name"));
    }
    let mut read_inputs = Vec::new();
    for input in inputs {
        match arguments.get(input.name) {
            None if input.required => return Err(invalid(input.name, "it is missing")),
            None => {}
            Some(value) => read_inputs.push((input.name, read_value(input, value)?)),
        }
    }
    Ok(ReadInputs(read_inputs))
}

fn read_value(input: &Input, value: &Value) -> Result<Read, McpError> {
    match input.kind {
        Kind::Text => match value {
            Value::String(text) => Ok(Read::Text(text.clone())),
            _ => Err(invalid(input.name, "it is not a string")),
        },
        Kind::Texts => texts(value)
            .map(Read::Texts)
            .ok_or_else(|| invalid(input.name, "it is not an array of strings")),
        Kind::Choice(words) => match value {
            Value::String(word) if words.contains(&word.as_str()) => Ok(Read::Text(word.clone())),
            _ => Err(invalid(
                input.name,
                &format!("it is not one of {}", words.join(", ")),
            )),
        },
        Kind::WholeNumber { minimum, maximum } => match whole_number(value) {
            Some(WholeNumber::U64(number)) if number <= maximum => Ok(Read::WholeNumber(number)),
            _ => Err(invalid(
                input.name,
                &format!("it is not a whole number from {minimum} to {maximum}"),
            )),
        },
        Kind::ChunkIndex => whole_number(value)
            .map(|index| Read::ChunkIndex(index.to_usize()))
            .ok_or_else(|| invalid(input.name, "it is not a whole number of 0 or more")),
    }
}

fn texts(value: &Value) -> Option<Vec<String>> {
    let Value::Array(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// A JSON number with no fractional part, 0 or more, such as `20`, `20.0`, or
/// `2e1`: JSON Schema counts each of them as an integer.
fn whole_number(value: &Value) -> Option<WholeNumber> {
    let Value::Number(number) = value else {
        return None;
    };
    if let Some(number) = number.as_u64() {
        return Some(WholeNumber::U64(number));
    }
    let number = number.as_f64()?;
    if number.fract() != 0.0 || number < 0.0 {
        return None;
    }
    if number >= BEYOND_U64 {
        return Some(WholeNumber::BeyondU64);
    }
    // A whole number of 0 or more and below 2^64 fits in a u64 exactly.
    Some(WholeNumber::U64(number as u64))
}

/// 2^64, the first whole number that a `u64` cannot hold. A float holds it
/// exactly.
const BEYOND_U64: f64 = 18_446_744_073_709_551_616.0;

fn invalid(input: &str, detail: &str) -> McpError {
    McpError::InvalidToolInput {
        input: input.to_owned(),
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(value: Value) -> JsonObject {
        match value {
            Value::Object(arguments) => arguments,
            _ => panic!("the arguments are an object"),
        }
    }

    /// The input and the detail of the error, or `None` when it was read.
    fn search_error(value: Value) -> Option<(String, String)> {
        match search_input(&arguments(value)) {
            Ok(_) => None,
            Err(McpError::InvalidToolInput { input, detail }) => Some((input, detail)),
            Err(error) => panic!("not an input error: {error}"),
        }
    }

    fn error(input: &str, detail: &str) -> Option<(String, String)> {
        Some((input.to_owned(), detail.to_owned()))
    }

    fn limit(value: Value) -> SearchLimit {
        search_input(&arguments(json!({ "query": "q", "limit": value })))
            .unwrap()
            .limit
    }

    #[test]
    fn each_tool_is_found_by_its_name() {
        for tool in Tool::ALL {
            assert_eq!(Tool::named(tool.name()), Some(tool));
        }
        assert_eq!(Tool::named("delete_everything"), None);
    }

    #[test]
    fn the_input_schema_lists_the_inputs_the_required_ones_and_no_others() {
        let schema = input_schema(Tool::GetDocument);

        let properties: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(
            properties,
            ["collection", "key", "chunk_index", "end_chunk_index"]
        );
        assert_eq!(schema["required"], json!(["collection", "key"]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn a_tool_without_inputs_has_an_empty_object_as_its_input_schema() {
        let schema = input_schema(Tool::ListCollections);

        assert_eq!(schema["properties"], json!({}));
        assert!(schema.get("required").is_none());
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn each_kind_of_input_has_its_schema_and_its_description() {
        let schema = input_schema(Tool::Search);
        let properties = &schema["properties"];

        assert_eq!(properties["query"]["type"], json!("string"));
        assert_eq!(
            properties["mode"]["enum"],
            json!(["fts", "vector", "hybrid"])
        );
        assert_eq!(
            properties["collections"]["items"],
            json!({ "type": "string" })
        );
        assert_eq!(properties["limit"]["type"], json!("integer"));
        assert_eq!(properties["limit"]["minimum"], json!(1));
        assert_eq!(
            properties["limit"]["maximum"],
            json!(9_007_199_254_740_991_u64)
        );
        for input in SEARCH_INPUTS {
            assert_eq!(
                properties[input.name]["description"],
                json!(input.description)
            );
        }
    }

    #[test]
    fn every_tool_has_an_output_schema_of_an_object() {
        for tool in definitions() {
            let schema = tool.output_schema.unwrap();
            assert_eq!(schema["type"], json!("object"), "{}", tool.name);
        }
    }

    #[test]
    fn an_input_the_tool_does_not_have_is_refused_before_the_others_are_checked() {
        assert_eq!(
            search_error(json!({ "querry": "q" })),
            error("querry", "the tool has no input of this name")
        );
    }

    #[test]
    fn a_tool_without_inputs_refuses_any_input() {
        assert!(no_input(&JsonObject::new()).is_ok());
        assert!(matches!(
            no_input(&arguments(json!({ "x": 1 }))),
            Err(McpError::InvalidToolInput { input, .. }) if input == "x"
        ));
    }

    #[test]
    fn a_missing_required_input_is_refused() {
        assert_eq!(search_error(json!({})), error("query", "it is missing"));
    }

    #[test]
    fn a_text_input_must_be_a_string() {
        assert_eq!(
            search_error(json!({ "query": null })),
            error("query", "it is not a string")
        );
    }

    #[test]
    fn a_choice_must_be_one_of_its_words() {
        assert_eq!(
            search_error(json!({ "query": "q", "mode": "semantic" })),
            error("mode", "it is not one of fts, vector, hybrid")
        );
        assert_eq!(
            search_error(json!({ "query": "q", "unit": 1 })),
            error("unit", "it is not one of document, chunk")
        );
    }

    #[test]
    fn the_collections_must_be_an_array_of_strings() {
        assert_eq!(
            search_error(json!({ "query": "q", "collections": "notes" })),
            error("collections", "it is not an array of strings")
        );
        assert_eq!(
            search_error(json!({ "query": "q", "collections": ["notes", 1] })),
            error("collections", "it is not an array of strings")
        );
    }

    #[test]
    fn the_modes_are_read_as_the_search_modes() {
        let mode = |word: &str| {
            search_input(&arguments(json!({ "query": "q", "mode": word })))
                .unwrap()
                .mode
        };

        assert_eq!(mode("fts"), SearchMode::FullText);
        assert_eq!(mode("vector"), SearchMode::Vector);
        assert_eq!(mode("hybrid"), SearchMode::Hybrid);
    }

    #[test]
    fn a_whole_number_may_be_written_with_a_fraction_of_zero_or_an_exponent() {
        assert_eq!(limit(json!(20.0)), SearchLimit::new(20));
        assert_eq!(limit(json!(2e1)), SearchLimit::new(20));
    }

    #[test]
    fn a_limit_of_zero_is_turned_away_as_a_limit_of_zero() {
        for value in [json!(0), json!(0.0)] {
            assert!(
                matches!(
                    search_input(&arguments(json!({ "query": "q", "limit": value }))),
                    Err(McpError::ZeroLimit)
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn a_limit_left_out_is_20() {
        assert_eq!(
            search_input(&arguments(json!({ "query": "q" })))
                .unwrap()
                .limit,
            SearchLimit::new(20)
        );
    }

    #[test]
    fn a_limit_of_9007199254740991_is_read_as_it_is() {
        assert_eq!(
            limit(json!(9_007_199_254_740_991_u64)),
            SearchLimit::new(9_007_199_254_740_991)
        );
    }

    // JSON reading gives a number of 2^64 or more as a float.
    #[test]
    fn a_limit_beyond_9007199254740991_is_refused() {
        let two_to_the_64: Value = serde_json::from_str("18446744073709551616").unwrap();
        for value in [
            json!(9_007_199_254_740_992_u64),
            json!(u64::MAX),
            two_to_the_64,
            json!(1e20),
        ] {
            assert_eq!(
                search_error(json!({ "query": "q", "limit": value })),
                error(
                    "limit",
                    "it is not a whole number from 1 to 9007199254740991"
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn a_whole_number_input_refuses_fractions_negatives_and_other_values() {
        for value in [json!(1.5), json!(-1), json!(-1.0), json!("20"), json!(null)] {
            assert_eq!(
                search_error(json!({ "query": "q", "limit": value })),
                error(
                    "limit",
                    "it is not a whole number from 1 to 9007199254740991"
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn the_chunk_indices_are_whole_numbers_of_0_or_more_without_an_upper_bound() {
        let schema = input_schema(Tool::GetDocument);

        for name in ["chunk_index", "end_chunk_index"] {
            let property = &schema["properties"][name];
            assert_eq!(property["type"], json!("integer"), "{name}");
            assert_eq!(property["minimum"], json!(0), "{name}");
            assert!(property.get("maximum").is_none(), "{name}");
        }
    }

    #[test]
    fn the_chunk_indices_are_read_with_the_mcp_output_size_limit() {
        let input = get_document_input(
            &arguments(json!({ "collection": "c", "key": "k", "end_chunk_index": 3 })),
            None,
        )
        .unwrap();

        assert_eq!(
            input.range,
            ChunkRangeRequest {
                start: None,
                end: Some(3)
            }
        );
        assert_eq!(input.size_limit, None);
    }

    #[test]
    fn a_chunk_index_beyond_usize_is_read_as_the_largest_usize() {
        let input = get_document_input(
            &arguments(json!({ "collection": "c", "key": "k", "chunk_index": 1e20 })),
            Some(1),
        )
        .unwrap();

        assert_eq!(input.range.start, Some(usize::MAX));
    }

    #[test]
    fn a_chunk_index_must_be_a_whole_number_of_0_or_more() {
        assert!(matches!(
            get_document_input(
                &arguments(json!({ "collection": "c", "key": "k", "chunk_index": -1 })),
                None,
            ),
            Err(McpError::InvalidToolInput { input, detail })
                if input == "chunk_index" && detail == "it is not a whole number of 0 or more"
        ));
    }
}
