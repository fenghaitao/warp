use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Error, protocol};

const TOOLS: &[(&str, &str, &str)] = &[
    (
        "run_shell_command",
        "RUN_SHELL_COMMAND",
        "Run a finite shell command in the client's current directory. Permissions are checked by the client. Avoid pagers and interactive or persistent commands.",
    ),
    (
        "read_files",
        "READ_FILES",
        "Read UTF-8 files on the client's machine. Use absolute paths.",
    ),
    (
        "grep",
        "GREP",
        "Search file contents for the supplied patterns.",
    ),
    (
        "file_glob_v2",
        "FILE_GLOB_V2",
        "Find files by name patterns in a directory.",
    ),
    (
        "apply_file_diffs",
        "APPLY_FILE_DIFFS",
        "Edit files by replacing exact text, creating files, or deleting files. Read files before editing them.",
    ),
];

pub(crate) fn definitions(settings: &Value) -> Vec<Value> {
    let supported = settings["supported_tools"].as_array();
    TOOLS.iter().filter(|(_, kind, _)| {
        supported.is_none_or(|items| items.is_empty() || items.iter().any(|item| item.as_str() == Some(kind)))
    }).map(|(name, _, description)| {
        let parameters = match *name {
            "run_shell_command" => object(json!({"command": {"type": "string"}}), &["command"]),
            "read_files" => object(json!({"files": {"type": "array", "minItems": 1, "items": object(json!({"name": {"type": "string"}}), &["name"])}}), &["files"]),
            "grep" => object(json!({"queries": strings(), "path": {"type": "string"}}), &["queries", "path"]),
            "file_glob_v2" => object(json!({"patterns": strings(), "search_dir": {"type": "string"}}), &["patterns", "search_dir"]),
            "apply_file_diffs" => {
                let mut properties = json!({
                    "summary": {"type": "string"},
                    "diffs": {"type": "array", "items": object(json!({"file_path": {"type": "string"}, "search": {"type": "string"}, "replace": {"type": "string"}}), &["file_path", "search", "replace"])},
                    "deleted_files": {"type": "array", "items": object(json!({"file_path": {"type": "string"}}), &["file_path"])}
                });
                if settings["supports_create_files"].as_bool() == Some(true) {
                    properties["new_files"] = json!({"type": "array", "items": object(json!({"file_path": {"type": "string"}, "content": {"type": "string"}}), &["file_path", "content"])});
                }
                object(properties, &["summary"])
            }
            _ => unreachable!("Tool registry is fixed"),
        };
        json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}})
    }).collect()
}

fn strings() -> Value {
    json!({"type": "array", "minItems": 1, "items": {"type": "string"}})
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    command: String,
}

pub(crate) fn translate(
    name: &str,
    arguments: &str,
    id: &str,
    definitions: &[Value],
) -> Result<Value, Error> {
    let definition = definitions
        .iter()
        .find(|tool| tool["function"]["name"] == name)
        .ok_or(Error::Provider("Provider requested an unsupported tool"))?;
    let mut arguments: Value = serde_json::from_str(arguments)
        .map_err(|_| Error::Provider("Provider returned invalid tool arguments"))?;
    validate(&arguments, &definition["function"]["parameters"])?;
    if name == "run_shell_command" {
        let command: Command = serde_json::from_value(arguments.clone())
            .map_err(|_| Error::Provider("Invalid shell command"))?;
        if command.command.trim().is_empty() {
            return Err(Error::Provider("Provider returned an empty command"));
        }
        // Risk classifications remain conservative; the model cannot grant itself permissions.
        arguments["is_risky"] = json!(true);
        arguments["risk_category"] = json!("RISK_CATEGORY_RISKY");
        arguments["wait_until_complete"] = json!(true);
    }
    if name == "file_glob_v2" {
        arguments["max_matches"] = json!(200);
    }
    let call = json!({"tool_call_id": id, name: arguments});
    protocol::encode("Message.ToolCall", &call)
        .map_err(|_| Error::Provider("Provider returned invalid tool arguments"))?;
    Ok(call)
}

fn validate(value: &Value, schema: &Value) -> Result<(), Error> {
    let invalid = || Error::Provider("Provider returned invalid tool arguments");
    match schema["type"].as_str() {
        Some("object") => {
            let fields = value.as_object().ok_or_else(invalid)?;
            let properties = schema["properties"].as_object().ok_or_else(invalid)?;
            for required in schema["required"].as_array().ok_or_else(invalid)? {
                if !fields.contains_key(required.as_str().ok_or_else(invalid)?) {
                    return Err(invalid());
                }
            }
            for (key, field) in fields {
                validate(field, properties.get(key).ok_or_else(invalid)?)?;
            }
        }
        Some("array") => {
            let items = value.as_array().ok_or_else(invalid)?;
            if items.len() < schema["minItems"].as_u64().unwrap_or(0) as usize {
                return Err(invalid());
            }
            for item in items {
                validate(item, &schema["items"])?;
            }
        }
        Some("string") if value.is_string() => {}
        _ => return Err(invalid()),
    }
    Ok(())
}
