// The live session socket, shared so that edits made while a session runs
// (a step's posture or grounding) steer its very next turn.

let socket: WebSocket | null = null;

export function setLiveSocket(ws: WebSocket | null) {
  socket = ws;
}

export function liveConnected(): boolean {
  return socket?.readyState === WebSocket.OPEN;
}

/** Send posture/grounding edits for steps to the running session. */
export function sendPostures(postures: Record<string, string>, grounds: Record<string, string>) {
  if (!liveConnected()) return;
  socket!.send(JSON.stringify({ type: 'updateFlowPostures', postures, grounds }));
}
