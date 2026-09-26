// The conversation as a graph: steps or stages, their transitions, and the
// live (or replayed) flow state on top. Drag from a node's right handle to
// another node to connect them; select and press Delete to remove.

import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Background,
  Controls,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  useReactFlow,
  type Connection,
  type Edge,
  type Node,
  type NodeProps,
} from '@xyflow/react';
import dagre from '@dagrejs/dagre';
import { useStudio } from '../store';
import { addNode, connect, disconnect, graphOf, modeOf, removeNode, type GraphEdge, type GraphNode, type Spec } from '../spec/graph';
import type { FlowStatus } from '../api';

const NODE_WIDTH = 240;
const NODE_HEIGHT = 110;

type NodeData = Record<string, unknown> & GraphNode & { state: 'active' | 'done' | 'idle' | 'waiting'; blocked: string[] };
type StudioNode = Node<NodeData, 'studio'>;

function layout(nodes: GraphNode[], edges: GraphEdge[]): Record<string, { x: number; y: number }> {
  const graph = new dagre.graphlib.Graph();
  graph.setGraph({ rankdir: 'LR', nodesep: 40, ranksep: 90, marginx: 20, marginy: 20 });
  graph.setDefaultEdgeLabel(() => ({}));
  for (const node of nodes) graph.setNode(node.id, { width: NODE_WIDTH, height: NODE_HEIGHT });
  for (const edge of edges) if (graph.hasNode(edge.source) && graph.hasNode(edge.target)) graph.setEdge(edge.source, edge.target);
  dagre.layout(graph);
  const out: Record<string, { x: number; y: number }> = {};
  for (const node of nodes) {
    const placed = graph.node(node.id);
    if (placed) out[node.id] = { x: placed.x - NODE_WIDTH / 2, y: placed.y - NODE_HEIGHT / 2 };
  }
  return out;
}

function stateOf(id: string, status: FlowStatus | null): NodeData['state'] {
  if (!status) return 'idle';
  if (status.active?.includes(id)) return status.active_progress?.[id]?.holds === false ? 'waiting' : 'active';
  if (status.done?.includes(id)) return 'done';
  return 'idle';
}

const StudioNodeView = memo(function StudioNodeView({ data, selected }: NodeProps<StudioNode>) {
  return (
    <div className={`node node-${data.kind} state-${data.state} ${data.terminal ? 'terminal' : ''} ${selected ? 'selected' : ''}`}>
      <Handle type="target" position={Position.Left} />
      <div className="node-title">
        <span className="node-id">{data.id || '(no id)'}</span>
        {data.overlay && <span className="badge">overlay · {data.overlay}</span>}
        {data.terminal && <span className="badge">end</span>}
      </div>
      {data.say && <div className="node-say">{data.say}</div>}
      <div className="node-chips">
        {data.collect.map((slot) => (
          <span key={`c-${slot}`} className="chip chip-slot" title="collects">
            {slot}
          </span>
        ))}
        {data.allow.map((tool) => (
          <span key={`a-${tool}`} className={`chip chip-tool ${data.blocked.includes(tool) ? 'blocked' : ''}`} title="allowed tool">
            {tool}
          </span>
        ))}
        {data.commit && (
          <span className="chip chip-commit" title="confirm before">
            ✓ {data.commit}
          </span>
        )}
      </div>
      {data.done && <div className="node-done">done when {data.done}</div>}
      {!data.terminal && <Handle type="source" position={Position.Right} />}
    </div>
  );
});

const nodeTypes = { studio: StudioNodeView };

function positionsKey(spec: Spec): string {
  return `gemini-adk-studio:positions:${typeof spec.name === 'string' ? spec.name : ''}:${modeOf(spec)}`;
}

