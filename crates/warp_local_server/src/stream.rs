use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use axum::response::sse::Event;
use eventsource_stream::Eventsource as _;
use futures::{Stream, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;

use crate::chat::{Turn, new_id};
use crate::{Error, Server, protocol, tools};

const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn response(
    server: Server,
    turn: Turn,
    permit: OwnedSemaphorePermit,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> + Send {
    async_stream::stream! {
        let _permit = permit;
        for initial in &turn.initial_events {
            match protocol::event(initial.clone()) {
                Ok(event) => yield Ok(event),
                Err(error) => { yield Ok(error_event(error)); return; }
            }
        }
        let request = server.client.post(format!("{}/chat/completions", server.config.base_url.trim_end_matches('/')))
            .bearer_auth(&turn.api_key).json(&turn.body).send().await;
        let upstream = match request {
            Ok(upstream) if upstream.status().is_success() => upstream,
            Ok(upstream) => {
                let status = upstream.status();
                let reason = if status.as_u16() == 401 || status.as_u16() == 403 {
                    json!({"invalid_api_key": {"model_name": turn.body["model"]}})
                } else if status.as_u16() == 429 { json!({"quota_limit": {}}) }
                else { json!({"llm_unavailable": {}}) };
                yield Ok(protocol::event(json!({"finished": reason})).expect("Valid finished event"));
                return;
            }
            Err(_) => { yield Ok(error_event(Error::Provider("Cannot connect to the configured model provider"))); return; }
        };
        if !upstream.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.starts_with("text/event-stream")) {
            yield Ok(error_event(Error::Provider("Provider did not return an SSE stream"))); return;
        }
        let mut raw = upstream.bytes_stream();
        let bounded = async_stream::stream! {
            let mut total = 0;
            while let Some(chunk) = raw.next().await {
                match chunk {
                    Ok(bytes) => {
                        total += bytes.len();
                        if total > MAX_OUTPUT_BYTES { yield Err(Error::Provider("Provider response exceeded the size limit")); break; }
                        yield Ok(bytes);
                    }
                    Err(_) => { yield Err(Error::Provider("Provider stream failed or timed out")); break; }
                }
            }
        };
        let events = bounded.eventsource();
        futures::pin_mut!(events);
        let mut state = Output::new(&turn);
        let mut done = false;
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                Err(_) => { yield Ok(error_event(Error::Provider("Provider stream failed or was malformed"))); return; }
            };
            if event.data == "[DONE]" { done = true; break; }
            let chunk = match serde_json::from_str::<Value>(&event.data) {
                Ok(chunk) => chunk,
                Err(_) => { yield Ok(error_event(Error::Provider("Provider returned invalid stream JSON"))); return; }
            };
            match state.push(&chunk) {
                Ok(actions) => for action in actions {
                    match protocol::event(action) {
                        Ok(event) => yield Ok(event),
                        Err(error) => { yield Ok(error_event(error)); return; }
                    }
                },
                Err(error) => { yield Ok(error_event(error)); return; }
            }
        }
        if !done { yield Ok(error_event(Error::Provider("Provider stream ended before the completion marker"))); return; }
        match state.finish(&turn) {
            Ok(final_events) => for event in final_events {
                match protocol::event(event) {
                    Ok(event) => yield Ok(event),
                    Err(error) => { yield Ok(error_event(error)); return; }
                }
            },
            Err(error) => yield Ok(error_event(error)),
        }
    }
}

fn error_event(error: Error) -> Event {
    protocol::event(json!({"finished": {"internal_error": {"message": error.to_string()}}}))
        .expect("Valid error event")
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}

struct Output {
    task_id: String,
    request_id: String,
    text_id: Option<String>,
    reasoning_id: Option<String>,
    emit_reasoning: bool,
    reasoning: String,
    calls: BTreeMap<u64, Call>,
    finish_reason: Option<String>,
    usage: Value,
}

impl Output {
    fn new(turn: &Turn) -> Self {
        Self {
            task_id: turn.task_id.clone(),
            request_id: turn.request_id.clone(),
            text_id: None,
            reasoning_id: None,
            emit_reasoning: turn.emit_reasoning,
            reasoning: String::new(),
            calls: BTreeMap::new(),
            finish_reason: None,
            usage: Value::Null,
        }
    }

