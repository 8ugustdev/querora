/* SPDX-License-Identifier: Apache-2.0 */
/**
 * Generated contract types — single source of truth from `crates/contracts`
 * (ts-rs). Regenerate with `cargo test -p querora-contracts`.
 */
export type { AnalyticalQuery } from "./types/AnalyticalQuery";
export type { MeasureRef } from "./types/MeasureRef";
export type { DimensionRef } from "./types/DimensionRef";
export type { Filter } from "./types/Filter";
export type { TimeSpec } from "./types/TimeSpec";
export type { SemanticGraph } from "./types/SemanticGraph";
export type { QueryResult } from "./types/QueryResult";
export type { AgentResult } from "./types/AgentResult";
export type { ResultStats } from "./types/ResultStats";
export type { VisualizationSpec } from "./types/VisualizationSpec";
export type { ChartType } from "./types/ChartType";
export type { SourceInfo } from "./types/SourceInfo";
export type { SourceKind } from "./types/SourceKind";
export type { SourceId } from "./types/SourceId";
export type { AgentEvent } from "./types/AgentEvent";
export type { AgentStatus } from "./types/AgentStatus";
export type { ToolError } from "./types/ToolError";
export type { ErrorCode } from "./types/ErrorCode";

/** Shape of the `querora_status` IPC command result. */
export interface CoreStatus {
  socket_path: string;
  semantic_version: string | null;
  source: string | null;
}
