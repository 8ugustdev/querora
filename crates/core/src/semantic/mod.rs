// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! The semantic layer: heuristic suggestion, FTS5 retrieval, agent
//! enrichment, review/publish flow. The PUBLISHED graph is the only thing
//! the validator/compiler accept (enforced in storage + validate).

pub mod eav;
pub mod enrich;
pub mod retrieval;
pub mod suggest;

pub use eav::{
    build_extension, detect, fetch_value_hints, merge as merge_eav, EavExtension, EavInfo,
};
pub use enrich::{enrich, EnrichResult};
pub use retrieval::{index_graph, search, SearchHit};
pub use suggest::{infer_data_type, suggest, Suggestion};
