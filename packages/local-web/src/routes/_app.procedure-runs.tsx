import { useEffect, useMemo, useRef, useState } from 'react';
import {
  createFileRoute,
  Link,
  useNavigate,
} from '@tanstack/react-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { z } from 'zod';
import {
  leadAgentApi,
  proceduresApi,
  projectsApi,
  workspacesApi,
} from '@/shared/lib/api';
import type {
  PickedPlan,
  ProcedureRun,
  ProcedureSummary,
  Project,
  Workspace,
} from 'shared/types';

const searchSchema = z.object({
  projectId: z.string().uuid().optional(),
});

function statusBadgeClass(status: string): string {
  switch (status) {
    case 'running':
      return 'bg-blue-500/15 text-blue-600 dark:text-blue-400';
    case 'awaiting_approval':
      return 'bg-amber-500/15 text-amber-600 dark:text-amber-400';
    case 'succeeded':
      return 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400';
    case 'failed':
      return 'bg-rose-500/15 text-rose-600 dark:text-rose-400';
    case 'cancelled':
      return 'bg-zinc-500/15 text-zinc-600 dark:text-zinc-400';
    default:
      return 'bg-zinc-500/15 text-zinc-600 dark:text-zinc-400';
  }
}

function formatTime(value: string | Date): string {
  const d = typeof value === 'string' ? new Date(value) : value;
  return d.toLocaleString();
}