    fn push(&mut self, chunk: &Value) -> Result<Vec<Value>, Error> {
        if chunk.get("error").is_some() {
            return Err(Error::Provider("Provider reported a streaming error"));
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = usage.clone();
        }
        let choices = chunk["choices"]
            .as_array()
            .ok_or(Error::Provider("Missing provider stream choices"))?;
        if choices.is_empty() {
            return Ok(Vec::new());
        }
        if choices.len() != 1 || choices[0]["index"].as_u64() != Some(0) {
            return Err(Error::Provider(
                "Only one provider response choice is supported",
            ));
        }
        let choice = &choices[0];
        if self.finish_reason.is_some() {
            return Err(Error::Provider("Provider sent content after finishing"));
        }
        let delta = choice["delta"]
            .as_object()
            .ok_or(Error::Provider("Missing provider stream delta"))?;
        let mut events = Vec::new();
        for (field, kind, text_field) in [
            ("reasoning_content", "agent_reasoning", "reasoning"),
            ("content", "agent_output", "text"),
        ] {
            if let Some(value) = delta.get(field).filter(|value| !value.is_null()) {
                let text = value
                    .as_str()
                    .ok_or(Error::Provider("Provider returned non-text content"))?;
                if field == "reasoning_content" {
                    self.reasoning.push_str(text);
                    if !self.emit_reasoning {
                        continue;
                    }
                }
                if !text.is_empty() {
                    let slot = if field == "content" {
                        &mut self.text_id
                    } else {
                        &mut self.reasoning_id
                    };
                    if let Some(id) = slot {
                        events.push(protocol::append(&self.task_id, id, kind, text_field, text));
                    } else {
                        let id = new_id();
                        events.push(protocol::add_messages(&self.task_id, vec![json!({"id": id, "request_id": self.request_id, kind: {text_field: text}})]));
                        *slot = Some(id);
                    }
                }
            }
        }
        if let Some(calls) = delta.get("tool_calls") {
            for fragment in calls
                .as_array()
                .ok_or(Error::Provider("Invalid provider tool calls"))?
            {
                let index = fragment["index"]
                    .as_u64()
                    .filter(|index| *index < 32)
                    .ok_or(Error::Provider("Invalid provider tool index"))?;
                if fragment["type"]
                    .as_str()
                    .is_some_and(|kind| kind != "function")
                {
                    return Err(Error::Provider("Unsupported provider tool type"));
                }
                let call = self.calls.entry(index).or_default();
                for (value, target) in [
                    (&fragment["id"], &mut call.id),
                    (&fragment["function"]["name"], &mut call.name),
                    (&fragment["function"]["arguments"], &mut call.arguments),
                ] {
                    if !value.is_null() {
                        target.push_str(
                            value
                                .as_str()
                                .ok_or(Error::Provider("Invalid provider tool fragment"))?,
                        );
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
            self.finish_reason = Some(
                reason
                    .as_str()
                    .ok_or(Error::Provider("Invalid provider finish reason"))?
                    .to_string(),
            );
        }
        Ok(events)
    }

    fn finish(self, turn: &Turn) -> Result<Vec<Value>, Error> {
        let reason = self
            .finish_reason
            .as_deref()
            .ok_or(Error::Provider("Provider omitted its finish reason"))?;
        let mut events = Vec::new();
        if !self.emit_reasoning
            && !self.reasoning.is_empty()
            && let Some(id) = &self.text_id
        {
            events.push(protocol::actions(vec![json!({"update_task_message": {
                    "task_id": self.task_id, "message": {"id": id, "server_message_data": json!({"local_reasoning": self.reasoning}).to_string()}, "mask": "serverMessageData"
                }})]));
        }
        let finished = match reason {
            "length" => json!({"max_token_limit": {}}),
            "stop" if self.calls.is_empty() && self.text_id.is_some() => json!({"done": {}}),
            "tool_calls" if self.calls.len() == 1 => {
                let mut ids = HashSet::new();
                let mut messages = Vec::new();
                for call in self.calls.into_values() {
                    if call.id.is_empty() || !ids.insert(call.id.clone()) {
                        return Err(Error::Provider(
                            "Provider returned missing or duplicate tool IDs",
                        ));
                    }
                    let tool =
                        tools::translate(&call.name, &call.arguments, &call.id, &turn.tools)?;
                    let mut saved =
                        json!({"local_function": {"name": call.name, "arguments": call.arguments}});
                    if !self.emit_reasoning
                        && self.text_id.is_none()
                        && messages.is_empty()
                        && !self.reasoning.is_empty()
                    {
                        saved["local_reasoning"] = json!(self.reasoning);
                    }
                    messages.push(json!({
                        "id": new_id(), "request_id": self.request_id, "tool_call": tool,
                        "server_message_data": saved.to_string()
                    }));
                }
                // Validate arguments before emitting an executable tool call.
                events.push(protocol::add_messages(&self.task_id, messages));
                json!({"done": {}})
            }
            _ => {
                return Err(Error::Provider(
                    "Provider returned an unsupported or inconsistent finish reason",
                ));
            }
        };
        let mut finished = finished;
        if let (Some(input), Some(output)) = (
            self.usage["prompt_tokens"].as_u64(),
            self.usage["completion_tokens"].as_u64(),
        ) && input <= u32::MAX as u64
            && output <= u32::MAX as u64
        {
            finished["token_usage"] =
                json!([{"model_id": turn.body["model"], "total_input": input, "output": output}]);
        }
        events.push(json!({"finished": finished}));
        Ok(events)
    }
}

pub(crate) fn http_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(120))
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::Invalid("Cannot initialize provider HTTP client"))
}
