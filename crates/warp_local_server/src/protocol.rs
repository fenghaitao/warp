use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE;
use prost::Message as _;
use prost_reflect::{DynamicMessage, SerializeOptions};
use serde_json::{Value, json};

use crate::Error;

pub(crate) fn decode_request(bytes: &[u8]) -> Result<Value, Error> {
    let descriptor = warp_multi_agent_api::get_descriptor_pool()
        .get_message_by_name("warp.multi_agent.v1.Request")
        .expect("Request descriptor exists");
    let message = DynamicMessage::decode(descriptor, bytes)
        .map_err(|_| Error::Invalid("Invalid Protobuf request"))?;
    to_json(&message)
}

pub(crate) fn to_json(message: &DynamicMessage) -> Result<Value, Error> {
    let mut bytes = Vec::new();
    message
        .serialize_with_options(
            &mut serde_json::Serializer::new(&mut bytes),
            &SerializeOptions::new().use_proto_field_name(true),
        )
        .map_err(|_| Error::Invalid("Cannot serialize Protobuf message"))?;
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("Cannot serialize Protobuf message"))
}

pub(crate) fn encode(name: &str, value: &Value) -> Result<Vec<u8>, Error> {
    let descriptor = warp_multi_agent_api::get_descriptor_pool()
        .get_message_by_name(&format!("warp.multi_agent.v1.{name}"))
        .ok_or(Error::Invalid("Unknown Protobuf message"))?;
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Invalid("Invalid protocol JSON"))?;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let message = DynamicMessage::deserialize(descriptor, &mut deserializer)
        .map_err(|_| Error::Invalid("Invalid protocol fields"))?;
    Ok(message.encode_to_vec())
}

pub(crate) fn event(value: Value) -> Result<axum::response::sse::Event, Error> {
    Ok(axum::response::sse::Event::default()
        .data(BASE64_URL_SAFE.encode(encode("ResponseEvent", &value)?)))
}

pub(crate) fn actions(actions: Vec<Value>) -> Value {
    json!({"client_actions": {"actions": actions}})
}

pub(crate) fn add_messages(task_id: &str, messages: Vec<Value>) -> Value {
    actions(vec![
        json!({"add_messages_to_task": {"task_id": task_id, "messages": messages}}),
    ])
}

pub(crate) fn append(task_id: &str, id: &str, kind: &str, field: &str, text: &str) -> Value {
    actions(vec![json!({"append_to_message_content": {
        "task_id": task_id,
        "message": {"id": id, kind: {field: text}},
        "mask": if kind == "agent_output" { "agentOutput.text" } else { "agentReasoning.reasoning" }
    }})])
}