function StartProcedureForm({ defaultProjectId }: { defaultProjectId?: string }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();

  const [projectId, setProjectId] = useState<string>(defaultProjectId ?? '');
  const [procedureName, setProcedureName] = useState<string>('');
  const [workspaceId, setWorkspaceId] = useState<string>('');
  const [paramValues, setParamValues] = useState<Record<string, string>>({});
  const [goal, setGoal] = useState<string>('');
  const [planNote, setPlanNote] = useState<string | null>(null);

  const projectsQuery = useQuery<Project[]>({
    queryKey: ['projects'],
    queryFn: () => projectsApi.list(),
  });
  const proceduresQuery = useQuery<ProcedureSummary[]>({
    queryKey: ['procedures', projectId || 'global'],
    queryFn: () =>
      projectId
        ? proceduresApi.listForProject(projectId)
        : proceduresApi.listDefinitions(),
    enabled: true,
  });
  const workspacesQuery = useQuery<Workspace[]>({
    queryKey: ['workspaces-all'],
    queryFn: () => workspacesApi.getAllWorkspaces(),
  });

  // Auto-select sensible defaults once data loads.
  useEffect(() => {
    if (!projectId && projectsQuery.data && projectsQuery.data.length > 0) {
      setProjectId(projectsQuery.data[0].id);
    }
  }, [projectId, projectsQuery.data]);
  useEffect(() => {
    if (!procedureName && proceduresQuery.data && proceduresQuery.data.length > 0) {
      setProcedureName(proceduresQuery.data[0].name);
    }
  }, [procedureName, proceduresQuery.data]);
  useEffect(() => {
    if (!workspaceId && workspacesQuery.data && workspacesQuery.data.length > 0) {
      setWorkspaceId(workspacesQuery.data[0].id);
    }
  }, [workspaceId, workspacesQuery.data]);

  const procedure = useMemo(
    () => proceduresQuery.data?.find((p) => p.name === procedureName),
    [proceduresQuery.data, procedureName]
  );

  // Reset param values when the user manually picks a different procedure;
  // skipped when the planner sets both at once via applyPickedPlan.
  const skipNextResetRef = useRef(false);
  useEffect(() => {
    if (skipNextResetRef.current) {
      skipNextResetRef.current = false;
      return;
    }
    setParamValues({});
  }, [procedureName]);

  const applyPickedPlan = (plan: PickedPlan) => {
    skipNextResetRef.current = true;
    setProcedureName(plan.procedure_name);
    const next: Record<string, string> = {};
    if (plan.params && typeof plan.params === 'object') {
      for (const [k, v] of Object.entries(plan.params as Record<string, unknown>)) {
        if (v == null) continue;
        next[k] = typeof v === 'string' ? v : JSON.stringify(v);
      }
    }
    setParamValues(next);
  };

  const planMutation = useMutation({
    mutationFn: async () => {
      if (!goal.trim()) throw new Error('goal is required');
      if (!projectId) throw new Error('project is required for planning');
      if (!workspaceId) throw new Error('workspace is required for planning');
      return leadAgentApi.plan(projectId, {
        goal: goal.trim(),
        workspace_id: workspaceId,
      });
    },
    onSuccess: (plan) => {
      applyPickedPlan(plan);
      setPlanNote(`Picked ${plan.procedure_name}. Review params, then Start run.`);
    },
  });

  const startMutation = useMutation({
    mutationFn: async () => {
      if (!procedure) throw new Error('procedure is required');
      if (!projectId) throw new Error('project is required');
      const params: Record<string, unknown> = {};
      for (const p of procedure.params) {
        const raw = paramValues[p.name] ?? '';
        if (raw === '' && !p.required) continue;
        if (raw === '' && p.required) {
          throw new Error(`param "${p.name}" is required`);
        }
        params[p.name] = coerceParam(raw, p.type);
      }
      return proceduresApi.startRun(projectId, {
        procedure_name: procedure.name,
        params,
        workspace_id: workspaceId || null,
      });
    },
    onSuccess: (run) => {
      queryClient.invalidateQueries({ queryKey: ['procedure-runs'] });
      navigate({ to: '/procedure-runs/$runId', params: { runId: run.id } });
    },
  });

  if (projectsQuery.isLoading || proceduresQuery.isLoading || workspacesQuery.isLoading) {
    return (
      <div className="rounded border p-4 text-sm text-low">Loading form…</div>
    );
  }

  const projects = projectsQuery.data ?? [];
  const procedures = proceduresQuery.data ?? [];
  const workspaces = workspacesQuery.data ?? [];

  return (
    <div className="rounded border p-4">
      <h2 className="text-sm font-medium mb-3">Start a procedure run</h2>

      <div className="rounded border border-dashed p-3 mb-4">
        <label className="flex flex-col gap-1 text-xs">
          <span className="text-low">
            Plan from a goal{' '}
            <span className="text-low">(asks Claude to pick a procedure + params; needs a workspace)</span>
          </span>
          <textarea
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
            placeholder="e.g. add a wave(name) function with a test"
            rows={2}
            className="rounded border bg-transparent px-2 py-1.5 text-sm"
          />
        </label>
        <div className="flex items-center gap-3 mt-2">
          <button
            onClick={() => planMutation.mutate()}
            disabled={!goal.trim() || !workspaceId || planMutation.isPending}
            title={
              !goal.trim()
                ? 'Enter a goal'
                : !workspaceId
                  ? 'Pick a workspace below'
                  : ''
            }
            className="rounded border border-blue-600 text-blue-600 hover:bg-blue-600/10 disabled:opacity-50 text-sm px-3 py-1.5"
          >
            {planMutation.isPending ? 'Planning…' : 'Plan'}
          </button>
          {!planNote && !planMutation.error && (!goal.trim() || !workspaceId) && (
            <span className="text-xs text-low">
              {!goal.trim()
                ? 'Enter a goal'
                : 'Pick a workspace below first'}
            </span>
          )}
          {planNote && !planMutation.error && (
            <span className="text-xs text-emerald-600 dark:text-emerald-400">{planNote}</span>
          )}
          {planMutation.error && (
            <span className="text-xs text-rose-500">
              {(planMutation.error as Error).message}
            </span>
          )}
        </div>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-3 mb-3">
        <label className="flex flex-col gap-1 text-xs">
          <span className="text-low">Project</span>
          <select
            value={projectId}
            onChange={(e) => setProjectId(e.target.value)}
            className="rounded border bg-transparent px-2 py-1.5 text-sm"
          >
            <option value="">— pick a project —</option>
            {projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </label>
        <label className="flex flex-col gap-1 text-xs">
          <span className="text-low">Procedure</span>
          <select
            value={procedureName}
            onChange={(e) => setProcedureName(e.target.value)}
            className="rounded border bg-transparent px-2 py-1.5 text-sm"
          >
            <option value="">— pick a procedure —</option>
            {procedures.map((p) => (
              <option key={p.name} value={p.name}>
                {p.name} (v{p.version})
              </option>
            ))}
          </select>
        </label>
        <label className="flex flex-col gap-1 text-xs">
          <span className="text-low">
            Workspace <span className="text-low">(optional)</span>
          </span>
          <select
            value={workspaceId}
            onChange={(e) => setWorkspaceId(e.target.value)}
            className="rounded border bg-transparent px-2 py-1.5 text-sm"
          >
            <option value="">— none —</option>
            {workspaces.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name ?? w.branch} ({w.id.slice(0, 8)})
              </option>
            ))}
          </select>
        </label>
      </div>

      {procedure && procedure.description && (
        <p className="text-xs text-low mb-3 whitespace-pre-line">
          {procedure.description}
        </p>
      )}

      {procedure && procedure.params.length > 0 && (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3 mb-3">
          {procedure.params.map((p) => (
            <label key={p.name} className="flex flex-col gap-1 text-xs">
              <span className="text-low">
                {p.name}
                <span className="ml-1 font-mono text-low">({p.type})</span>
                {p.required && <span className="text-rose-500"> *</span>}
              </span>
              <input
                type="text"
                value={paramValues[p.name] ?? ''}
                onChange={(e) =>
                  setParamValues((prev) => ({ ...prev, [p.name]: e.target.value }))
                }
                placeholder={p.description ?? ''}
                className="rounded border bg-transparent px-2 py-1.5 text-sm"
              />
              {p.description && (
                <span className="text-low">{p.description}</span>
              )}
            </label>
          ))}
        </div>
      )}

      <div className="flex items-center gap-3">
        <button
          onClick={() => startMutation.mutate()}
          disabled={!projectId || !procedure || startMutation.isPending}
          className="rounded bg-blue-600 hover:bg-blue-700 disabled:opacity-50 text-white text-sm px-4 py-1.5"
        >
          {startMutation.isPending ? 'Starting…' : 'Start run'}
        </button>
        {startMutation.error && (
          <span className="text-xs text-rose-500">
            {(startMutation.error as Error).message}
          </span>
        )}
      </div>
    </div>
  );
}

