import { useMemo } from 'react';
import {
  Background,
  Controls,
  MarkerType,
  ReactFlow,
  type Edge,
  type Node,
} from '@xyflow/react';
import dagre from 'dagre';
import type {
  ProcedureGraphEdge,
  ProcedureGraphNode,
  ProcedureGraphView,
} from 'shared/types';

import '@xyflow/react/dist/style.css';

const NODE_WIDTH = 200;
const NODE_HEIGHT = 80;

function nodeStyle(
  node: ProcedureGraphNode,
  isInitial: boolean,
  isCurrent: boolean
): React.CSSProperties {
  // Tailwind-ish hex values picked to read on both light + dark backgrounds.
  let bg = '#f4f4f5';
  let border = '#a1a1aa';
  let color = '#111';
  switch (node.kind) {
    case 'terminal_success':
      bg = '#d1fae5';
      border = '#10b981';
      break;
    case 'terminal_failure':
      bg = '#fee2e2';
      border = '#ef4444';
      break;
    default:
      if (isInitial) {
        bg = '#dbeafe';
        border = '#3b82f6';
      }
  }
  if (isCurrent) {
    border = '#f59e0b';
    color = '#92400e';
  }
  return {
    background: bg,
    border: `2px solid ${border}`,
    borderRadius: 6,
    padding: 8,
    width: NODE_WIDTH,
    color,
    fontSize: 12,
    fontFamily:
      'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
  };
}

function nodeLabel(node: ProcedureGraphNode): React.ReactNode {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
      <strong>{node.id}</strong>
      <div style={{ display: 'flex', gap: 4, flexWrap: 'wrap' }}>
        {node.action_kind && (
          <span
            style={{
              fontSize: 10,
              background: 'rgba(59,130,246,0.15)',
              color: '#1e40af',
              padding: '1px 4px',
              borderRadius: 3,
            }}
          >
            {node.action_kind}
          </span>
        )}
        {node.gate_kind && (
          <span
            style={{
              fontSize: 10,
              background: 'rgba(168,85,247,0.15)',
              color: '#6b21a8',
              padding: '1px 4px',
              borderRadius: 3,
            }}
          >
            gate: {node.gate_kind}
          </span>
        )}
        {node.kind === 'terminal_success' && (
          <span style={{ fontSize: 10 }}>✓ success</span>
        )}
        {node.kind === 'terminal_failure' && (
          <span style={{ fontSize: 10 }}>✗ failure</span>
        )}
      </div>
    </div>
  );
}

function edgeStyle(edge: ProcedureGraphEdge): {
  stroke: string;
  label: string;
} {
  if (edge.kind === 'success') {
    return { stroke: '#10b981', label: 'on_success' };
  }
  return { stroke: '#ef4444', label: 'on_failure' };
}

/// Build {nodes, edges} laid out by dagre so we don't ship a hand-rolled
/// graph layout. Top-to-bottom because procedures read like a script — the
/// initial state should be at the top.
function layout(
  graph: ProcedureGraphView,
  currentState: string | null
): { nodes: Node[]; edges: Edge[] } {
  const g = new dagre.graphlib.Graph();
  g.setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: 'TB', nodesep: 30, ranksep: 50 });

  for (const node of graph.nodes) {
    g.setNode(node.id, { width: NODE_WIDTH, height: NODE_HEIGHT });
  }
  for (const edge of graph.edges) {
    g.setEdge(edge.from, edge.to);
  }
  dagre.layout(g);

  const rfNodes: Node[] = graph.nodes.map((n) => {
    const pos = g.node(n.id);
    return {
      id: n.id,
      data: { label: nodeLabel(n) },
      // dagre returns center-positioned coords; React Flow expects top-left.
      position: { x: pos.x - NODE_WIDTH / 2, y: pos.y - NODE_HEIGHT / 2 },
      style: nodeStyle(n, n.id === graph.initial_state, n.id === currentState),
      draggable: true,
    };
  });

  const rfEdges: Edge[] = graph.edges.map((e, i) => {
    const styling = edgeStyle(e);
    return {
      id: `e${i}-${e.from}-${e.to}-${e.kind}`,
      source: e.from,
      target: e.to,
      label: styling.label,
      labelStyle: { fontSize: 10, fill: styling.stroke },
      style: { stroke: styling.stroke, strokeWidth: 2 },
      markerEnd: { type: MarkerType.ArrowClosed, color: styling.stroke },
    };
  });

  return { nodes: rfNodes, edges: rfEdges };
}

interface ProcedureGraphProps {
  graph: ProcedureGraphView;
  /// Optional state to highlight (e.g. the run's current_state on the
  /// run-detail page). Passed from outside so this component stays unaware
  /// of run state.
  currentState?: string | null;
  height?: number;
}

export function ProcedureGraph({
  graph,
  currentState = null,
  height = 480,
}: ProcedureGraphProps) {
  const { nodes, edges } = useMemo(
    () => layout(graph, currentState),
    [graph, currentState]
  );

  return (
    <div
      style={{ height, width: '100%' }}
      className="rounded border bg-white dark:bg-zinc-900"
    >
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodesDraggable
        fitView
        proOptions={{ hideAttribution: true }}
      >
        <Background />
        <Controls />
      </ReactFlow>
    </div>
  );
}
