import { useEffect, useMemo, useState } from 'react';
import {
  createFileRoute,
  Link,
  useNavigate,
  useParams,
  useSearch,
} from '@tanstack/react-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { z } from 'zod';
import { proceduresApi } from '@/shared/lib/api';
import { ProcedureGraph } from '@web/shared/ProcedureGraph';
import type {
  ProcedureGraphView,
  ProcedureSourceView,
  ProcedureSummary,
} from 'shared/types';

const NEW_PROCEDURE_TEMPLATE = `name: my_procedure
version: 1
description: |
  One-paragraph description of what this procedure does.

triggers:
  match_hints:
    - "do the thing"
  params:
    goal:
      type: string
      required: true
      description: What the procedure should accomplish.

initial_state: plan

states:
  plan:
    action:
      kind: create_session
      executor: CLAUDE_CODE
      prompt: |
        Plan: {{goal}}
    on_success: done
    on_failure: failed

  done:
    terminal: success

  failed:
    terminal: failure
`;

function sourceBadgeClass(source: string): string {
  switch (source) {
    case 'builtin':
      return 'bg-zinc-200 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-300';
    case 'lead_agent':
      return 'bg-violet-500/15 text-violet-600 dark:text-violet-400';
    case 'user':
    default:
      return 'bg-blue-500/15 text-blue-600 dark:text-blue-400';
  }
}

