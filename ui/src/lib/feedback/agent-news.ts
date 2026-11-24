import type { AgentOperationSummary } from "@ytmcp/protocol";

/** What the shell reports by itself: a change to the queue or to what is playing. */
export function settledAgentChange(operation: AgentOperationSummary): operation is AgentOperationSummary & {
  resource: "queue" | "interruption";
} {
  return (operation.resource === "queue" || operation.resource === "interruption") &&
    (operation.phase === "verified" || operation.phase === "partial");
}

/** Operation IDs are unique within a principal, so both fields identify a journal entry. */
export function operationKey(principal: string, operationId: string): string {
  return JSON.stringify([principal, operationId]);
}

/** An agent asked the listener to sign in, renew or switch accounts and is waiting for it. */
export function pendingAccountHandoff(operation: AgentOperationSummary): boolean {
  return operation.resource === "accountHandoff" && operation.phase === "acknowledged";
}
