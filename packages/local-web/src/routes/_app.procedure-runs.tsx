import { createFileRoute, Link } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { proceduresApi } from '@/shared/lib/api';
import type { ProcedureRun } from 'shared/types';

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

function ProcedureRunsList() {
  const { data, isLoading, error } = useQuery<ProcedureRun[]>({
    queryKey: ['procedure-runs'],
    queryFn: () => proceduresApi.listAllRuns(),
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
        <h1 className="text-xl font-semibold">Procedure runs</h1>
        <p className="text-sm text-low mt-1">
          Lead Agent procedure executions. Polls every 3s.
        </p>
      </header>

      {runs.length === 0 ? (
        <div className="rounded border border-dashed p-8 text-center text-sm text-low">
          No procedure runs yet. Start one via the API:
          <pre className="mt-3 inline-block text-xs text-left bg-zinc-100 dark:bg-zinc-900 rounded px-3 py-2">
            POST /api/projects/{'{project_id}'}/procedure-runs
          </pre>
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
  component: ProcedureRunsList,
});
