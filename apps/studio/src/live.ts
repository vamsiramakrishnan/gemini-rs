// Only a continuous sequence of supported edits may patch a running session.
// Loading a document or changing its structure breaks that revision chain.
let connection: { socket: WebSocket; revision: number } | null = null;

export function setLiveSocket(value: typeof connection) {
  connection = value;
}

export function liveConnected(): boolean {
  return connection?.socket.readyState === WebSocket.OPEN;
}

export function sendPostures({ postures, grounds, previousRevision, revision }: {
  postures: Record<string, string>;
  grounds: Record<string, string>;
  previousRevision: number;
  revision: number;
}) {
  if (!connection || connection.socket.readyState !== WebSocket.OPEN || connection.revision !== previousRevision) return;
  connection.socket.send(JSON.stringify({ type: 'updateFlowPostures', postures, grounds }));
  connection.revision = revision;
}