function ProcedureEditorPage() {
  const { projectId } = useParams({
    from: '/_app/projects/$projectId_/procedures',
  });
  const search = useSearch({
    from: '/_app/projects/$projectId_/procedures',
  });
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const selectedName = search.name ?? null;
  const isNew = search.mode === 'new';

  const proceduresQuery = useQuery<ProcedureSummary[]>({
    queryKey: ['procedures', projectId],
    queryFn: () => proceduresApi.listForProject(projectId),
  });

  const sourceQuery = useQuery<ProcedureSourceView | null>({
    queryKey: ['procedure-source', projectId, selectedName],
    queryFn: async () =>
      selectedName
        ? proceduresApi.getProcedureSource(projectId, selectedName)
        : null,
    enabled: !!selectedName && !isNew,
  });

  const [yamlDraft, setYamlDraft] = useState<string>('');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  // Default to graph view because the user's complaint was specifically that
  // raw YAML was a poor primary presentation. YAML stays one click away.
  const [view, setView] = useState<'graph' | 'yaml'>('graph');

  const graphQuery = useQuery<ProcedureGraphView | null>({
    queryKey: ['procedure-graph', projectId, selectedName],
    queryFn: async () =>
      selectedName
        ? proceduresApi.getProcedureGraph(projectId, selectedName)
        : null,
    // The graph endpoint re-parses YAML server-side, so it only makes sense
    // for already-saved procedures. New (unsaved) drafts show the YAML view
    // until they're saved.
    enabled: !!selectedName && !isNew,
  });

  // Reset draft when the selection changes.
  useEffect(() => {
    setErrorMsg(null);
    if (isNew) {
      setYamlDraft(NEW_PROCEDURE_TEMPLATE);
      return;
    }
    if (sourceQuery.data) {
      setYamlDraft(sourceQuery.data.yaml);
    }
  }, [isNew, sourceQuery.data]);

  const isReadOnly = !isNew && (sourceQuery.data?.read_only ?? false);

  const saveMutation = useMutation({
    mutationFn: () => proceduresApi.upsertProcedure(projectId, yamlDraft),
    onSuccess: () => {
      setErrorMsg(null);
      queryClient.invalidateQueries({ queryKey: ['procedures', projectId] });
      queryClient.invalidateQueries({
        queryKey: ['procedure-source', projectId],
      });
      queryClient.invalidateQueries({
        queryKey: ['procedure-graph', projectId],
      });
    },
    onError: (err: unknown) => {
      setErrorMsg(err instanceof Error ? err.message : String(err));
    },
  });

  const deleteMutation = useMutation({
    mutationFn: () => {
      if (!selectedName) throw new Error('No procedure selected');
      return proceduresApi.deleteProcedure(projectId, selectedName);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['procedures', projectId] });
      navigate({
        to: '/projects/$projectId/procedures',
        params: { projectId },
        search: {},
      });
    },
    onError: (err: unknown) => {
      setErrorMsg(err instanceof Error ? err.message : String(err));
    },
  });

  const procedures = useMemo(
    () => proceduresQuery.data ?? [],
    [proceduresQuery.data]
  );

  return (
    <div className="flex flex-col p-6 gap-4 max-w-6xl">
      <header>
        <Link
          to="/projects/$projectId/lead-agent"
          params={{ projectId }}
          className="text-xs text-blue-600 dark:text-blue-400 hover:underline"
        >
          ← back to project lead agent
        </Link>
        <h1 className="text-xl font-semibold mt-2">Procedures</h1>
        <p className="text-sm text-low mt-1">
          Built-ins are read-only — open one and click "Fork to project-local"
          to start from its YAML. Project-local procedures can be edited or
          deleted here.
        </p>
      </header>

      <div className="grid gap-4 md:grid-cols-[280px_1fr]">
        <aside className="rounded border p-3 max-h-[70vh] overflow-y-auto">
          <button
            onClick={() =>
              navigate({
                to: '/projects/$projectId/procedures',
                params: { projectId },
                search: { mode: 'new' },
              })
            }
            className="w-full rounded bg-blue-600 hover:bg-blue-700 text-white text-sm px-3 py-1.5 mb-3"
          >
            + New procedure
          </button>
          {proceduresQuery.isLoading ? (
            <div className="text-xs text-low">Loading…</div>
          ) : (
            <ul className="text-sm flex flex-col gap-1">
              {procedures.map((p) => {
                const isSelected = !isNew && selectedName === p.name;
                return (
                  <li key={p.name}>
                    <Link
                      to="/projects/$projectId/procedures"
                      params={{ projectId }}
                      search={{ name: p.name }}
                      className={`block rounded px-2 py-1 hover:bg-zinc-100 dark:hover:bg-zinc-900 ${
                        isSelected
                          ? 'bg-zinc-100 dark:bg-zinc-900 font-medium'
                          : ''
                      }`}
                    >
                      <div className="flex items-center gap-2">
                        <span className="truncate">{p.name}</span>
                        <span
                          className={`ml-auto rounded px-1.5 py-0.5 text-[10px] ${sourceBadgeClass(p.source)}`}
                        >
                          {p.source}
                        </span>
                      </div>
                      {p.description && (
                        <div className="text-[11px] text-low mt-0.5 line-clamp-2">
                          {p.description}
                        </div>
                      )}
                    </Link>
                  </li>
                );
              })}
            </ul>
          )}
        </aside>

        <main className="flex flex-col gap-3">
          {!isNew && !selectedName && (
            <div className="rounded border border-dashed p-6 text-sm text-low">
              Pick a procedure on the left to view or edit, or click "New
              procedure" to author one from scratch.
            </div>
          )}

          {(isNew || selectedName) && (
            <>
              <div className="flex items-baseline gap-3">
                <h2 className="text-lg font-medium">
                  {isNew ? 'New procedure' : (selectedName ?? '')}
                </h2>
                {!isNew && sourceQuery.data && (
                  <span
                    className={`rounded px-2 py-0.5 text-xs ${sourceBadgeClass(sourceQuery.data.source)}`}
                  >
                    {sourceQuery.data.source}
                  </span>
                )}
                {isReadOnly && (
                  <span className="text-xs text-low">read-only</span>
                )}
              </div>

              {!isNew && sourceQuery.isLoading && (
                <div className="text-xs text-low">Loading source…</div>
              )}

              {!isNew && (
                <div className="inline-flex rounded border text-xs overflow-hidden self-start">
                  {(['graph', 'yaml'] as const).map((opt) => (
                    <button
                      key={opt}
                      onClick={() => setView(opt)}
                      className={`px-3 py-1 ${
                        view === opt
                          ? 'bg-zinc-200 dark:bg-zinc-800 font-medium'
                          : 'hover:bg-zinc-100 dark:hover:bg-zinc-900'
                      }`}
                    >
                      {opt === 'graph' ? 'Graph' : 'YAML'}
                    </button>
                  ))}
                </div>
              )}

              {/* New (unsaved) procedures don't have a server-rendered graph
                  yet, so always show YAML. Saved procedures default to graph
                  unless the user toggled. */}
              {!isNew && view === 'graph' ? (
                graphQuery.isLoading ? (
                  <div className="text-xs text-low">Loading graph…</div>
                ) : graphQuery.error ? (
                  <div className="rounded border border-rose-400 bg-rose-50 dark:bg-rose-950/30 text-rose-700 dark:text-rose-300 text-xs p-2">
                    Graph unavailable: {(graphQuery.error as Error).message}.
                    Switch to YAML to view the source.
                  </div>
                ) : graphQuery.data ? (
                  <ProcedureGraph graph={graphQuery.data} />
                ) : null
              ) : (
                <textarea
                  value={yamlDraft}
                  onChange={(e) => setYamlDraft(e.target.value)}
                  spellCheck={false}
                  readOnly={isReadOnly}
                  rows={28}
                  className="font-mono text-xs rounded border p-3 bg-zinc-50 dark:bg-zinc-950 focus:outline-none focus:ring-1 focus:ring-blue-500"
                />
              )}

              {errorMsg && (
                <div className="rounded border border-rose-400 bg-rose-50 dark:bg-rose-950/30 text-rose-700 dark:text-rose-300 text-xs p-2 whitespace-pre-wrap">
                  {errorMsg}
                </div>
              )}

              <div className="flex gap-2">
                {!isReadOnly && (
                  <button
                    onClick={() => saveMutation.mutate()}
                    disabled={saveMutation.isPending || !yamlDraft.trim()}
                    className="rounded bg-emerald-600 hover:bg-emerald-700 disabled:opacity-50 text-white text-sm px-3 py-1.5"
                  >
                    {saveMutation.isPending ? 'Saving…' : 'Save'}
                  </button>
                )}
                {isReadOnly && (
                  <button
                    onClick={() => {
                      // Switch into "new procedure" mode pre-populated with
                      // the built-in body. The user is expected to rename
                      // before saving (server rejects built-in name reuse).
                      navigate({
                        to: '/projects/$projectId/procedures',
                        params: { projectId },
                        search: { mode: 'new' },
                      });
                      setYamlDraft(yamlDraft);
                    }}
                    className="rounded bg-blue-600 hover:bg-blue-700 text-white text-sm px-3 py-1.5"
                  >
                    Fork to project-local
                  </button>
                )}
                {!isNew && !isReadOnly && (
                  <button
                    onClick={() => {
                      if (
                        confirm(
                          `Delete project-local procedure "${selectedName}"?`
                        )
                      ) {
                        deleteMutation.mutate();
                      }
                    }}
                    disabled={deleteMutation.isPending}
                    className="rounded border border-rose-400 hover:bg-rose-50 dark:hover:bg-rose-950/30 text-rose-700 dark:text-rose-400 text-sm px-3 py-1.5"
                  >
                    Delete
                  </button>
                )}
              </div>
            </>
          )}
        </main>
      </div>
    </div>
  );
}

const searchSchema = z.object({
  name: z.string().optional(),
  mode: z.literal('new').optional(),
});

export const Route = createFileRoute('/_app/projects/$projectId_/procedures')({
  component: ProcedureEditorPage,
  validateSearch: searchSchema,
});