export function Canvas() {
  const spec = useStudio((s) => s.spec);
  const edit = useStudio((s) => s.edit);
  const select = useStudio((s) => s.select);
  const selection = useStudio((s) => s.selection);
  const status = useStudio((s) => s.status);
  const layoutEpoch = useStudio((s) => s.layoutEpoch);
  const graph = useMemo(() => graphOf(spec), [spec]);
  const [positions, setPositions] = useState<Record<string, { x: number; y: number }>>(() => {
    try {
      return JSON.parse(localStorage.getItem(positionsKey(spec)) ?? '{}') as Record<string, { x: number; y: number }>;
    } catch {
      return {};
    }
  });
  const lastEpoch = useRef(layoutEpoch);
  const { fitView } = useReactFlow();
  const fitPending = useRef(true);

  // Lay out on request, and place nodes that have no position yet.
  useEffect(() => {
    const relayout = lastEpoch.current !== layoutEpoch;
    lastEpoch.current = layoutEpoch;
    const missing = graph.nodes.some((n) => !positions[n.id]);
    if (!relayout && !missing) return;
    const computed = layout(graph.nodes, graph.edges);
    setPositions((current) => (relayout ? computed : { ...computed, ...current }));
    if (relayout) fitPending.current = true;
  }, [graph, layoutEpoch]); // eslint-disable-line react-hooks/exhaustive-deps

  // Frame the graph once nodes have their positions (on open and relayout).
  useEffect(() => {
    if (!fitPending.current || graph.nodes.some((n) => !positions[n.id])) return;
    fitPending.current = false;
    const frame = requestAnimationFrame(() => void fitView({ padding: 0.15, maxZoom: 1.1, duration: 200 }));
    return () => cancelAnimationFrame(frame);
  }, [positions, graph, fitView]);

  useEffect(() => {
    try {
      localStorage.setItem(positionsKey(spec), JSON.stringify(positions));
    } catch {
      // View state only.
    }
  }, [positions, spec]);

  const blocked = Object.keys(status?.blocked_tools ?? {});
  const nodes: StudioNode[] = graph.nodes.map((node) => ({
    id: node.id,
    type: 'studio',
    position: positions[node.id] ?? { x: 0, y: 0 },
    data: { ...node, state: stateOf(node.id, status), blocked },
    selected: selection?.kind === 'node' && selection.id === node.id,
  }));
  const edges: Edge[] = graph.edges.map((edge) => ({
    id: edge.id,
    source: edge.source,
    target: edge.target,
    label: edge.label,
    className: `edge-${edge.kind}`,
    animated: edge.kind === 'next' && status?.active?.includes(edge.source) === true,
    markerEnd: { type: MarkerType.ArrowClosed },
    selected: selection?.kind === 'edge' && selection.id === edge.id,
  }));

  const onConnect = useCallback(
    (connection: Connection) => {
      if (connection.source && connection.target) edit(connect(spec, connection.source, connection.target));
    },
    [spec, edit],
  );

  const onDelete = useCallback(
    ({ nodes: gone, edges: cut }: { nodes: Node[]; edges: Edge[] }) => {
      let next = spec;
      for (const edge of cut) {
        const graphEdge = graph.edges.find((e) => e.id === edge.id);
        if (graphEdge) next = disconnect(next, graphEdge);
      }
      for (const node of gone) next = removeNode(next, node.id);
      if (next !== spec) {
        edit(next);
        select(null);
      }
    },
    [spec, graph, edit, select],
  );

  return (
    <div className="canvas">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        onNodesChange={(changes) => {
          setPositions((current) => {
            let next = current;
            for (const change of changes) {
              if (change.type === 'position' && change.position) next = { ...next, [change.id]: change.position };
            }
            return next;
          });
        }}
        onConnect={onConnect}
        onDelete={onDelete}
        onNodeClick={(_, node) => select({ kind: 'node', id: node.id })}
        onEdgeClick={(_, edge) => select({ kind: 'edge', id: edge.id })}
        onPaneClick={() => select(null)}
        deleteKeyCode={['Delete', 'Backspace']}
        fitView
        minZoom={0.2}
        proOptions={{ hideAttribution: true }}
      >
        <Background gap={24} />
        <Controls showInteractive={false} />
      </ReactFlow>
      <div className="canvas-actions">
        <button
          type="button"
          onClick={() => {
            const { spec: next, id } = addNode(spec);
            edit(next);
            select({ kind: 'node', id });
          }}
        >
          + {modeOf(spec) === 'flow' ? 'Step' : 'Stage'}
        </button>
        <button type="button" onClick={() => useStudio.getState().relayout()}>
          Auto layout
        </button>
      </div>
      {graph.nodes.length === 0 && <div className="canvas-empty">No {modeOf(spec) === 'flow' ? 'steps' : 'stages'} yet. Add one to start.</div>}
    </div>
  );
}
