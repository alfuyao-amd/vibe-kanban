import { useEffect, useMemo, useState } from 'react';
import {
  createFileRoute,
  Link,
  useNavigate,
  useParams,
} from '@tanstack/react-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { leadAgentApi, proceduresApi, workspacesApi } from '@/shared/lib/api';
import { WorkspaceProvider } from '@/shared/providers/WorkspaceProvider';
import { Workspaces as WorkspacesUi } from '@/pages/workspaces/Workspaces';
import type {
  LeadAgentSession,
  ProcedureRun,
  ProcedureSummary,
  Workspace,
} from 'shared/types';

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

  // When a lead-agent session is bound, render the workspace chat UI
  // pinned to that session — embedded inline at /projects/:id/lead-agent so
  // the user doesn't have to navigate to the workspace to talk to the agent.
  // Workers/runs/procedures still have their own dedicated routes; this page
  // is now the chat surface, with a slim header that surfaces those links.
  if (session) {
    return (
      <div className="flex flex-col h-full min-h-0">
        <header className="flex items-center gap-3 px-4 py-2 border-b bg-blue-50/50 dark:bg-blue-950/20">
          <span className="text-sm font-medium">Lead agent</span>
          <span className="rounded px-1.5 py-0.5 text-[10px] bg-blue-500/15 text-blue-700 dark:text-blue-300">
            persistent
          </span>
          <span className="text-xs text-low font-mono truncate">
            {session.session_id.slice(0, 8)}…
          </span>
          <div className="ml-auto flex items-center gap-2 text-xs">
            <Link
              to="/projects/$projectId/procedures"
              params={{ projectId }}
              className="rounded border px-2 py-1 hover:bg-zinc-100 dark:hover:bg-zinc-900"
            >
              Procedures
            </Link>
            <Link
              to="/procedure-runs"
              search={{ projectId }}
              className="rounded border px-2 py-1 hover:bg-zinc-100 dark:hover:bg-zinc-900"
            >
              Runs ({recentRuns.length})
            </Link>
          </div>
        </header>
        <div className="flex-1 min-h-0 overflow-hidden">
          {/* Inner WorkspaceProvider overrides the outer (app-root) one with
              this project's lead-agent workspace + session, so the embedded
              chat lands directly on the lead session. */}
          <WorkspaceProvider
            workspaceIdOverride={session.workspace_id}
            initialSessionIdOverride={session.session_id}
          >
            <WorkspacesUi />
          </WorkspaceProvider>
        </div>
      </div>
    );
  }

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

      <section className="rounded border p-4">
        <div className="text-sm font-medium mb-1">No agent yet</div>
        <p className="text-xs text-low mb-3">
          Pick a workspace where the agent will live. The agent runs as a normal
          VK Claude session in that workspace, so it'll show up in the
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
