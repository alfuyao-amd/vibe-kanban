import { createFileRoute, Link, useParams } from '@tanstack/react-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { proceduresApi } from '@/shared/lib/api';
import type { ProcedureRun, StateHistoryEntry } from 'shared/types';

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

function outcomeColor(outcome: string | null | undefined): string {
  if (outcome === 'success') return 'text-emerald-600 dark:text-emerald-400';
  if (outcome === 'failure') return 'text-rose-600 dark:text-rose-400';
  if (outcome === 'cancelled') return 'text-zinc-500';
  return 'text-low';
}

function formatTime(value: string | Date | null | undefined): string {
  if (!value) return '—';
  const d = typeof value === 'string' ? new Date(value) : value;
  return d.toLocaleString();
}

function HistoryRow({
  entry,
  index,
}: {
  entry: StateHistoryEntry;
  index: number;
}) {
  return (
    <li className="relative pl-6 pb-4">
      <span className="absolute left-0 top-1 inline-block h-3 w-3 rounded-full bg-zinc-300 dark:bg-zinc-700" />
      <span className="absolute left-[5px] top-4 bottom-0 w-px bg-zinc-200 dark:bg-zinc-800" />
      <div className="text-sm">
        <span className="font-mono">{index + 1}.</span>{' '}
        <span className="font-medium">{entry.state}</span>
        {entry.attempt > 1 && (
          <span className="text-xs text-low ml-1">
            (attempt {entry.attempt})
          </span>
        )}
        {' → '}
        <span className={outcomeColor(entry.outcome)}>
          {entry.outcome ?? 'pending'}
        </span>
      </div>
      {entry.gate_summary && (
        <div className="text-xs font-mono text-low mt-0.5">
          {entry.gate_summary}
        </div>
      )}
      <div className="text-xs text-low mt-0.5">
        {formatTime(entry.entered_at)}
      </div>
    </li>
  );
}

function ProcedureRunDetail() {
  const { runId } = useParams({ from: '/_app/procedure-runs_/$runId' });
  const queryClient = useQueryClient();

  const { data, isLoading, error } = useQuery<ProcedureRun>({
    queryKey: ['procedure-run', runId],
    queryFn: () => proceduresApi.getRun(runId),
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      if (status === 'running' || status === 'awaiting_approval') return 2000;
      return false;
    },
  });

  const approveMutation = useMutation({
    mutationFn: () => proceduresApi.approve(runId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['procedure-run', runId] });
    },
  });

  const rejectMutation = useMutation({
    mutationFn: () => proceduresApi.reject(runId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['procedure-run', runId] });
    },
  });

  const cancelMutation = useMutation({
    mutationFn: () => proceduresApi.cancel(runId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['procedure-run', runId] });
    },
  });

  if (isLoading) {
    return <div className="p-6 text-sm text-low">Loading run…</div>;
  }
  if (error || !data) {
    return (
      <div className="p-6 text-sm text-rose-500">
        Failed to load procedure run: {(error as Error)?.message ?? 'not found'}
      </div>
    );
  }

  const run = data;
  const isAwaiting = run.status === 'awaiting_approval';
  const isLive = run.status === 'running' || run.status === 'awaiting_approval';

  return (
    <div className="flex flex-col p-6 gap-6 max-w-4xl">
      <header className="flex flex-col gap-2">
        <Link
          to="/procedure-runs"
          className="text-xs text-blue-600 dark:text-blue-400 hover:underline"
        >
          ← all procedure runs
        </Link>
        <div className="flex items-baseline gap-3">
          <h1 className="text-xl font-semibold">{run.procedure_name}</h1>
          <span className="text-xs text-low">
            v{String(run.procedure_version)}
          </span>
          <span
            className={`rounded px-2 py-0.5 text-xs font-medium ${statusBadgeClass(run.status)}`}
          >
            {run.status}
          </span>
        </div>
        <div className="text-xs text-low font-mono">{run.id}</div>
        <div className="text-xs text-low">
          current state: <span className="font-mono">{run.current_state}</span>
        </div>
      </header>

      {isAwaiting && (
        <section className="rounded border border-amber-300 bg-amber-50 dark:bg-amber-950/30 p-4">
          <div className="text-sm font-medium text-amber-800 dark:text-amber-300 mb-2">
            Awaiting approval
          </div>
          {run.pending_approval_prompt && (
            <div className="text-sm mb-3">{run.pending_approval_prompt}</div>
          )}
          <div className="flex gap-2">
            <button
              onClick={() => approveMutation.mutate()}
              disabled={approveMutation.isPending}
              className="rounded bg-emerald-600 hover:bg-emerald-700 disabled:opacity-50 text-white text-sm px-3 py-1.5"
            >
              {approveMutation.isPending ? 'Approving…' : 'Approve'}
            </button>
            <button
              onClick={() => rejectMutation.mutate()}
              disabled={rejectMutation.isPending}
              className="rounded bg-rose-600 hover:bg-rose-700 disabled:opacity-50 text-white text-sm px-3 py-1.5"
            >
              {rejectMutation.isPending ? 'Rejecting…' : 'Reject'}
            </button>
          </div>
          {(approveMutation.error || rejectMutation.error) && (
            <div className="text-xs text-rose-500 mt-2">
              {(approveMutation.error || rejectMutation.error)?.message}
            </div>
          )}
        </section>
      )}

      <section>
        <h2 className="text-sm font-medium mb-3">State history</h2>
        {run.state_history.length === 0 ? (
          <div className="text-sm text-low">No transitions recorded yet.</div>
        ) : (
          <ol className="text-sm">
            {run.state_history.map((entry, i) => (
              <HistoryRow
                key={`${entry.state}-${entry.attempt}-${i}`}
                entry={entry}
                index={i}
              />
            ))}
          </ol>
        )}
      </section>

      <section>
        <h2 className="text-sm font-medium mb-2">Params</h2>
        <pre className="text-xs bg-zinc-100 dark:bg-zinc-900 rounded p-3 overflow-x-auto">
          {JSON.stringify(run.params, null, 2)}
        </pre>
      </section>

      {isLive && (
        <section>
          <button
            onClick={() => cancelMutation.mutate()}
            disabled={cancelMutation.isPending}
            className="rounded border border-zinc-300 dark:border-zinc-700 hover:bg-zinc-100 dark:hover:bg-zinc-900 disabled:opacity-50 text-sm px-3 py-1.5"
          >
            {cancelMutation.isPending ? 'Cancelling…' : 'Cancel run'}
          </button>
          {cancelMutation.error && (
            <div className="text-xs text-rose-500 mt-2">
              {cancelMutation.error.message}
            </div>
          )}
        </section>
      )}
    </div>
  );
}

export const Route = createFileRoute('/_app/procedure-runs_/$runId')({
  component: ProcedureRunDetail,
});
