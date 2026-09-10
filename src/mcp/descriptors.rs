//! What an MCP client is told this server offers: its tools, its resources, and its one prompt.
//!
//! All of them are pure functions of the catalog, so the tool list is rendered once per catalog
//! revision and served from [`Cache`].

use parking_lot::RwLock;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::service::{Catalog, Collection, Operation, Origin};

/// The knowledge base's own source, so a client can read the rules it is calling rather than
/// take the tool descriptions on faith.
pub const SOURCE_RESOURCE_URI: &str = "charon://source";
pub const SOURCE_RESOURCE_MIME_TYPE: &str = "text/x-prolog";
/// Collections are read under this prefix, one resource each.
pub const COLLECTION_RESOURCE_PREFIX: &str = "charon://collection/";
pub const OVERVIEW_PROMPT: &str = "knowledge_base_overview";

/// The URI a collection is read at.
pub fn collection_uri(name: &str) -> String {
    format!("{COLLECTION_RESOURCE_PREFIX}{name}")
}

/// The operations exposed as tools: everything available that came from a documented predicate.
///
/// A collection is deliberately absent, because it is a *resource* — as is the predicate that
/// declares one, which the catalog exposes only as its table. Resources are for context and
/// tools are for acting; making a client spend a tool call to see a table it could simply have
/// been handed gets that backwards.
pub fn tools_of(catalog: &Catalog) -> impl Iterator<Item = &Operation> {
    catalog
        .available()
        .filter(|operation| *operation.origin() == Origin::Predicate)
}

/// The `tools/list` payload.
///
/// A tool name is flat, so it is the predicate's own name — verb prefixes are *not* stripped.
/// Collapsing `get_book` and `put_book` onto one tool the way the HTTP router does would lose
/// the distinction entirely; the prefix becomes behavioural metadata instead, which is what MCP
/// clients actually want from it.
pub fn tools(catalog: &Catalog) -> Value {
    json!({ "tools": tools_of(catalog).map(tool).collect::<Vec<_>>() })
}

fn tool(operation: &Operation) -> Value {
    let mut description = operation.summary();
    if let Some(rest) = operation.description() {
        description.push_str("\n\n");
        description.push_str(&rest);
    }

    let verb = operation.verb();
    let mut annotations = json!({
        // A short human name rather than the summary: a client renders this in a list, where a
        // whole sentence reads as noise.
        "title": operation.title(),
        "readOnlyHint": verb.is_read_only(),
        "idempotentHint": verb.is_idempotent(),
        "destructiveHint": verb.is_destructive(),
        // Every answer comes from the local knowledge base; nothing reaches the network.
        "openWorldHint": false,
    });
    if operation.is_deprecated() {
        annotations["deprecated"] = json!(true);
    }

    json!({
        "name": operation.name(),
        "description": description,
        "inputSchema": operation.input_schema(),
        "outputSchema": {
            "type": "object",
            "properties": {
                "solutions": operation.response_schema(),
                "truncated": {
                    "type": "boolean",
                    "description": "Whether the solution cap cut this answer short.",
                },
            },
            "required": ["solutions"],
            "additionalProperties": false,
        },
        "annotations": annotations,
    })
}

/// Holds the tool list for the catalog revision it was rendered from. A reload moves the
/// revision on, which is what invalidates it.
#[derive(Default)]
pub struct Cache(RwLock<Option<(u64, Arc<Value>)>>);

impl Cache {
    pub fn tools(&self, catalog: &Catalog) -> Arc<Value> {
        let revision = catalog.revision();
        if let Some((cached, rendered)) = self.0.read().as_ref()
            && *cached == revision
        {
            return Arc::clone(rendered);
        }

        let rendered = Arc::new(tools(catalog));
        *self.0.write() = Some((revision, Arc::clone(&rendered)));
        rendered
    }
}

/// The `resources/list` payload: the source text, plus one resource per declared collection.
pub fn resources(catalog: &Catalog) -> Value {
    let mut resources = vec![json!({
        "uri": SOURCE_RESOURCE_URI,
        "name": "Knowledge base source",
        "description": "The Prolog source text this server was started with.",
        "mimeType": SOURCE_RESOURCE_MIME_TYPE,
    })];

    for collection in catalog.collections() {
        resources.push(json!({
            "uri": collection_uri(collection.name()),
            "name": collection.name(),
            "description": format!("{} Every member, as a JSON array.", collection.summary()),
            "mimeType": "application/json",
        }));
    }

    json!({ "resources": resources })
}

/// The `resources/templates/list` payload: one template per collection, so a client can read a
/// single member instead of pulling the whole table into its context.
pub fn resource_templates(catalog: &Catalog) -> Value {
    let templates: Vec<Value> = catalog
        .collections()
        .iter()
        .map(|collection| {
            json!({
                "uriTemplate": format!(
                    "{}/{}",
                    collection_uri(collection.name()),
                    key_template(collection),
                ),
                "name": format!("{} member", collection.name()),
                "description": format!(
                    "One member of `{}`, identified by {}.",
                    collection.name(),
                    join(collection.key().map(|c| format!("`{}`", c.name)), " and "),
                ),
                "mimeType": "application/json",
            })
        })
        .collect();

    json!({ "resourceTemplates": templates })
}

/// The `prompts/list` payload.
///
/// One prompt, built from the module documentation and the operation set, because the hardest
/// thing about driving a knowledge base you have not read is knowing what it is *for*.
pub fn prompts() -> Value {
    json!({
        "prompts": [{
            "name": OVERVIEW_PROMPT,
            "title": "How to use this knowledge base",
            "description": "An orientation: what this knowledge base is about, what it can be \
                            asked, and what its collections hold.",
            "arguments": [],
        }]
    })
}

/// The overview prompt's message.
pub fn overview(catalog: &Catalog) -> String {
    let mut text = format!("# {}\n\n{}\n", catalog.title(), catalog.description());

    let tools: Vec<&Operation> = tools_of(catalog).collect();
    if !tools.is_empty() {
        text.push_str("\n## Tools\n\n");
        for operation in tools {
            text.push_str(&format!(
                "- `{}` — {}\n",
                operation.name(),
                operation.summary()
            ));
        }
    }

    let collections = catalog.collections();
    if !collections.is_empty() {
        text.push_str(
            "\n## Collections\n\nRead these as resources rather than calling a tool for them.\n\n",
        );
        for collection in collections {
            let uri = collection_uri(collection.name());
            text.push_str(&format!(
                "- `{}` — {} Read `{uri}`, or one member at `{uri}/{}`.\n",
                collection.name(),
                collection.summary(),
                key_template(collection),
            ));
        }
    }

    text.push_str(&format!(
        "\nThe rules themselves are readable at `{SOURCE_RESOURCE_URI}`.\n"
    ));
    text
}

/// A collection's key as a URI template fragment: `{Author}/{Year}`.
fn key_template(collection: &Collection) -> String {
    join(
        collection
            .key()
            .map(|column| format!("{{{}}}", column.name)),
        "/",
    )
}

fn join(parts: impl Iterator<Item = String>, separator: &str) -> String {
    parts.collect::<Vec<_>>().join(separator)
}
