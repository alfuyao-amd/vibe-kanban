import { ReactNode, useMemo, useCallback, useEffect, useRef } from 'react';
import { useParams, useSearch } from '@tanstack/react-router';
import { useQueryClient } from '@tanstack/react-query';
import { useWorkspaces } from '@/shared/hooks/useWorkspaces';
import { workspaceSummaryKeys } from '@/shared/hooks/workspaceSummaryKeys';
import { useWorkspaceRecord } from '@/shared/hooks/useWorkspaceRecord';
import { useWorkspaceRepo } from '@/shared/hooks/useWorkspaceRepo';
import { useWorkspaceSessions } from '@/shared/hooks/useWorkspaceSessions';
import { useGitHubComments } from '@/shared/hooks/useGitHubComments';
import { useDiffStream } from '@/shared/hooks/useDiffStream';
import { workspacesApi } from '@/shared/lib/api';
import { useWorkspaceDiffStore } from '@/shared/stores/useWorkspaceDiffStore';
import type { DiffStats } from 'shared/types';
import { useAppNavigation } from '@/shared/hooks/useAppNavigation';
import { useCurrentAppDestination } from '@/shared/hooks/useCurrentAppDestination';

import { WorkspaceContext } from '@/shared/hooks/useWorkspaceContext';

interface WorkspaceProviderProps {
  children: ReactNode;
  /**
   * Mount the provider against this workspace_id instead of the URL's
   * `workspaceId` route param. Used by routes that need to host the
   * workspace UI without owning the workspace URL — e.g. the lead-agent
   * page embeds the chat for a project's bound workspace at
   * `/projects/:id/lead-agent`. A nested `WorkspaceProvider` with this prop
   * overrides the outer (app-root) one for its subtree, which is how the
   * lead-agent route shows the lead session inline without redirecting.
   */
  workspaceIdOverride?: string | null;
  /**
   * Pre-select this session id at mount, ignoring the `?session=` URL
   * search param. The lead-agent route uses this to pin the lead session;
   * regular workspace routes leave it undefined and fall back to URL.
   */
  initialSessionIdOverride?: string | null;
}

