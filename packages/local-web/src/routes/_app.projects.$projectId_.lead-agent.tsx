import { useEffect, useMemo, useState } from 'react';
import {
  createFileRoute,
  Link,
  useNavigate,
  useParams,
} from '@tanstack/react-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { leadAgentApi, proceduresApi, workspacesApi } from '@/shared/lib/api';
import type {
  LeadAgentSession,
  ProcedureRun,
  ProcedureSummary,
  Workspace,
} from 'shared/types';

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

/// Derive the unique session ids (in order of first appearance) referenced
/// by a run's state history. These are the run's "worker sessions" —
/// distinct from the project's persistent lead-agent session.
function workerSessionIdsForRun(run: ProcedureRun): string[] {
  const seen = new Set<string>();
  const ordered: string[] = [];
  for (const entry of run.state_history ?? []) {
    const sid = (entry as { session_id?: string | null }).session_id;
    if (sid && !seen.has(sid)) {
      seen.add(sid);
      ordered.push(sid);
    }
  }
  return ordered;
}

function LeadAgentPage() {
  const { projectId } = useParams({
    from: '/_app/projects/$projectId_/lead-agent',
  });
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const sessionQuery = useQuery<LeadAgentSession | null>({
    queryKey: ['lead-agent-session', projectId],
    queryFn: () => leadAgentApi.getSession(projectId),
  });
  const workspacesQuery = useQuery<Workspace[]>({
    queryKey: ['workspaces-all'],
    queryFn: () => workspacesApi.getAllWorkspaces(),
  });
  const proceduresQuery = useQuery<ProcedureSummary[]>({
    queryKey: ['procedures', projectId],
    queryFn: () => proceduresApi.listForProject(projectId),
  });
  const runsQuery = useQuery<ProcedureRun[]>({
    queryKey: ['procedure-runs', projectId],
    queryFn: () => proceduresApi.listRunsForProject(projectId),
    refetchInterval: 5000,
  });

  const recentRuns = useMemo(
    () => (runsQuery.data ?? []).slice(0, 8),
    [runsQuery.data]
  );

  const [workspaceId, setWorkspaceId] = useState<string>('');

  // Default to most recent workspace once they load.
  useEffect(() => {
    if (
      !workspaceId &&
      workspacesQuery.data &&
      workspacesQuery.data.length > 0
    ) {
      setWorkspaceId(workspacesQuery.data[0].id);
    }
  }, [workspaceId, workspacesQuery.data]);

  const startMutation = useMutation({
    mutationFn: () =>
      leadAgentApi.startSession(projectId, { workspace_id: workspaceId }),
    onSuccess: (session) => {
      queryClient.invalidateQueries({
        queryKey: ['lead-agent-session', projectId],
      });
      navigate({
        to: '/workspaces/$workspaceId',
        params: { workspaceId: session.workspace_id },
      });
    },
  });

  if (sessionQuery.isLoading) {
    return <div className="p-6 text-sm text-low">Loading lead agent…</div>;
  }

  const session = sessionQuery.data ?? null;

  return (
    <div className="flex flex-col p-6 gap-6 max-w-4xl">
      <header>
        <h1 className="text-xl font-semibold">Project lead agent</h1>
        <p className="text-sm text-low mt-1">
          A persistent Claude session that manages this project's procedures and
          runs. It auto-saves any procedures it authors and briefs you in plain
          English — you don't need to read YAML.
        </p>
      </header>

      {session ? (
        <section className="rounded border-2 border-blue-300 dark:border-blue-700 bg-blue-50/50 dark:bg-blue-950/20 p-4">
          <div className="flex items-baseline gap-2 mb-1">
            <div className="text-sm font-medium">Lead agent</div>
            <span className="rounded px-1.5 py-0.5 text-[10px] bg-blue-500/15 text-blue-700 dark:text-blue-300">
              persistent
            </span>
          </div>
          <p className="text-xs text-low mb-3">
            Your conversational entry point. Talk to it to author or run
            procedures; it'll spawn worker sessions (below) for each run.
          </p>
          <div className="text-xs text-low font-mono mb-3">
            session: {session.session_id.slice(0, 8)}…
          </div>
          <div className="flex gap-2">
            <Link
              to="/workspaces/$workspaceId"
              params={{ workspaceId: session.workspace_id }}
              search={{ session: session.session_id }}
              className="rounded bg-blue-600 hover:bg-blue-700 text-white text-sm px-3 py-1.5"
            >
              Chat with lead agent →
            </Link>
            <Link
              to="/procedure-runs"
              search={{ projectId }}
              className="rounded border text-sm px-3 py-1.5 hover:bg-zinc-100 dark:hover:bg-zinc-900"
            >
              See runs
            </Link>
          </div>
        </section>
      ) : (
        <section className="rounded border p-4">
          <div className="text-sm font-medium mb-1">No agent yet</div>
          <p className="text-xs text-low mb-3">
            Pick a workspace where the agent will live. The agent runs as a
            normal VK Claude session in that workspace, so it'll show up in the
            workspace's session list. You only need to do this once per project.
          </p>
          <label className="flex flex-col gap-1 text-xs mb-3">
            <span className="text-low">Workspace</span>
            <select
              value={workspaceId}
              onChange={(e) => setWorkspaceId(e.target.value)}
              className="rounded border bg-transparent px-2 py-1.5 text-sm"
            >
              <option value="">— pick a workspace —</option>
              {(workspacesQuery.data ?? []).map((w) => (
                <option key={w.id} value={w.id}>
                  {w.name ?? w.branch} ({w.id.slice(0, 8)})
                </option>
              ))}
            </select>
          </label>
          <button
            onClick={() => startMutation.mutate()}
            disabled={!workspaceId || startMutation.isPending}
            className="rounded bg-blue-600 hover:bg-blue-700 disabled:opacity-50 text-white text-sm px-4 py-1.5"
          >
            {startMutation.isPending ? 'Starting…' : 'Start lead agent'}
          </button>
          {startMutation.error && (
            <div className="text-xs text-rose-500 mt-2">
              {(startMutation.error as Error).message}
            </div>
          )}
        </section>
      )}

      {session && (
        <section className="rounded border-2 border-violet-300 dark:border-violet-800 bg-violet-50/40 dark:bg-violet-950/15 p-4">
          <div className="flex items-baseline gap-2 mb-1">
            <h2 className="text-sm font-medium">Procedure workers</h2>
            <span className="rounded px-1.5 py-0.5 text-[10px] bg-violet-500/15 text-violet-700 dark:text-violet-300">
              ephemeral
            </span>
          </div>
          <p className="text-xs text-low mb-3">
            Sessions the runtime spawns inside this workspace for each procedure
            run (plan / implement / review / etc.). They live alongside the
            lead-agent chat in the workspace's session list, but follow the run,
            not you. Click into a worker to see what it did.
          </p>
          {runsQuery.isLoading ? (
            <div className="text-xs text-low">Loading…</div>
          ) : recentRuns.length === 0 ? (
            <div className="text-xs text-low">
              No runs yet. Ask the lead agent to start one.
            </div>
          ) : (
            <ul className="flex flex-col gap-2">
              {recentRuns.map((run) => {
                const workerSessionIds = workerSessionIdsForRun(run);
                return (
                  <li
                    key={run.id}
                    className="rounded border bg-white dark:bg-zinc-950 p-2 text-xs"
                  >
                    <div className="flex items-center gap-2 mb-1">
                      <Link
                        to="/procedure-runs/$runId"
                        params={{ runId: run.id }}
                        className="font-medium hover:underline"
                      >
                        {run.procedure_name}
                      </Link>
                      <span
                        className={`rounded px-1.5 py-0.5 text-[10px] ${statusBadgeClass(run.status)}`}
                      >
                        {run.status}
                      </span>
                      <span className="text-low">
                        state{' '}
                        <span className="font-mono">{run.current_state}</span>
                      </span>
                      <span className="text-low ml-auto font-mono">
                        {run.id.slice(0, 8)}
                      </span>
                    </div>
                    {workerSessionIds.length > 0 && (
                      <div className="flex flex-wrap gap-1">
                        {workerSessionIds.map((sid) => (
                          <Link
                            key={sid}
                            to="/workspaces/$workspaceId"
                            params={{
                              workspaceId:
                                run.workspace_id ?? session.workspace_id,
                            }}
                            search={{ session: sid }}
                            className="rounded border bg-zinc-50 dark:bg-zinc-900 hover:bg-violet-100 dark:hover:bg-violet-900/30 px-1.5 py-0.5 font-mono text-[10px]"
                          >
                            worker {sid.slice(0, 8)}
                          </Link>
                        ))}
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      )}

      <section>
        <div className="flex items-center justify-between mb-2">
          <h2 className="text-sm font-medium">
            Procedures available to this project
          </h2>
          <Link
            to="/projects/$projectId/procedures"
            params={{ projectId }}
            className="text-xs text-blue-600 dark:text-blue-400 hover:underline"
          >
            Edit procedures →
          </Link>
        </div>
        {proceduresQuery.isLoading ? (
          <div className="text-sm text-low">Loading…</div>
        ) : (
          <ul className="text-sm">
            {(proceduresQuery.data ?? []).map((p) => (
              <li key={p.name} className="border-t py-2 first:border-t-0">
                <span className="font-medium">{p.name}</span>
                <span className="ml-2 text-xs text-low">v{p.version}</span>
                {p.description && (
                  <div className="text-xs text-low mt-0.5">{p.description}</div>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

export const Route = createFileRoute('/_app/projects/$projectId_/lead-agent')({
  component: LeadAgentPage,
});
