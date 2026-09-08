//! `GET /api/stream?since=<seq>`: server-sent events with replay, resync,
//! and per-client coalescing.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;
use crate::stream::StreamEvent;

const COALESCE_WINDOW: Duration = Duration::from_millis(50);

#[derive(Deserialize, utoipa::IntoParams)]
pub struct StreamQuery {
    /// Last sequence number the client has seen.
    pub since: Option<u64>,
}

fn to_sse(e: &StreamEvent) -> Event {
    Event::default()
        .id(e.seq.to_string())
        .event(e.kind)
        .data(e.data.to_string())
}

#[utoipa::path(get, path = "/api/stream", params(StreamQuery), responses((status = 200, description = "text/event-stream of run.updated, task_run.updated, log.appended, flow.registered, resync")))]
pub async fn stream(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.stream.subscribe();
    let latest = state.stream.latest_seq();
    let since = q.since.unwrap_or(latest);
    let backlog: Vec<Event> = match state.stream.replay(since) {
        Some(events) => events.iter().map(|e| to_sse(e)).collect(),
        None => vec![Event::default()
            .id(latest.to_string())
            .event("resync")
            .data(serde_json::json!({"latest": latest}).to_string())],
    };
    let hello = Event::default()
        .event("hello")
        .data(serde_json::json!({"latest": latest, "since": since}).to_string());
    let mut shutdown = state.shutdown.clone();

    let live = async_stream(
        move |yielder: tokio::sync::mpsc::Sender<Event>| async move {
            let _ = yielder.send(hello).await;
            for e in backlog {
                if yielder.send(e).await.is_err() {
                    return;
                }
            }
            loop {
                let received = tokio::select! {
                    r = rx.recv() => r,
                    _ = shutdown.changed() => return,
                };
                let first = match received {
                    Ok(e) => e,
                    Err(RecvError::Lagged(_)) => {
                        let latest = state.stream.latest_seq();
                        let _ = yielder
                            .send(
                                Event::default()
                                    .id(latest.to_string())
                                    .event("resync")
                                    .data(serde_json::json!({"latest": latest}).to_string()),
                            )
                            .await;
                        continue;
                    }
                    Err(RecvError::Closed) => return,
                };
                // Coalesce: keep the latest message per (kind, key) within the window.
                let mut batch: Vec<Arc<StreamEvent>> = vec![first];
                let deadline = tokio::time::Instant::now() + COALESCE_WINDOW;
                loop {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Ok(e)) => batch.push(e),
                        Ok(Err(RecvError::Lagged(_))) => continue,
                        Ok(Err(RecvError::Closed)) => break,
                        Err(_) => break,
                    }
                }
                let mut latest_by_key: HashMap<(&'static str, String), Arc<StreamEvent>> =
                    HashMap::new();
                let mut order: Vec<(&'static str, String)> = Vec::new();
                for e in batch {
                    let k = (e.kind, e.key.clone());
                    if !latest_by_key.contains_key(&k) {
                        order.push(k.clone());
                    }
                    latest_by_key.insert(k, e);
                }
                for k in order {
                    if let Some(e) = latest_by_key.get(&k) {
                        if yielder.send(to_sse(e)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        },
    );
    Sse::new(live).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// Small helper turning a producer future into a Stream via an mpsc channel.
fn async_stream<F, Fut>(producer: F) -> impl Stream<Item = Result<Event, Infallible>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<Event>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(256);
    tokio::spawn(producer(tx));
    tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok)
}

use tokio_stream::StreamExt;
