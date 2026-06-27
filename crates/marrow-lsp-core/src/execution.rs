//! Shared execution DTO adapters for transports that expose Marrow run state.

use marrow_run::ProjectSession;
use serde_json::Value as Json;

pub fn execution_boundary_json(session: &ProjectSession) -> Json {
    let boundary = marrow_json::execution_boundary_to_json(session);
    serde_json::to_value(boundary).expect("Marrow execution boundary DTO serializes")
}
