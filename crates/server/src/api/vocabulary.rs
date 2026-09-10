//! `GET /api/vocabulary`: the event and state names a rule may use.
//!
//! Static for the life of the process, so the UI fetches it once and offers the
//! same vocabulary the server validates rule specs against.

use axum::Json;
use cereyan_core::{EventName, StateName, StateType};
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
pub struct EventEntry {
    /// The name a rule matches on, e.g. `run.failed`.
    pub name: String,
    /// The resource kind the event hangs off.
    pub resource: String,
    /// One sentence on when the engine records it.
    pub when: String,
    /// The payload keys the emit site sets.
    pub payload_fields: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct StateEntry {
    pub name: String,
    /// For a sub-state, the type it belongs to; for a type, itself.
    pub state_type: String,
    pub is_sub_state: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Vocabulary {
    /// Every engine-emitted event, in catalogue order.
    pub events: Vec<EventEntry>,
    /// Prefixes the engine owns. A name under one of these must be an entry in
    /// `events`; any other name is a custom event and is never checked.
    pub reserved_prefixes: Vec<String>,
    /// Every value a rule's `states` accepts: the types then the sub-states. A
    /// rule naming a type also matches that type's sub-states.
    pub states: Vec<StateEntry>,
}

#[utoipa::path(get, path = "/api/vocabulary", responses((status = 200, body = Vocabulary)))]
pub async fn get_vocabulary() -> Json<Vocabulary> {
    Json(Vocabulary {
        events: EventName::ALL
            .iter()
            .map(|e| EventEntry {
                name: e.as_str().into(),
                resource: e.resource().into(),
                when: e.when().into(),
                payload_fields: e.payload_fields().iter().map(|f| (*f).into()).collect(),
            })
            .collect(),
        reserved_prefixes: cereyan_core::RESERVED_PREFIXES
            .iter()
            .map(|p| (*p).into())
            .collect(),
        states: StateType::ALL
            .iter()
            .map(|t| StateEntry {
                name: t.as_str().into(),
                state_type: t.as_str().into(),
                is_sub_state: false,
            })
            .chain(StateName::ALL.iter().map(|n| StateEntry {
                name: n.as_str().into(),
                state_type: n.state_type().as_str().into(),
                is_sub_state: true,
            }))
            .collect(),
    })
}
