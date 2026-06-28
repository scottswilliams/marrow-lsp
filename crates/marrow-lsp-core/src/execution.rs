//! Shared execution DTO adapters for transports that expose Marrow run state.

use marrow_run::{ProjectSession, SurfaceServeBoundary};
use serde_json::{Value as Json, json};

pub fn execution_boundary_json(session: &ProjectSession) -> Json {
    let boundary = marrow_json::execution_boundary_to_json(session);
    serde_json::to_value(boundary).expect("Marrow execution boundary DTO serializes")
}

pub fn surface_serve_boundary_json(boundary: &SurfaceServeBoundary) -> Json {
    let boundary = marrow_json::surface_serve_boundary_to_json(boundary);
    serde_json::to_value(boundary).expect("Marrow surface serve boundary DTO serializes")
}

pub fn debug_data_boundary_json(execution_boundary: &Json, store_snapshot: Json) -> Json {
    json!({
        "sourceAnalysisGeneration": execution_boundary["sourceAnalysisGeneration"].clone(),
        "storeSnapshot": store_snapshot,
    })
}
