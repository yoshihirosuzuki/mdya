# mdya manual

*English / [日本語](../ja/README.md)*

This is the manual to read after you start using mdya. Installation and the first steps are in the repository's [README](../../../README.md).

## Contents

- [Collections and collection types](collections.md): Where mdya ingests documents from, and which items become documents. The differences between directory, archive, and raw.
- [Commands](commands.md): How to write each CLI command, and its output and exit codes.
- [Settings](configuration.md): The names and values of the settings, directories, the embedding model, memory and parallelism, and logs.
- [The MCP server](mcp.md): How to register it with an AI assistant, and the MCP tools.
- [Errors and how to fix them](errors.md): Look up what happened and how to fix it from the exit code and the error message.

When mdya ends with an error, open [Errors and how to fix them](errors.md) and search it for words from the error message.

## Terms

These are the terms used in this manual.

- collection: Something you create and name, and put documents in. Every document in mdya is in one of the collections.
- collection type: Where a collection gets the content of its documents from. There are three: directory, archive, and raw ([Collections and collection types](collections.md)).
- document: The unit for searching and getting. It is not always a file. The file type of a document is Markdown or PDF.
- key: The name that tells documents apart within a collection. In a directory collection, it is the path relative to the directory.
- body: The whole text of a document, taken from the document content. For Markdown, it is the document content itself; for PDF, it is the text extracted from the PDF.
- chunk: One of the pieces a document's body is divided into. Chunks are numbered from 0, in order from the start of the body.
- source: Where document content comes from. It is a directory, an archive file, or document content that you pass in.
- ingest: To read documents from a source and make them searchable.
- update: To read the current contents of a source and make the collection's documents match those contents.
- index: Everything mdya keeps for searching.
- embedding model: A model that represents the meaning of text as lists of numbers that can be used for search (embeddings). Vector search and hybrid search use it.
