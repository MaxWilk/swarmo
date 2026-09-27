/**
 * The one place the frontend names a status.
 *
 * Mirrors `swarmo_core::model::grpc_code_name` and `StatusCount::describe`.
 * Kept here rather than in each view so a code cannot be named two different
 * things on two different screens.
 */
import type { Protocol } from "./types";

/** Canonical name for a gRPC status code. */
export function grpcCodeName(code: number): string {
  return (
    [
      "OK",
      "CANCELLED",
      "UNKNOWN",
      "INVALID_ARGUMENT",
      "DEADLINE_EXCEEDED",
      "NOT_FOUND",
      "ALREADY_EXISTS",
      "PERMISSION_DENIED",
      "RESOURCE_EXHAUSTED",
      "FAILED_PRECONDITION",
      "ABORTED",
      "OUT_OF_RANGE",
      "UNIMPLEMENTED",
      "INTERNAL",
      "UNAVAILABLE",
      "DATA_LOSS",
      "UNAUTHENTICATED",
    ][code] ?? "UNKNOWN"
  );
}

/** A label and a pill class for one status. */
export interface StatusLabel {
  label: string;
  cls: "ok" | "warn" | "err";
}

/**
 * Name a status the way its own protocol does.
 *
 * The protocol has to come from the record rather than the number: the two
 * schemes collide at zero, where gRPC means OK and HTTP means the request
 * never got a response at all.
 */
export function describeStatus(sc: { protocol: Protocol; code: number }): StatusLabel {
  if (sc.protocol === "ws") {
    // A socket has no status codes; the sample records 101 for a completed
    // handshake, 0 for an answered message, otherwise the close code.
    if (sc.code === 101) return { label: "WS connected", cls: "ok" };
    if (sc.code === 0) return { label: "WS message", cls: "ok" };
    return { label: `WS closed ${sc.code}`, cls: sc.code === 1000 ? "ok" : "err" };
  }
  if (sc.protocol === "grpc") {
    return {
      label: `gRPC ${sc.code} ${grpcCodeName(sc.code)}`,
      cls: sc.code === 0 ? "ok" : "err",
    };
  }
  if (sc.code === 0) {
    return { label: "HTTP (no response)", cls: "err" };
  }
  return {
    label: `HTTP ${sc.code}`,
    cls: sc.code < 300 ? "ok" : sc.code < 400 ? "warn" : "err",
  };
}