export function WorkspaceProvider({
  children,
  workspaceIdOverride,
  initialSessionIdOverride,
}: WorkspaceProviderProps) {
  const { workspaceId: routeWorkspaceId } = useParams({ strict: false });
  const workspaceId = workspaceIdOverride ?? routeWorkspaceId;
  // Pulled non-strictly because WorkspaceProvider mounts at the app root and
  // most routes don't declare a `session` search param. The unknown-shape
  // record is fine here — we just look up the optional string.
  const search = useSearch({ strict: false }) as Record<string, unknown>;
  const initialSessionId =
    initialSessionIdOverride !== undefined
      ? initialSessionIdOverride
      : typeof search?.session === 'string' && search.session.length > 0
        ? search.session
        : null;
  const appNavigation = useAppNavigation();
  const currentDestination = useCurrentAppDestination();
  const queryClient = useQueryClient();

  const isCreateMode = currentDestination?.kind === 'workspaces-create';

  const {
    workspaces: activeWorkspaces,
    archivedWorkspaces,
    isLoading: isLoadingList,
  } = useWorkspaces();

  const { data: workspace, isLoading: isLoadingWorkspace } = useWorkspaceRecord(
    workspaceId,
    { enabled: !!workspaceId && !isCreateMode }
  );

  const {
    sessions,
    selectedSession,
    selectedSessionId,
    selectSession,
    selectLatestSession,
    isLoading: isSessionsLoading,
    isNewSessionMode,
    startNewSession,
  } = useWorkspaceSessions(workspaceId, {
    enabled: !isCreateMode,
    initialSessionId,
  });

  const { repos, isLoading: isReposLoading } = useWorkspaceRepo(workspaceId, {
    enabled: !isCreateMode,
  });

  // TODO: Support multiple repos - currently only fetches comments from the primary repo.
  const primaryRepoId = repos[0]?.id;

  const currentWorkspaceSummary = activeWorkspaces.find(
    (w) => w.id === workspaceId
  );
  const hasPrAttached = !!currentWorkspaceSummary?.prStatus;

  const {
    gitHubComments,
    isGitHubCommentsLoading,
    showGitHubComments,
    setShowGitHubComments,
    getGitHubCommentsForFile,
    getGitHubCommentCountForFile,
    getFilesWithGitHubComments,
    getFirstCommentLineForFile,
  } = useGitHubComments({
    workspaceId,
    repoId: primaryRepoId,
    enabled: !isCreateMode && hasPrAttached,
  });

  const { diffs } = useDiffStream(workspaceId ?? null, !isCreateMode);

  const diffPaths = useMemo(
    () =>
      new Set(diffs.map((d) => d.newPath || d.oldPath || '').filter(Boolean)),
    [diffs]
  );

  const diffStats: DiffStats = useMemo(
    () => ({
      files_changed: diffs.length,
      lines_added: diffs.reduce((sum, d) => sum + (d.additions ?? 0), 0),
      lines_removed: diffs.reduce((sum, d) => sum + (d.deletions ?? 0), 0),
    }),
    [diffs]
  );

  const rafRef = useRef<number | null>(null);
  const batchCountRef = useRef(0);

  const latestDiffDataRef = useRef({
    diffs,
    diffPaths,
    diffStats,
    gitHubComments,
    isGitHubCommentsLoading,
    showGitHubComments,
    setShowGitHubComments,
    getGitHubCommentsForFile,
    getGitHubCommentCountForFile,
    getFilesWithGitHubComments,
    getFirstCommentLineForFile,
  });
  latestDiffDataRef.current = {
    diffs,
    diffPaths,
    diffStats,
    gitHubComments,
    isGitHubCommentsLoading,
    showGitHubComments,
    setShowGitHubComments,
    getGitHubCommentsForFile,
    getGitHubCommentCountForFile,
    getFilesWithGitHubComments,
    getFirstCommentLineForFile,
  };

  useEffect(() => {
    batchCountRef.current++;
    if (rafRef.current === null) {
      rafRef.current = requestAnimationFrame(() => {
        rafRef.current = null;
        batchCountRef.current = 0;
        useWorkspaceDiffStore
          .getState()
          .setWorkspaceDiffData(latestDiffDataRef.current);
      });
    }
    return () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, [
    diffs,
    diffPaths,
    diffStats,
    gitHubComments,
    isGitHubCommentsLoading,
    showGitHubComments,
    setShowGitHubComments,
    getGitHubCommentsForFile,
    getGitHubCommentCountForFile,
    getFilesWithGitHubComments,
    getFirstCommentLineForFile,
  ]);

  useEffect(() => {
    return () => {
      useWorkspaceDiffStore.getState().clearWorkspaceDiffData();
    };
  }, []);

  const isLoading = isLoadingList || isLoadingWorkspace;

  useEffect(() => {
    if (!workspaceId || isCreateMode) return;

    workspacesApi
      .markSeen(workspaceId)
      .then(() => {
        queryClient.invalidateQueries({ queryKey: workspaceSummaryKeys.all });
      })
      .catch((error) => {
        console.warn('Failed to mark workspace as seen:', error);
      });
  }, [workspaceId, isCreateMode, queryClient]);

  const selectWorkspace = useCallback(
    (id: string) => {
      appNavigation.goToWorkspace(id);
    },
    [appNavigation]
  );

  const navigateToCreate = useMemo(
    () => () => {
      appNavigation.goToWorkspacesCreate();
    },
    [appNavigation]
  );

  const coreValue = useMemo(
    () => ({
      workspaceId,
      workspace,
      activeWorkspaces,
      archivedWorkspaces,
      isWorkspacesListLoading: isLoadingList,
      isLoading,
      isCreateMode,
      selectWorkspace,
      navigateToCreate,
      sessions,
      selectedSession,
      selectedSessionId,
      selectSession,
      selectLatestSession,
      isSessionsLoading,
      isNewSessionMode,
      startNewSession,
      repos,
      isReposLoading,
    }),
    [
      workspaceId,
      workspace,
      activeWorkspaces,
      archivedWorkspaces,
      isLoadingList,
      isLoading,
      isCreateMode,
      selectWorkspace,
      navigateToCreate,
      sessions,
      selectedSession,
      selectedSessionId,
      selectSession,
      selectLatestSession,
      isSessionsLoading,
      isNewSessionMode,
      startNewSession,
      repos,
      isReposLoading,
    ]
  );

  return (
    <WorkspaceContext.Provider value={coreValue}>
      {children}
    </WorkspaceContext.Provider>
  );
}