function coerceParam(raw: string, ty: string): unknown {
  switch (ty) {
    case 'integer': {
      const n = parseInt(raw, 10);
      return Number.isNaN(n) ? raw : n;
    }
    case 'boolean':
      return raw.toLowerCase() === 'true' || raw === '1';
    case 'array':
      try {
        return JSON.parse(raw);
      } catch {
        return raw.split(',').map((s) => s.trim());
      }
    case 'string':
    default:
      return raw;
  }
}

function ProcedureRunsList() {
  const { projectId } = Route.useSearch();

  const { data, isLoading, error } = useQuery<ProcedureRun[]>({
    queryKey: ['procedure-runs', projectId ?? 'all'],
    queryFn: () =>
      projectId
        ? proceduresApi.listRunsForProject(projectId)
        : proceduresApi.listAllRuns(),
    refetchInterval: 3000,
  });

  if (isLoading) {
    return <div className="p-6 text-sm text-low">Loading procedure runs…</div>;
  }
  if (error) {
    return (
      <div className="p-6 text-sm text-rose-500">
        Failed to load procedure runs: {(error as Error).message}
      </div>
    );
  }

  const runs = data ?? [];

  return (
    <div className="flex flex-col p-6 gap-4">
      <header>
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-semibold">Procedure runs</h1>
          {projectId && (
            <Link
              to="/projects/$projectId/lead-agent"
              params={{ projectId }}
              className="rounded border border-blue-600 text-blue-600 hover:bg-blue-600/10 text-xs px-3 py-1.5"
            >
              Open lead agent →
            </Link>
          )}
        </div>
        <p className="text-sm text-low mt-1">
          Lead Agent procedure executions
          {projectId ? (
            <>
              {' '}
              (project <span className="font-mono">{projectId.slice(0, 8)}</span>{' '}
              <Link
                to="/procedure-runs"
                search={{}}
                className="text-blue-600 dark:text-blue-400 hover:underline"
              >
                clear filter
              </Link>
              )
            </>
          ) : (
            ' across all projects'
          )}
          . Polls every 3s.
        </p>
      </header>

      <StartProcedureForm defaultProjectId={projectId} />

      {runs.length === 0 ? (
        <div className="rounded border border-dashed p-8 text-center text-sm text-low">
          No procedure runs yet.
        </div>
      ) : (
        <div className="overflow-x-auto rounded border">
          <table className="w-full text-sm">
            <thead className="bg-zinc-50 dark:bg-zinc-900 text-left">
              <tr>
                <th className="px-3 py-2 font-medium">Procedure</th>
                <th className="px-3 py-2 font-medium">Status</th>
                <th className="px-3 py-2 font-medium">State</th>
                <th className="px-3 py-2 font-medium">Started</th>
                <th className="px-3 py-2 font-medium">Updated</th>
              </tr>
            </thead>
            <tbody>
              {runs.map((run) => (
                <tr key={run.id} className="border-t hover:bg-zinc-50/50 dark:hover:bg-zinc-900/50">
                  <td className="px-3 py-2">
                    <Link
                      to="/procedure-runs/$runId"
                      params={{ runId: run.id }}
                      className="text-blue-600 dark:text-blue-400 hover:underline"
                    >
                      {run.procedure_name}
                    </Link>
                    <span className="ml-2 text-xs text-low">v{String(run.procedure_version)}</span>
                  </td>
                  <td className="px-3 py-2">
                    <span
                      className={`inline-block rounded px-2 py-0.5 text-xs font-medium ${statusBadgeClass(run.status)}`}
                    >
                      {run.status}
                    </span>
                  </td>
                  <td className="px-3 py-2 font-mono text-xs">{run.current_state}</td>
                  <td className="px-3 py-2 text-xs text-low">{formatTime(run.created_at)}</td>
                  <td className="px-3 py-2 text-xs text-low">{formatTime(run.updated_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

export const Route = createFileRoute('/_app/procedure-runs')({
  validateSearch: searchSchema,
  component: ProcedureRunsList,
});
